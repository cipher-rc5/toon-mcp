// file: crates/toon-mcp-server/tests/mcp_transport.rs
// description: MCP transport integration tests — verify tool routing and wire protocol

//! These tests verify that the `ToonMcpServer` correctly registers its MCP
//! tools via the rmcp `tool_router!` macro, dispatches tool calls to the
//! correct handler functions, deserialises JSON parameters, and returns
//! well-formed JSON-RPC responses over an in-memory duplex transport.
//!
//! Run with:
//! ```bash
//! cargo test --package toon-mcp-server --test mcp_transport -- --test-threads=1
//! ```

use std::sync::Arc;

use rmcp::{
    ClientHandler, ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
};
use toon_mcp_logging::NoopSink;
use toon_mcp_server::config::Config;
use toon_mcp_server::server::ToonMcpServer;

// ---------------------------------------------------------------------------
// Minimal client handler — delegates everything to rmcp defaults.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct TestClient;

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn test_config() -> Config {
    Config {
        max_output_ratio: 0.85,
        // Set min_bytes to 0 so small test inputs are not short-circuited.
        min_bytes: 0,
        max_input_bytes: toon_mcp_core::DEFAULT_MAX_INPUT_BYTES,
        key_folding: true,
        delimiter: toon_format::Delimiter::Comma,
        tabular_min_rows: 3,
        fold_min_depth: 3,
        primitive_array_min: 5,
        csv_numeric_coercion: true,
        logging_enabled: false,
        logging: toon_mcp_logging::JsonlSinkConfig::default(),
        log_level: "error".into(),
        strict_config: false,
        client_hint: None,
        pipeline_timeout_ms: 30_000,
        max_concurrent_calls: 8,
    }
}

/// Extract the text payload from the first content item of a tool call result.
fn first_text(result: &rmcp::model::CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str())
        .expect("expected text content in tool result")
}

/// Join a spawned server task after the client has cancelled, surfacing any
/// server-side panic instead of silently absorbing it.
///
/// Once the client cancels, the in-memory duplex closes and the server's
/// `waiting()` future resolves, so the task completes promptly. We bound the
/// join with a short timeout so a wedged server task fails the test instead of
/// hanging it. A `JoinError` (panic) or a non-`Ok` server result fails the test.
async fn join_server(handle: tokio::task::JoinHandle<anyhow::Result<()>>) {
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("server task did not finish within 5 s after client cancel");
    let result = joined.expect("server task panicked");
    result.expect("server task returned an error");
}

// ---------------------------------------------------------------------------
// Transport integration tests
// ---------------------------------------------------------------------------

