// file: crates/toon-mcp-server/src/handler.rs
// description: Tool handler functions and input/output types for the three MCP tools

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use rmcp::ErrorData as McpError;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Schema helper: emit `{ "type": "integer" }` without a Go-style `"format"`
/// annotation (`"uint"`, `"uint64"`, etc.) that AJV does not recognise.
fn schema_as_integer(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "integer" })
}

use toon_mcp_core::{CompressConfig, CompressDecision, Compressor, FormatDetector, InputFormat};
use toon_mcp_logging::{LogEvent, LogSink};

use crate::config::Config;

// ---------------------------------------------------------------------------
// Config → CompressConfig conversion
// ---------------------------------------------------------------------------

impl From<&Config> for CompressConfig {
    fn from(c: &Config) -> Self {
        Self {
            max_output_ratio: c.max_output_ratio,
            min_bytes: c.min_bytes,
            max_input_bytes: c.max_input_bytes,
            key_folding: c.key_folding,
            delimiter: c.delimiter,
            tabular_min_rows: c.tabular_min_rows,
            fold_min_depth: c.fold_min_depth,
            primitive_array_min: c.primitive_array_min,
        }
    }
}

// ---------------------------------------------------------------------------
// Named outcome struct for compress pipeline (L2)
// ---------------------------------------------------------------------------

/// Intermediate result derived from a `CompressDecision` for use in handlers.
struct CompressOutcome {
    output: String,
    compressed: bool,
    format_str: String,
    shape_str: String,
    output_bytes: usize,
    savings_pct: f64,
    pass_reason_str: Option<String>,
}

// ---------------------------------------------------------------------------
// detect_format
// ---------------------------------------------------------------------------

/// Input parameters for `detect_format`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DetectParams {
    /// The raw input string to detect the format of.
    pub input: String,
}

/// Output from `detect_format`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DetectResult {
    /// Detected format: `"json"`, `"jsonl"`, `"csv"`, `"tsv"`, or `"unknown"`.
    pub format: String,
    /// Byte length of the input string.
    #[schemars(schema_with = "schema_as_integer")]
    pub input_bytes: usize,
    /// Number of non-empty lines (populated for JSONL inputs).
    #[schemars(schema_with = "schema_as_integer")]
    pub line_count: Option<usize>,
    /// Number of columns in the first header row (populated for CSV/TSV inputs).
    #[schemars(schema_with = "schema_as_integer")]
    pub column_count: Option<usize>,
}

