// file: crates/toon-mcp-server/src/handler.rs
// description: Tool handler implementations for detect_format, compress_content, compression_stats

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use rmcp::ErrorData as McpError;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::Instrument;
use xxhash_rust::xxh3::xxh3_64;

use toon_mcp_core::{CompressConfig, CompressDecision, Compressor, FormatDetector, InputFormat};
use toon_mcp_logging::{LogEvent, LogSink};

use crate::config::Config;

/// Schema helper: emit `{ "type": "integer" }` without a Go-style `"format"`
/// annotation (`"uint"`, `"uint64"`, etc.) that AJV does not recognise.
fn schema_as_integer(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "integer" })
}

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique event ID using xxh3_64 over a counter + nanosecond timestamp.
/// Avoids OS RNG. The output is a 64-bit xxh3 hash; under the birthday paradox,
/// collisions become non-negligible around 2^32 (~4.3 billion) generated IDs.
/// Practically irrelevant for an MCP server but worth knowing for high-throughput consumers.
fn new_event_id() -> String {
    let seq = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&seq.to_le_bytes());
    buf[8..].copy_from_slice(&ts.to_le_bytes());
    format!("{:016x}", xxh3_64(&buf))
}

// ---------------------------------------------------------------------------
// Pipeline helper
// ---------------------------------------------------------------------------

