// file: crates/toon-mcp-server/tests/mcp_transport.rs
// description: MCP transport integration tests — verify tool routing and wire protocol

//! These tests verify that the `ToonMcpServer` correctly registers its three
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
        logging_enabled: false,
        logging: toon_mcp_logging::JsonlSinkConfig::default(),
        log_level: "error".into(),
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
        .and_then(|c| c.raw.as_text())
        .map(|t| t.text.as_str())
        .expect("expected text content in tool result")
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
    let _server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient::default().serve(client_transport).await?;

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
    let _server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient::default().serve(client_transport).await?;

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
    Ok(())
}

/// Verify that `compress_content` returns the original input unchanged when
/// no compression benefit is found, with `compressed: false` and a non-null
/// `pass_reason`.
#[tokio::test]
async fn compress_content_passes_through_prose() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let _server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient::default().serve(client_transport).await?;

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
    Ok(())
}

/// Verify that `compression_stats` routes correctly and returns `would_compress`
/// with supporting statistics without producing encoded output.
#[tokio::test]
async fn compression_stats_tool_routes_and_returns_stats() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let _server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient::default().serve(client_transport).await?;

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
    Ok(())
}

/// Verify that calling a non-existent tool name returns an error response
/// rather than panicking.
#[tokio::test]
async fn unknown_tool_name_returns_error() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(65_536);

    let server = ToonMcpServer::new(test_config(), Arc::new(NoopSink));
    let _server_handle = tokio::spawn(async move {
        let _ = server.serve(server_transport).await?.waiting().await;
        anyhow::Ok(())
    });

    let client = TestClient::default().serve(client_transport).await?;

    // rmcp returns an error for unknown tool names.
    let result = client
        .call_tool(CallToolRequestParams::new("nonexistent_tool"))
        .await;

    assert!(result.is_err(), "expected error for unknown tool name");

    client.cancel().await?;
    Ok(())
}
