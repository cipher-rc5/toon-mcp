// file: crates/toon-mcp-server/src/handler.rs
// description: Tool handler functions and input/output types for the three MCP tools

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use rmcp::ErrorData as McpError;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema helper: emit `{ "type": "integer" }` without a Go-style `"format"`
/// annotation (`"uint"`, `"uint64"`, etc.) that AJV does not recognise.
fn schema_as_integer(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "integer" })
}

use toon_mcp_core::{CompressConfig, CompressDecision, Compressor, FormatDetector};
use toon_mcp_logging::{LogEvent, LogSink};

use crate::config::Config;

// ---------------------------------------------------------------------------
// Config → CompressConfig conversion (eliminates L3 DRY violation)
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
) -> Result<DetectResult, McpError> {
    let input = &params.input;
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

    let start = Instant::now();
    let fmt = FormatDetector::detect(input);

    // Use core helpers — no csv import needed in the server layer.
    let line_count = FormatDetector::jsonl_line_count(fmt, input);
    let column_count = FormatDetector::column_count(fmt, input);

    let duration_us = start.elapsed().as_micros() as u64;

    let event = LogEvent {
        event_id: Uuid::new_v4().to_string(),
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
pub async fn handle_compress_content(
    params: CompressParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
) -> Result<CompressResult, McpError> {
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

    let compress_config = CompressConfig::from(config.as_ref());

    // Run the compression pipeline with a per-call timeout.
    let timeout = Duration::from_millis(config.pipeline_timeout_ms);
    let start = Instant::now();
    let decision = tokio::time::timeout(timeout, async {
        // Compressor::decide is synchronous; wrap in spawn_blocking so we
        // do not hold the tokio thread for large inputs.
        let input_clone = input.clone();
        tokio::task::spawn_blocking(move || Compressor::decide(&input_clone, &compress_config))
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
    .map_err(|e| McpError::invalid_params(format!("spawn_blocking failed: {e}"), None))?;

    let duration_us = start.elapsed().as_micros() as u64;

    // Derive structured fields from the decision.
    // CompressDecision::Compressed now carries input_format and shape_class,
    // eliminating the previous double-parse.
    let (output, compressed, format_str, shape_str, output_bytes, savings_pct, pass_reason_str): (
        String,
        bool,
        String,
        String,
        usize,
        f64,
        Option<String>,
    ) = match &decision {
        CompressDecision::Compressed {
            toon,
            toon_bytes,
            savings_pct,
            input_format,
            shape_class,
            ..
        } => (
            toon.clone(),
            true,
            input_format.as_str().into(),
            shape_class.as_str().into(),
            *toon_bytes,
            *savings_pct,
            None,
        ),
        CompressDecision::PassedThrough { reason } => {
            let fmt: String = FormatDetector::detect(&input).as_str().into();
            let reason_str: String = reason.as_str().into();
            (
                input.clone(),
                false,
                fmt,
                toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
                input_bytes,
                0.0,
                Some(reason_str),
            )
        }
    };

    let event = LogEvent {
        event_id: Uuid::new_v4().to_string(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "compress_content".into(),
        input_format: format_str.clone(),
        shape_class: shape_str.clone(),
        input_bytes: input_bytes as u64,
        output_bytes: output_bytes as u64,
        compressed,
        savings_pct,
        threshold_used: config.max_output_ratio,
        duration_us,
        pass_reason: pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    Ok(CompressResult {
        output,
        compressed,
        format: format_str,
        shape_class: shape_str,
        input_bytes,
        output_bytes,
        savings_pct,
        duration_us,
        pass_reason: pass_reason_str,
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
) -> Result<StatsResult, McpError> {
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

    let compress_config = CompressConfig::from(config.as_ref());

    let timeout = Duration::from_millis(config.pipeline_timeout_ms);
    let start = Instant::now();
    let decision = tokio::time::timeout(timeout, async {
        let input_clone = input.clone();
        tokio::task::spawn_blocking(move || Compressor::decide(&input_clone, &compress_config))
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
    .map_err(|e| McpError::invalid_params(format!("spawn_blocking failed: {e}"), None))?;

    let duration_us = start.elapsed().as_micros() as u64;

    // Derive format for pass-through paths where CompressDecision does not carry it.
    let fmt = FormatDetector::detect(&input);

    let (would_compress, shape_str, estimated_output_bytes, estimated_savings_pct, pass_reason_str):
        (bool, String, usize, f64, Option<String>) =
        match &decision {
            CompressDecision::Compressed {
                toon_bytes,
                savings_pct,
                shape_class,
                ..
            } => (
                true,
                shape_class.as_str().into(),
                *toon_bytes,
                *savings_pct,
                None,
            ),
            CompressDecision::PassedThrough { reason } => {
                let reason_str: String = reason.as_str().into();
                (
                    false,
                    toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
                    input_bytes,
                    0.0,
                    Some(reason_str),
                )
            }
        };

    let event = LogEvent {
        event_id: Uuid::new_v4().to_string(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "compression_stats".into(),
        input_format: fmt.as_str().into(),
        shape_class: shape_str.clone(),
        input_bytes: input_bytes as u64,
        output_bytes: estimated_output_bytes as u64,
        compressed: would_compress,
        savings_pct: estimated_savings_pct,
        threshold_used: config.max_output_ratio,
        duration_us,
        pass_reason: pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    Ok(StatsResult {
        would_compress,
        format: fmt.as_str().into(),
        shape_class: shape_str,
        input_bytes,
        estimated_output_bytes,
        estimated_savings_pct,
        threshold: config.max_output_ratio,
        pass_reason: pass_reason_str,
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
            logging: toon_mcp_logging::ParquetSinkConfig::default(),
            log_level: "info".into(),
            client_hint: None,
            pipeline_timeout_ms: 30_000,
        })
    }

    fn noop_sink() -> Arc<dyn LogSink> {
        Arc::new(NoopSink)
    }

    // --- detect_format ---

    #[tokio::test]
    async fn detect_format_json() {
        let result = handle_detect_format(
            DetectParams {
                input: r#"{"key":"value"}"#.into(),
            },
            test_config(),
            noop_sink(),
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
        let result = handle_detect_format(
            DetectParams {
                input: "{\"a\":1}\n{\"b\":2}\n{\"c\":3}".into(),
            },
            test_config(),
            noop_sink(),
        )
        .await
        .unwrap();
        assert_eq!(result.format, "jsonl");
        assert_eq!(result.line_count, Some(3));
        assert!(result.column_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_csv_has_column_count() {
        let result = handle_detect_format(
            DetectParams {
                input: "id,name,score\n1,Alice,9.5".into(),
            },
            test_config(),
            noop_sink(),
        )
        .await
        .unwrap();
        assert_eq!(result.format, "csv");
        assert_eq!(result.column_count, Some(3));
        assert!(result.line_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_unknown() {
        let result = handle_detect_format(
            DetectParams {
                input: "this is plain text".into(),
            },
            test_config(),
            noop_sink(),
        )
        .await
        .unwrap();
        assert_eq!(result.format, "unknown");
    }

    #[tokio::test]
    async fn detect_format_rejects_oversized_input() {
        let config = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let result = handle_detect_format(
            DetectParams {
                input: "x".repeat(100),
            },
            config,
            noop_sink(),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));
    }

    // --- compress_content ---

    #[tokio::test]
    async fn compress_below_threshold_passes_through() {
        let result = handle_compress_content(
            CompressParams {
                input: r#"{"x":1}"#.into(),
            },
            test_config(),
            noop_sink(),
        )
        .await
        .unwrap();
        assert!(!result.compressed);
        assert_eq!(result.output, r#"{"x":1}"#);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
    }

    #[tokio::test]
    async fn compress_rejects_oversized_input() {
        let config = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let result = handle_compress_content(
            CompressParams {
                input: "x".repeat(100),
            },
            config,
            noop_sink(),
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

        let config = Arc::new(Config {
            max_output_ratio: 0.99,
            ..(*test_config()).clone()
        });
        let result = handle_compress_content(CompressParams { input }, config, noop_sink())
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
        let result = handle_compression_stats(
            StatsParams {
                input: r#"{"x":1}"#.into(),
            },
            test_config(),
            noop_sink(),
        )
        .await
        .unwrap();
        assert!(!result.would_compress);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
    }

    #[tokio::test]
    async fn compression_stats_rejects_oversized_input() {
        let config = Arc::new(Config {
            max_input_bytes: 10,
            ..(*test_config()).clone()
        });
        let result = handle_compression_stats(
            StatsParams {
                input: "x".repeat(100),
            },
            config,
            noop_sink(),
        )
        .await;
        assert!(result.is_err());
    }

    // --- MemorySink integration tests (M5) ---

    fn memory_sink() -> (Arc<dyn LogSink>, Arc<Mutex<Vec<LogEvent>>>) {
        let (sink, events) = MemorySink::new();
        (Arc::new(sink), events)
    }

    #[tokio::test]
    async fn detect_format_emits_correct_log_event() {
        let (sink, events) = memory_sink();
        handle_detect_format(
            DetectParams {
                input: r#"{"key":"value"}"#.into(),
            },
            test_config(),
            sink,
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
        let (sink, events) = memory_sink();
        handle_compress_content(
            CompressParams {
                input: r#"{"x":1}"#.into(),
            },
            test_config(),
            sink,
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

        let config = Arc::new(Config {
            max_output_ratio: 0.99,
            ..(*test_config()).clone()
        });
        let (sink, events) = memory_sink();
        handle_compress_content(CompressParams { input }, config, sink)
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
        let (sink, events) = memory_sink();
        handle_compression_stats(
            StatsParams {
                input: r#"{"x":1}"#.into(),
            },
            test_config(),
            sink,
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