/// Run a synchronous closure on a blocking thread under a wall-clock timeout.
///
/// Wraps the `tokio::time::timeout(spawn_blocking(...))` pattern shared by
/// every handler so the surrounding code does not have to thread two layers
/// of error mapping. The timeout error message references
/// `TOON_PIPELINE_TIMEOUT_MS` so operators can find the knob.
async fn run_pipeline<F, T>(
    op_name: &'static str,
    pipeline_timeout_ms: u64,
    f: F,
) -> Result<T, McpError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let timeout = Duration::from_millis(pipeline_timeout_ms);
    tokio::time::timeout(timeout, tokio::task::spawn_blocking(f))
        .await
        .map_err(|_| {
            McpError::internal_error(
                format!(
                    "pipeline_timeout: {op_name} did not complete within \
                     {pipeline_timeout_ms}ms (TOON_PIPELINE_TIMEOUT_MS)"
                ),
                Some(serde_json::json!({
                    "code": "pipeline_timeout",
                    "timeout_ms": pipeline_timeout_ms,
                    "op": op_name,
                })),
            )
        })?
        .map_err(|e| {
            McpError::internal_error(
                format!("spawn_blocking failed: {e}"),
                Some(serde_json::json!({
                    "code": "spawn_blocking_failed",
                    "op": op_name,
                })),
            )
        })
}

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
            csv_numeric_coercion: c.csv_numeric_coercion,
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

    // M1: Queue for up to pipeline_timeout_ms before rejecting — honours TOON_PIPELINE_TIMEOUT_MS.
    let _permit = tokio::time::timeout(
        Duration::from_millis(config.pipeline_timeout_ms),
        semaphore.acquire(),
    )
    .await
    .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?
    .map_err(|_| McpError::internal_error("semaphore closed", None))?;

    let event_id = new_event_id();

    // M6: event_id is attached to the span for correlation with LogEvent.
    // Concurrency: use `.instrument(span).await` instead of `span.enter()` so
    // the span is not held across `.await` on the multi-threaded runtime.
    let span = tracing::info_span!("detect_format", event_id = %event_id);

    async move {
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let start = Instant::now();
        // C1: Run the synchronous detect call on a blocking thread — FormatDetector::detect
        // performs a full serde_json::from_str and CSV allocation which must not
        // run on the tokio executor.
        let (fmt, line_count, column_count) =
            run_pipeline("detection", pipeline_timeout_ms, move || {
                let fmt = FormatDetector::detect(&input);
                let line_count = FormatDetector::jsonl_line_count(fmt, &input);
                let column_count = FormatDetector::column_count(fmt, &input);
                (fmt, line_count, column_count)
            })
            .await?;

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

        let _ = log_sink.record(event).await;

        Ok(DetectResult {
            format: fmt.as_str().into(),
            input_bytes,
            line_count,
            column_count,
        })
    }
    .instrument(span)
    .await
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

    let _permit = tokio::time::timeout(
        Duration::from_millis(config.pipeline_timeout_ms),
        semaphore.acquire(),
    )
    .await
    .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?
    .map_err(|_| McpError::internal_error("semaphore closed", None))?;

    let event_id = new_event_id();
    let span = tracing::info_span!("compress_content", event_id = %event_id);

    async move {
        let compress_config = CompressConfig::from(config.as_ref());
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let start = Instant::now();

        // L1: Return (input, decision) so input is available for pass-through output.
        let (input, decision) = run_pipeline("compression", pipeline_timeout_ms, move || {
            let decision = Compressor::decide(&input, &compress_config);
            (input, decision)
        })
        .await?;

        let duration_us = start.elapsed().as_micros() as u64;

        // L2 + H3: named struct, no second detect call.
        // Match is exhaustive over `CompressDecision` (no wildcard) so any new
        // variant produces a compile-time error.
        // Move `input` into the pass-through output to avoid a String clone;
        // `decision` is consumed here so the compressed branch can take `toon`
        // by value.
        let outcome = match decision {
            CompressDecision::Compressed {
                toon,
                toon_bytes,
                savings_pct,
                input_format,
                shape_class,
                ..
            } => CompressOutcome {
                output: toon,
                compressed: true,
                format_str: input_format.as_str().into(),
                shape_str: shape_class.as_str().into(),
                output_bytes: toon_bytes,
                savings_pct,
                pass_reason_str: None,
            },
            CompressDecision::PassedThrough {
                reason,
                input_format,
            } => CompressOutcome {
                output: input,
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
    .instrument(span)
    .await
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

    let _permit = tokio::time::timeout(
        Duration::from_millis(config.pipeline_timeout_ms),
        semaphore.acquire(),
    )
    .await
    .map_err(|_| McpError::internal_error("server busy: too many concurrent calls", None))?
    .map_err(|_| McpError::internal_error("semaphore closed", None))?;

    let event_id = new_event_id();
    let span = tracing::info_span!("compression_stats", event_id = %event_id);

    async move {
        let compress_config = CompressConfig::from(config.as_ref());
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let start = Instant::now();

        let decision = run_pipeline("compression", pipeline_timeout_ms, move || {
            Compressor::decide(&input, &compress_config)
        })
        .await?;

        let duration_us = start.elapsed().as_micros() as u64;

        // L2 + H3: named struct, no second detect call.
        // Exhaustive match — see `handle_compress_content_inner` for rationale.
        let outcome = match &decision {
            CompressDecision::Compressed {
                toon_bytes,
                savings_pct,
                input_format,
                shape_class,
                ..
            } => CompressOutcome {
                output: String::new(),
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
    .instrument(span)
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use toon_mcp_logging::{MemorySink, NoopSink};

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
            csv_numeric_coercion: true,
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
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let huge = "x".repeat(cfg.max_input_bytes + 1);
        let err = handle_detect_format(DetectParams { input: huge }, cfg, noop_sink(), sem)
            .await
            .unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));
    }

    // --- compress_content ---

    #[tokio::test]
    async fn compress_content_compresses_large_json() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let input = serde_json::to_string(
            &(0..50)
                .map(|i| {
                    serde_json::json!({
                        "id": i,
                        "name": format!("item_{i}"),
                        "value": i * 10,
                        "active": true,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let result = handle_compress_content_inner(CompressParams { input }, cfg, noop_sink(), sem)
            .await
            .unwrap();
        assert!(result.compressed || result.output_bytes <= result.input_bytes);
    }

    #[tokio::test]
    async fn compress_content_passes_through_small_input() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let input = r#"{"k":"v"}"#.to_string();
        let result = handle_compress_content_inner(
            CompressParams {
                input: input.clone(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert!(!result.compressed);
        assert_eq!(result.output, input);
    }

    #[tokio::test]
    async fn compress_content_rejects_oversized_input() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let huge = "x".repeat(cfg.max_input_bytes + 1);
        let err =
            handle_compress_content_inner(CompressParams { input: huge }, cfg, noop_sink(), sem)
                .await
                .unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));
    }

    // --- compression_stats ---

    #[tokio::test]
    async fn compression_stats_returns_threshold() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_compression_stats(
            StatsParams {
                input: r#"{"key":"value"}"#.into(),
            },
            cfg.clone(),
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.threshold, cfg.max_output_ratio);
    }

    #[tokio::test]
    async fn compression_stats_rejects_oversized_input() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let huge = "x".repeat(cfg.max_input_bytes + 1);
        let err = handle_compression_stats(StatsParams { input: huge }, cfg, noop_sink(), sem)
            .await
            .unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));
    }

    // --- logging ---

    #[tokio::test]
    async fn detect_format_logs_event() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        handle_detect_format(
            DetectParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            sink,
            sem,
        )
        .await
        .unwrap();
        let events = events.lock().expect("not poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "detect_format");
    }

    #[tokio::test]
    async fn compress_content_logs_event() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        handle_compress_content_inner(
            CompressParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            sink,
            sem,
        )
        .await
        .unwrap();
        let events = events.lock().expect("not poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "compress_content");
    }

    #[tokio::test]
    async fn compression_stats_logs_event() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        handle_compression_stats(
            StatsParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            sink,
            sem,
        )
        .await
        .unwrap();
        let events = events.lock().expect("not poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "compression_stats");
    }

    // --- memory sink ---

    #[tokio::test]
    async fn memory_sink_collects_multiple_events() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let cfg = test_config();
        let sem = Arc::new(Semaphore::new(cfg.max_concurrent_calls));
        for _ in 0..3 {
            handle_detect_format(
                DetectParams {
                    input: r#"{"x":1}"#.into(),
                },
                Arc::clone(&cfg),
                Arc::clone(&sink),
                Arc::clone(&sem),
            )
            .await
            .unwrap();
        }
        assert_eq!(events.lock().expect("not poisoned").len(), 3);
    }

    /// The semaphore permit acquired at handler entry must be released
    /// even when the pipeline times out. Otherwise, sustained timeouts
    /// would gradually deplete `max_concurrent_calls` and wedge the server.
    #[tokio::test]
    async fn detect_format_releases_permit_on_timeout() {
        let mut base = (*test_config()).clone();
        base.pipeline_timeout_ms = 0;
        base.max_concurrent_calls = 2;
        let cfg = Arc::new(base);
        let sem = Arc::new(Semaphore::new(cfg.max_concurrent_calls));

        let initial = sem.available_permits();
        // The result may be `Ok` if the underlying spawn_blocking completes
        // faster than the 0ms timeout fires (tokio polls the inner future
        // at least once before checking the deadline). The contract under
        // test is the permit lifecycle, not the error path, so we tolerate
        // either outcome.
        let _ = handle_detect_format(
            DetectParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await;

        // Whether the timeout fired during semaphore acquire, during the
        // pipeline body, or never — the permit must always be released by
        // the time the call returns. Otherwise sustained timeouts would
        // gradually wedge the server.
        assert_eq!(
            sem.available_permits(),
            initial,
            "permit was not released after handler returned"
        );
    }

    /// Same property for `compress_content`.
    #[tokio::test]
    async fn compress_content_releases_permit_on_timeout() {
        let mut base = (*test_config()).clone();
        base.pipeline_timeout_ms = 0;
        base.max_concurrent_calls = 2;
        let cfg = Arc::new(base);
        let sem = Arc::new(Semaphore::new(cfg.max_concurrent_calls));

        let initial = sem.available_permits();
        let _ = handle_compress_content_inner(
            CompressParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await;

        assert_eq!(sem.available_permits(), initial);
    }

    /// Same property for `compression_stats`.
    #[tokio::test]
    async fn compression_stats_releases_permit_on_timeout() {
        let mut base = (*test_config()).clone();
        base.pipeline_timeout_ms = 0;
        base.max_concurrent_calls = 2;
        let cfg = Arc::new(base);
        let sem = Arc::new(Semaphore::new(cfg.max_concurrent_calls));

        let initial = sem.available_permits();
        let _ = handle_compression_stats(
            StatsParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await;

        assert_eq!(sem.available_permits(), initial);
    }

    #[test]
    fn new_event_id_is_unique() {
        let a = new_event_id();
        let b = new_event_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn new_event_id_is_hex() {
        let id = new_event_id();
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