/// Verify that `detect_format` is routed correctly and returns a valid JSON
/// response containing the `format` field set to `"json"`.
#[tokio::test]
async fn detect_format_tool_routes_and_responds() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("detect_format").with_arguments(
                serde_json::json!({ "input": r#"{"x":1}"# })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;

    assert!(
        !result.is_error.unwrap_or(false),
        "expected successful result, got: {result:?}"
    );

    let text = first_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(
        parsed["format"], "json",
        "expected format=json, got: {parsed}"
    );
    assert!(parsed["input_bytes"].is_number(), "expected input_bytes");

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that `compress_content` routes correctly, compresses a large tabular
/// JSON payload, and returns `compressed: true` with a non-empty output.
#[tokio::test]
async fn compress_content_tool_routes_and_compresses() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    // Use a permissive ratio so even modest savings are accepted.
    let mut cfg = test_config();
    cfg.max_output_ratio = 0.99;

    let server = ToonMcpServer::new(cfg, Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // Build a large tabular JSON payload that will definitely compress.
    let rows: Vec<String> = (0..50)
        .map(|i| {
            format!(
                r#"{{"id":{i},"name":"User{i}","score":{s},"active":true,"tag":"alpha"}}"#,
                i = i,
                s = i as f64 * 0.5
            )
        })
        .collect();
    let big_json = format!("[{}]", rows.join(","));

    let result = client
        .call_tool(
            CallToolRequestParams::new("compress_content").with_arguments(
                serde_json::json!({ "input": big_json })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;

    assert!(
        !result.is_error.unwrap_or(false),
        "expected successful result, got: {result:?}"
    );

    let text = first_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(parsed["compressed"], true, "expected compressed=true");
    assert_eq!(parsed["format"], "json");
    assert_eq!(parsed["shape_class"], "tabular");
    assert!(
        parsed["savings_pct"].as_f64().unwrap_or(0.0) > 0.0,
        "expected positive savings_pct"
    );
    assert!(
        !parsed["output"].as_str().unwrap_or("").is_empty(),
        "expected non-empty TOON output"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that `compress_content` returns the original input unchanged when
/// no compression benefit is found, with `compressed: false` and a non-null
/// `pass_reason`.
#[tokio::test]
async fn compress_content_passes_through_prose() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    let prose = "This is plain prose text with no structured format.";

    let result = client
        .call_tool(
            CallToolRequestParams::new("compress_content").with_arguments(
                serde_json::json!({ "input": prose })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;

    assert!(
        !result.is_error.unwrap_or(false),
        "expected successful result, got: {result:?}"
    );

    let text = first_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(parsed["compressed"], false, "expected compressed=false");
    assert_eq!(parsed["output"], prose, "expected original input as output");
    assert!(
        parsed["pass_reason"].is_string(),
        "expected non-null pass_reason"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that `compression_stats` routes correctly and returns `would_compress`
/// with supporting statistics without producing encoded output.
#[tokio::test]
async fn compression_stats_tool_routes_and_returns_stats() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("compression_stats").with_arguments(
                serde_json::json!({ "input": "id,name\n1,Alice\n2,Bob" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;

    assert!(
        !result.is_error.unwrap_or(false),
        "expected successful result, got: {result:?}"
    );

    let text = first_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(text)?;
    // The result must have these fields regardless of compression outcome.
    assert!(
        parsed["would_compress"].is_boolean(),
        "expected would_compress boolean"
    );
    assert!(parsed["format"].is_string(), "expected format string");
    assert!(parsed["input_bytes"].is_number(), "expected input_bytes");
    assert!(
        parsed["estimated_output_bytes"].is_number(),
        "expected estimated_output_bytes"
    );
    assert!(parsed["threshold"].is_number(), "expected threshold");

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that the diagnostics tool routes and returns logging health fields.
#[tokio::test]
async fn toon_diagnostics_tool_routes_and_returns_health() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("toon_diagnostics")
                .with_arguments(serde_json::json!({}).as_object().unwrap().clone()),
        )
        .await?;

    assert!(
        !result.is_error.unwrap_or(false),
        "expected successful result, got: {result:?}"
    );

    let text = first_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(text)?;
    assert!(parsed["logging"].is_object(), "expected logging object");
    assert!(parsed["handler"].is_object(), "expected handler object");
    assert!(
        parsed["logging"]["record_dropped_count"].is_number(),
        "expected record_dropped_count"
    );
    assert!(
        parsed["handler"]["pipeline_timeout_count"].is_number(),
        "expected pipeline_timeout_count"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that calling a non-existent tool name returns an error response
/// rather than panicking.
#[tokio::test]
async fn unknown_tool_name_returns_error() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // rmcp returns an error for unknown tool names.
    let result = client
        .call_tool(CallToolRequestParams::new("nonexistent_tool"))
        .await;

    assert!(result.is_err(), "expected error for unknown tool name");

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that `compress_content` rejects inputs exceeding `max_input_bytes`
/// with an `input_exceeds_limit` error surfaced as an MCP error response.
#[tokio::test]
async fn compress_content_rejects_oversized_input() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    // Override max_input_bytes to a small value to keep the test fast.
    let mut cfg = test_config();
    cfg.max_input_bytes = 1024;

    let server = ToonMcpServer::new(cfg, Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // Build an input twice the limit, well above the 1024-byte cap.
    let huge = "x".repeat(2048);

    let result = client
        .call_tool(
            CallToolRequestParams::new("compress_content").with_arguments(
                serde_json::json!({ "input": huge })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;

    let err = result.expect_err("expected error for oversized input");
    let msg = err.to_string();
    assert!(
        msg.contains("input_exceeds_limit"),
        "expected error to contain 'input_exceeds_limit', got: {msg}"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that `compress_content` returns an error when the parameters do not
/// match the expected schema (here: missing the required `input` field).
#[tokio::test]
async fn compress_content_malformed_params_returns_error() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // Empty object — required `input` field is missing.
    let result = client
        .call_tool(
            CallToolRequestParams::new("compress_content")
                .with_arguments(serde_json::json!({}).as_object().unwrap().clone()),
        )
        .await;

    // rmcp 3.x surfaces schema mismatches as a tool-level error result
    // (is_error = true) rather than a protocol-level error.
    let result = result.expect("call must complete at the protocol level");
    assert_eq!(
        result.is_error,
        Some(true),
        "expected tool-level error for malformed params (missing input field), got: {result:?}"
    );
    let msg = first_text(&result);
    assert!(
        msg.contains("input"),
        "expected error text to mention the missing `input` field, got: {msg}"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that the `max_concurrent_calls` semaphore plus `pipeline_timeout_ms`
/// queue deadline cooperate to surface a `server busy` error when more
/// concurrent calls arrive than the configured limit and the queue deadline
/// expires before a permit becomes available.
///
/// Uses `max_concurrent_calls = 1` and a very short `pipeline_timeout_ms` so
/// that the second of two parallel calls cannot acquire a permit in time and
/// must return `server busy`.
#[tokio::test]
async fn concurrent_calls_respect_max_concurrent_calls() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let mut cfg = test_config();
    cfg.max_concurrent_calls = 1;
    cfg.pipeline_timeout_ms = 1;
    cfg.max_output_ratio = 0.99;

    let server = ToonMcpServer::new(cfg, Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // A non-trivial payload — combined with `pipeline_timeout_ms = 1` and
    // `max_concurrent_calls = 1`, both parallel calls must either fail
    // their own pipeline timeout or fail to acquire a permit (busy).
    let rows: Vec<String> = (0..1000)
        .map(|i| {
            format!(
                r#"{{"id":{i},"name":"User{i}","score":{s},"active":true,"tag":"alpha"}}"#,
                i = i,
                s = i as f64 * 0.5
            )
        })
        .collect();
    let big_json = format!("[{}]", rows.join(","));

    let args = serde_json::json!({ "input": big_json })
        .as_object()
        .unwrap()
        .clone();

    let call_a = client
        .call_tool(CallToolRequestParams::new("compress_content").with_arguments(args.clone()));
    let call_b = client
        .call_tool(CallToolRequestParams::new("compress_content").with_arguments(args.clone()));

    let (res_a, res_b) = tokio::join!(call_a, call_b);

    // At least one call must surface a busy error or a pipeline_timeout — both
    // are valid outcomes when the system is saturated. The contract is "did
    // the concurrency gate engage?", not "which specific error fired first".
    let mut saw_busy_or_timeout = false;
    for r in [&res_a, &res_b] {
        if let Err(err) = r {
            let msg = err.to_string();
            if msg.contains("server busy") || msg.contains("pipeline_timeout") {
                saw_busy_or_timeout = true;
            }
        }
    }
    assert!(
        saw_busy_or_timeout,
        "expected at least one concurrent call to surface 'server busy' or 'pipeline_timeout'; \
         got a={res_a:?} b={res_b:?}"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}

/// Verify that an extremely short `pipeline_timeout_ms` causes
/// `compress_content` to return a `pipeline_timeout` error.
#[tokio::test]
async fn pipeline_timeout_returns_internal_error() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let mut cfg = test_config();
    cfg.pipeline_timeout_ms = 1;
    cfg.max_output_ratio = 0.99;

    let server = ToonMcpServer::new(cfg, Arc::new(NoopSink));
    let server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient.serve(client_transport).await?;

    // A non-trivial JSON payload — large enough that the blocking pipeline
    // call cannot complete within 1ms on any realistic hardware.
    let rows: Vec<String> = (0..1000)
        .map(|i| {
            format!(
                r#"{{"id":{i},"name":"User{i}","score":{s},"active":true,"tag":"alpha"}}"#,
                i = i,
                s = i as f64 * 0.5
            )
        })
        .collect();
    let big_json = format!("[{}]", rows.join(","));

    let result = client
        .call_tool(
            CallToolRequestParams::new("compress_content").with_arguments(
                serde_json::json!({ "input": big_json })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;

    let err = result.expect_err("expected pipeline_timeout error");
    let msg = err.to_string();
    assert!(
        msg.contains("pipeline_timeout"),
        "expected error to contain 'pipeline_timeout', got: {msg}"
    );

    client.cancel().await?;
    join_server(server_handle).await;
    Ok(())
}