/// Handle the `detect_format` MCP tool.
pub async fn handle_detect_format(
    params: DetectParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<DetectResult, McpError> {
    let input = params.input;
    let input_bytes = input.len();

    // Guard against oversized input before any allocation.
    if input_bytes > config.max_input_bytes {
        return Err(McpError::invalid_params(
            format!(
                "input_exceeds_limit: input is {input_bytes} bytes; \
                 maximum allowed is {} bytes (TOON_MAX_INPUT_BYTES)",
                config.max_input_bytes
            ),
            None,
        ));
    }

    // M1: Acquire concurrency permit before dispatching to spawn_blocking.
    let _permit = semaphore
        .try_acquire()
        .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?;

    let event_id = Uuid::new_v4().to_string();

    // M6: Emit event_id into tracing span for correlation with LogEvent.
    let span = tracing::info_span!("detect_format", event_id = %event_id);
    let _enter = span.enter();

    // C1: Run the synchronous detect call on a blocking thread — FormatDetector::detect
    // performs a full serde_json::from_str and CSV allocation which must not
    // run on the tokio executor.
    let timeout = Duration::from_millis(config.pipeline_timeout_ms);
    let start = Instant::now();
    let (fmt, line_count, column_count) = tokio::time::timeout(timeout, async {
        tokio::task::spawn_blocking(move || {
            let fmt = FormatDetector::detect(&input);
            let line_count = FormatDetector::jsonl_line_count(fmt, &input);
            let column_count = FormatDetector::column_count(fmt, &input);
            (fmt, line_count, column_count)
        })
        .await
    })
    .await
    .map_err(|_| {
        McpError::invalid_params(
            format!(
                "pipeline_timeout: detection did not complete within {}ms \
                 (TOON_PIPELINE_TIMEOUT_MS)",
                config.pipeline_timeout_ms
            ),
            None,
        )
    })?
    // H2: JoinError from a panicking task is an internal server error, not a
    // bad client request.
    .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?;

    let duration_us = start.elapsed().as_micros() as u64;

    let event = LogEvent {
        event_id: event_id.clone(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "detect_format".into(),
        input_format: fmt.as_str().into(),
        shape_class: toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
        input_bytes: input_bytes as u64,
        output_bytes: input_bytes as u64,
        compressed: false,
        savings_pct: 0.0,
        threshold_used: config.max_output_ratio,
        duration_us,
        pass_reason: None,
        client_hint: config.client_hint.clone(),
    };

    // Fire-and-forget; backpressure is handled by the bounded channel.
    let _ = log_sink.record(event).await;

    Ok(DetectResult {
        format: fmt.as_str().into(),
        input_bytes,
        line_count,
        column_count,
    })
}

// ---------------------------------------------------------------------------
// compress_content
// ---------------------------------------------------------------------------

/// Input parameters for `compress_content`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompressParams {
    /// The raw structured input to compress.
    pub input: String,
}

/// Output from `compress_content`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CompressResult {
    /// The output string: TOON-encoded when `compressed` is true, otherwise
    /// the original `input` unchanged.
    pub output: String,
    /// Whether TOON encoding was applied.
    pub compressed: bool,
    /// Detected input format.
    pub format: String,
    /// Classifier shape class.
    pub shape_class: String,
    /// Byte length of the input string.
    #[schemars(schema_with = "schema_as_integer")]
    pub input_bytes: usize,
    /// Byte length of the output string.
    #[schemars(schema_with = "schema_as_integer")]
    pub output_bytes: usize,
    /// Fraction of bytes saved (0.0 when not compressed).
    pub savings_pct: f64,
    /// Wall-clock duration in microseconds.
    #[schemars(schema_with = "schema_as_integer")]
    pub duration_us: u64,
    /// Reason compression was skipped (when `compressed` is false).
    pub pass_reason: Option<String>,
}

/// Handle the `compress_content` MCP tool.
///
/// Returns both the (possibly compressed) output and the original input when
/// passing through, by moving `input` into `spawn_blocking` and returning it
/// together with the `CompressDecision`.
pub(crate) async fn handle_compress_content_inner(
    params: CompressParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<CompressResult, McpError> {
    let input = params.input;
    let input_bytes = input.len();

    if input_bytes > config.max_input_bytes {
        return Err(McpError::invalid_params(
            format!(
                "input_exceeds_limit: input is {input_bytes} bytes; \
                 maximum allowed is {} bytes (TOON_MAX_INPUT_BYTES)",
                config.max_input_bytes
            ),
            None,
        ));
    }

    let _permit = semaphore
        .try_acquire()
        .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?;

    let event_id = Uuid::new_v4().to_string();
    let span = tracing::info_span!("compress_content", event_id = %event_id);
    let _enter = span.enter();

    let compress_config = CompressConfig::from(config.as_ref());
    let timeout = Duration::from_millis(config.pipeline_timeout_ms);
    let start = Instant::now();

    // L1: Return (input, decision) so input is available for pass-through output.
    let (input, decision) = tokio::time::timeout(timeout, async {
        tokio::task::spawn_blocking(move || {
            let decision = Compressor::decide(&input, &compress_config);
            (input, decision)
        })
        .await
    })
    .await
    .map_err(|_| {
        McpError::invalid_params(
            format!(
                "pipeline_timeout: compression did not complete within {}ms \
                 (TOON_PIPELINE_TIMEOUT_MS)",
                config.pipeline_timeout_ms
            ),
            None,
        )
    })?
    .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?;

    let duration_us = start.elapsed().as_micros() as u64;

    // L2 + H3: named struct, no second detect call.
    let outcome = match &decision {
        CompressDecision::Compressed {
            toon,
            toon_bytes,
            savings_pct,
            input_format,
            shape_class,
            ..
        } => CompressOutcome {
            output: toon.clone(),
            compressed: true,
            format_str: input_format.as_str().into(),
            shape_str: shape_class.as_str().into(),
            output_bytes: *toon_bytes,
            savings_pct: *savings_pct,
            pass_reason_str: None,
        },
        CompressDecision::PassedThrough {
            reason,
            input_format,
        } => CompressOutcome {
            output: input.clone(),
            compressed: false,
            format_str: input_format.unwrap_or(InputFormat::Unknown).as_str().into(),
            shape_str: toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
            output_bytes: input_bytes,
            savings_pct: 0.0,
            pass_reason_str: Some(reason.as_str().into()),
        },
    };

    let event = LogEvent {
        event_id: event_id.clone(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "compress_content".into(),
        input_format: outcome.format_str.clone(),
        shape_class: outcome.shape_str.clone(),
        input_bytes: input_bytes as u64,
        output_bytes: outcome.output_bytes as u64,
        compressed: outcome.compressed,
        savings_pct: outcome.savings_pct,
        threshold_used: config.max_output_ratio,
        duration_us,
        pass_reason: outcome.pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    Ok(CompressResult {
        output: outcome.output,
        compressed: outcome.compressed,
        format: outcome.format_str,
        shape_class: outcome.shape_str,
        input_bytes,
        output_bytes: outcome.output_bytes,
        savings_pct: outcome.savings_pct,
        duration_us,
        pass_reason: outcome.pass_reason_str,
    })
}

// ---------------------------------------------------------------------------
// compression_stats
// ---------------------------------------------------------------------------

/// Input parameters for `compression_stats`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatsParams {
    /// The raw input to preview compression for.
    pub input: String,
}

/// Output from `compression_stats`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StatsResult {
    /// Whether the compressor would apply TOON encoding.
    pub would_compress: bool,
    /// Detected input format.
    pub format: String,
    /// Classifier shape class.
    pub shape_class: String,
    /// Byte length of the input string.
    #[schemars(schema_with = "schema_as_integer")]
    pub input_bytes: usize,
    /// Estimated output byte length after compression.
    #[schemars(schema_with = "schema_as_integer")]
    pub estimated_output_bytes: usize,
    /// Estimated fraction of bytes saved.
    pub estimated_savings_pct: f64,
    /// Active compression threshold (maximum output-to-input ratio).
    pub threshold: f64,
    /// Reason compression would be skipped (when `would_compress` is false).
    pub pass_reason: Option<String>,
}

/// Handle the `compression_stats` MCP tool.
///
/// Performs the full pipeline (detect, parse, classify, encode) but returns
/// statistics instead of the encoded output. The encoded string is discarded.
pub async fn handle_compression_stats(
    params: StatsParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<StatsResult, McpError> {
    let input = params.input;
    let input_bytes = input.len();

    if input_bytes > config.max_input_bytes {
        return Err(McpError::invalid_params(
            format!(
                "input_exceeds_limit: input is {input_bytes} bytes; \
                 maximum allowed is {} bytes (TOON_MAX_INPUT_BYTES)",
                config.max_input_bytes
            ),
            None,
        ));
    }

    let _permit = semaphore
        .try_acquire()
        .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?;

    let event_id = Uuid::new_v4().to_string();
    let span = tracing::info_span!("compression_stats", event_id = %event_id);
    let _enter = span.enter();

    let compress_config = CompressConfig::from(config.as_ref());
    let timeout = Duration::from_millis(config.pipeline_timeout_ms);
    let start = Instant::now();

    // L1: Return (input, decision) — no clone needed.
    let (_input, decision) = tokio::time::timeout(timeout, async {
        tokio::task::spawn_blocking(move || {
            let decision = Compressor::decide(&input, &compress_config);
            (input, decision)
        })
        .await
    })
    .await
    .map_err(|_| {
        McpError::invalid_params(
            format!(
                "pipeline_timeout: compression did not complete within {}ms \
                 (TOON_PIPELINE_TIMEOUT_MS)",
                config.pipeline_timeout_ms
            ),
            None,
        )
    })?
    .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?;

    let duration_us = start.elapsed().as_micros() as u64;

    // L2 + H3: named struct, no second detect call.
    let outcome = match &decision {
        CompressDecision::Compressed {
            toon_bytes,
            savings_pct,
            input_format,
            shape_class,
            ..
        } => CompressOutcome {
            output: String::new(), // not used for stats
            compressed: true,
            format_str: input_format.as_str().into(),
            shape_str: shape_class.as_str().into(),
            output_bytes: *toon_bytes,
            savings_pct: *savings_pct,
            pass_reason_str: None,
        },
        CompressDecision::PassedThrough {
            reason,
            input_format,
        } => CompressOutcome {
            output: String::new(),
            compressed: false,
            format_str: input_format.unwrap_or(InputFormat::Unknown).as_str().into(),
            shape_str: toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
            output_bytes: input_bytes,
            savings_pct: 0.0,
            pass_reason_str: Some(reason.as_str().into()),
        },
    };

    let event = LogEvent {
        event_id: event_id.clone(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "compression_stats".into(),
        input_format: outcome.format_str.clone(),
        shape_class: outcome.shape_str.clone(),
        input_bytes: input_bytes as u64,
        output_bytes: outcome.output_bytes as u64,
        compressed: outcome.compressed,
        savings_pct: outcome.savings_pct,
        threshold_used: config.max_output_ratio,
        duration_us,
        pass_reason: outcome.pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    Ok(StatsResult {
        would_compress: outcome.compressed,
        format: outcome.format_str,
        shape_class: outcome.shape_str,
        input_bytes,
        estimated_output_bytes: outcome.output_bytes,
        estimated_savings_pct: outcome.savings_pct,
        threshold: config.max_output_ratio,
        pass_reason: outcome.pass_reason_str,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use toon_mcp_logging::{LogEvent, MemorySink, NoopSink};

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            max_output_ratio: 0.85,
            min_bytes: 256,
            max_input_bytes: toon_mcp_core::DEFAULT_MAX_INPUT_BYTES,
            key_folding: true,
            delimiter: toon_format::Delimiter::Comma,
            tabular_min_rows: 3,
            fold_min_depth: 3,
            primitive_array_min: 5,
            logging_enabled: false,
            logging: toon_mcp_logging::JsonlSinkConfig::default(),
            log_level: "info".into(),
            client_hint: None,
            pipeline_timeout_ms: 30_000,
            max_concurrent_calls: 8,
        })
    }

    fn noop_sink() -> Arc<dyn LogSink> {
        Arc::new(NoopSink)
    }

    fn test_semaphore(config: &Config) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(config.max_concurrent_calls))
    }

    // --- detect_format ---

    #[tokio::test]
    async fn detect_format_json() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: r#"{"key":"value"}"#.into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.format, "json");
        assert_eq!(result.input_bytes, 15);
        assert!(result.line_count.is_none());
        assert!(result.column_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_jsonl_has_line_count() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: "{\"a\":1}\n{\"b\":2}\n{\"c\":3}".into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.format, "jsonl");
        assert_eq!(result.line_count, Some(3));
        assert!(result.column_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_csv_has_column_count() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: "id,name,score\n1,Alice,9.5".into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.format, "csv");
        assert_eq!(result.column_count, Some(3));
        assert!(result.line_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_unknown() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: "this is plain text".into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.format, "unknown");
    }

    #[tokio::test]
    async fn detect_format_rejects_oversized_input() {
        let cfg = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: "x".repeat(100),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));
    }

    // --- compress_content ---

    #[tokio::test]
    async fn compress_below_threshold_passes_through() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_compress_content_inner(
            CompressParams {
                input: r#"{"x":1}"#.into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert!(!result.compressed);
        assert_eq!(result.output, r#"{"x":1}"#);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
    }

    #[tokio::test]
    async fn compress_rejects_oversized_input() {
        let cfg = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let sem = test_semaphore(&cfg);
        let result = handle_compress_content_inner(
            CompressParams {
                input: "x".repeat(100),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compress_content_large_tabular_compresses() {
        let rows: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"name":"User{i}","score":{score},"active":true,"tag":"alpha"}}"#,
                    i = i,
                    score = i as f64 * 0.5
                )
            })
            .collect();
        let input = format!("[{}]", rows.join(","));

        let cfg = Arc::new(Config {
            max_output_ratio: 0.99,
            ..(*test_config()).clone()
        });
        let sem = test_semaphore(&cfg);
        let result = handle_compress_content_inner(CompressParams { input }, cfg, noop_sink(), sem)
            .await
            .unwrap();
        assert!(result.compressed);
        assert!(result.savings_pct > 0.0);
        assert_eq!(result.format, "json");
        assert_eq!(result.shape_class, "tabular");
    }

    // --- compression_stats ---

    #[tokio::test]
    async fn compression_stats_below_threshold() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_compression_stats(
            StatsParams {
                input: r#"{"x":1}"#.into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert!(!result.would_compress);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
    }

    #[tokio::test]
    async fn compression_stats_rejects_oversized_input() {
        let cfg = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let sem = test_semaphore(&cfg);
        let result = handle_compression_stats(
            StatsParams {
                input: "x".repeat(100),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await;
        assert!(result.is_err());
    }

    // --- MemorySink integration tests ---

    fn memory_sink() -> (Arc<dyn LogSink>, Arc<Mutex<Vec<LogEvent>>>) {
        let (sink, events) = MemorySink::new();
        (Arc::new(sink), events)
    }

    #[tokio::test]
    async fn detect_format_emits_correct_log_event() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let (sink, events) = memory_sink();
        handle_detect_format(
            DetectParams {
                input: r#"{"key":"value"}"#.into(),
            },
            cfg,
            sink,
            sem,
        )
        .await
        .unwrap();

        let locked = events.lock().unwrap();
        assert_eq!(locked.len(), 1);
        let ev = &locked[0];
        assert_eq!(ev.tool_name, "detect_format");
        assert_eq!(ev.input_format, "json");
        assert_eq!(ev.input_bytes, 15);
        assert!(!ev.compressed);
        assert_eq!(ev.savings_pct, 0.0);
        assert!(ev.duration_us > 0);
        assert!(ev.pass_reason.is_none());
    }

    #[tokio::test]
    async fn compress_content_emits_correct_log_event_on_pass_through() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let (sink, events) = memory_sink();
        handle_compress_content_inner(
            CompressParams {
                input: r#"{"x":1}"#.into(),
            },
            cfg,
            sink,
            sem,
        )
        .await
        .unwrap();

        let locked = events.lock().unwrap();
        assert_eq!(locked.len(), 1);
        let ev = &locked[0];
        assert_eq!(ev.tool_name, "compress_content");
        assert!(!ev.compressed);
        assert_eq!(ev.savings_pct, 0.0);
        assert_eq!(ev.pass_reason.as_deref(), Some("below_min_bytes"));
    }

    #[tokio::test]
    async fn compress_content_emits_correct_log_event_on_compress() {
        let rows: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"name":"User{i}","score":{score},"active":true}}"#,
                    i = i,
                    score = i as f64 * 0.5
                )
            })
            .collect();
        let input = format!("[{}]", rows.join(","));

        let cfg = Arc::new(Config {
            max_output_ratio: 0.99,
            ..(*test_config()).clone()
        });
        let sem = test_semaphore(&cfg);
        let (sink, events) = memory_sink();
        handle_compress_content_inner(CompressParams { input }, cfg, sink, sem)
            .await
            .unwrap();

        let locked = events.lock().unwrap();
        assert_eq!(locked.len(), 1);
        let ev = &locked[0];
        assert_eq!(ev.tool_name, "compress_content");
        assert!(ev.compressed);
        assert!(ev.savings_pct > 0.0);
        assert_eq!(ev.input_format, "json");
        assert_eq!(ev.shape_class, "tabular");
        assert!(ev.pass_reason.is_none());
    }

    #[tokio::test]
    async fn compression_stats_emits_correct_log_event() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let (sink, events) = memory_sink();
        handle_compression_stats(
            StatsParams {
                input: r#"{"x":1}"#.into(),
            },
            cfg,
            sink,
            sem,
        )
        .await
        .unwrap();

        let locked = events.lock().unwrap();
        assert_eq!(locked.len(), 1);
        let ev = &locked[0];
        assert_eq!(ev.tool_name, "compression_stats");
        assert!(!ev.compressed);
        assert_eq!(ev.pass_reason.as_deref(), Some("below_min_bytes"));
    }
}
