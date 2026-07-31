// file: crates/toon-mcp-server/src/handler.rs
// description: Tool handler implementations for detect_format, compress_content, compression_stats

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::ErrorData as McpError;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{Instrument, warn};
use xxhash_rust::xxh3::xxh3_64;

use toon_mcp_core::{
    CompressConfig, CompressDecision, Compressor, DetectionMetadata, FormatDetector, InputFormat,
    parser::csv::{CsvCoercionMetadata, CsvParser},
};
use toon_mcp_logging::{LogEvent, LogSink};

use crate::config::Config;

/// Schema helper: emit `{ "type": "integer" }` without a Go-style `"format"`
/// annotation (`"uint"`, `"uint64"`, etc.) that AJV does not recognise.
fn schema_as_integer(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "integer" })
}

// Process-lifetime diagnostic counters. All use `Relaxed` ordering because
// they are independent monotonic tallies with no cross-counter invariants.
// Overflow wraps silently and is not a correctness concern: at one event per
// nanosecond a u64 still takes ~585 years to wrap, and these feed gauges, not
// control flow.
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);
static HANDLER_LOG_RECORD_FAILED_COUNT: AtomicU64 = AtomicU64::new(0);
static HANDLER_LOG_RECORD_DROPPED_COUNT: AtomicU64 = AtomicU64::new(0);
static PIPELINE_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);
static BLOCKING_TASKS_ABANDONED: AtomicU64 = AtomicU64::new(0);
static REQUEST_FAILED_COUNT: AtomicU64 = AtomicU64::new(0);
static INPUT_REJECTED_COUNT: AtomicU64 = AtomicU64::new(0);
static SERVER_BUSY_COUNT: AtomicU64 = AtomicU64::new(0);
static REQUEST_SUCCEEDED_COUNT: AtomicU64 = AtomicU64::new(0);
static REQUEST_DURATION_US_TOTAL: AtomicU64 = AtomicU64::new(0);
static REQUEST_DURATION_US_MAX: AtomicU64 = AtomicU64::new(0);

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

fn record_success_duration(duration_us: u64) {
    REQUEST_SUCCEEDED_COUNT.fetch_add(1, Ordering::Relaxed);
    REQUEST_DURATION_US_TOTAL.fetch_add(duration_us, Ordering::Relaxed);
    let mut current = REQUEST_DURATION_US_MAX.load(Ordering::Relaxed);
    while duration_us > current {
        match REQUEST_DURATION_US_MAX.compare_exchange_weak(
            current,
            duration_us,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

async fn record_log_event(log_sink: &Arc<dyn LogSink>, event: LogEvent) {
    let event_id = event.event_id.clone();
    let tool_name = event.tool_name.clone();
    let before = log_sink.diagnostics();

    match log_sink.record(event).await {
        Ok(()) => {
            let after = log_sink.diagnostics();
            let dropped = after
                .record_dropped_count
                .saturating_sub(before.record_dropped_count);
            if dropped > 0 {
                HANDLER_LOG_RECORD_DROPPED_COUNT.fetch_add(dropped, Ordering::Relaxed);
                warn!(
                    component = "handler_logging",
                    tool_name,
                    event_id,
                    dropped,
                    total_dropped = after.record_dropped_count,
                    "log event dropped by logging sink"
                );
            }
        }
        Err(err) => {
            HANDLER_LOG_RECORD_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
            warn!(
                component = "handler_logging",
                tool_name,
                event_id,
                error = %err,
                "log event failed; preserving successful tool response"
            );
        }
    }
}

/// Classify a `run_pipeline` error into a `LogEvent` outcome string.
fn error_outcome(err: &McpError) -> &'static str {
    let is_timeout = err
        .data
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        == Some("pipeline_timeout");
    if is_timeout { "timeout" } else { "failed" }
}

/// Increment the failure counters, emit a failure `LogEvent`, and hand the
/// error back for the handler to propagate.
///
/// Failure events use `"unknown"` / pass-through placeholders for fields the
/// pipeline never produced (the input was rejected, timed out, or could not
/// be processed).
async fn record_failure(
    err: McpError,
    outcome: &'static str,
    tool_name: &'static str,
    input_bytes: usize,
    duration_us: u64,
    config: &Config,
    log_sink: &Arc<dyn LogSink>,
) -> McpError {
    REQUEST_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
    match outcome {
        "rejected" => {
            INPUT_REJECTED_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        "busy" => {
            SERVER_BUSY_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }

    let event = LogEvent {
        event_id: new_event_id(),
        ts_us: jiff::Timestamp::now().as_microsecond(),
        tool_name: tool_name.into(),
        input_format: InputFormat::Unknown.as_str().into(),
        shape_class: toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
        input_bytes: input_bytes as u64,
        output_bytes: input_bytes as u64,
        compressed: false,
        savings_pct: 0.0,
        threshold_used: config.max_output_ratio,
        duration_us,
        outcome: outcome.into(),
        pass_reason: None,
        client_hint: config.client_hint.clone(),
    };
    record_log_event(log_sink, event).await;
    err
}

/// Reject inputs above `TOON_MAX_INPUT_BYTES`, recording the failure.
/// Returns `None` when the input is within bounds.
async fn reject_oversized(
    tool_name: &'static str,
    input_bytes: usize,
    config: &Config,
    log_sink: &Arc<dyn LogSink>,
) -> Option<McpError> {
    if input_bytes <= config.max_input_bytes {
        return None;
    }
    let err = McpError::invalid_params(
        format!(
            "input_exceeds_limit: input is {input_bytes} bytes; \
             maximum allowed is {} bytes (TOON_MAX_INPUT_BYTES)",
            config.max_input_bytes
        ),
        None,
    );
    Some(record_failure(err, "rejected", tool_name, input_bytes, 0, config, log_sink).await)
}

/// Queue for up to `TOON_PIPELINE_TIMEOUT_MS` for an owned concurrency
/// permit, recording a busy failure when the deadline expires.
///
/// The owned permit is later moved into `run_pipeline`, which holds it until
/// the blocking task resolves — including past a timeout — so abandoned work
/// keeps counting against `max_concurrent_calls`.
async fn acquire_permit(
    tool_name: &'static str,
    input_bytes: usize,
    config: &Config,
    log_sink: &Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, McpError> {
    match tokio::time::timeout(
        Duration::from_millis(config.pipeline_timeout_ms),
        semaphore.acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(record_failure(
            McpError::internal_error("semaphore closed", None),
            "failed",
            tool_name,
            input_bytes,
            0,
            config,
            log_sink,
        )
        .await),
        Err(_) => Err(record_failure(
            McpError::internal_error(
                "server busy: too many concurrent calls",
                Some(serde_json::json!({ "code": "server_busy" })),
            ),
            "busy",
            tool_name,
            input_bytes,
            0,
            config,
            log_sink,
        )
        .await),
    }
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
///
/// The caller's concurrency permit is passed in and held until the blocking
/// task actually resolves — not merely until the timeout fires. A blocking
/// task cannot be cancelled, so on timeout the error is returned to the
/// caller while the abandoned task keeps its permit occupied; releasing it
/// early would let timed-out work stack up beyond `max_concurrent_calls`.
async fn run_pipeline<F, T>(
    op_name: &'static str,
    pipeline_timeout_ms: u64,
    permit: OwnedSemaphorePermit,
    f: F,
) -> Result<T, McpError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let timeout = Duration::from_millis(pipeline_timeout_ms);
    let mut handle = tokio::task::spawn_blocking(f);
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(join_result) => {
            drop(permit);
            join_result.map_err(|e| {
                McpError::internal_error(
                    format!("spawn_blocking failed: {e}"),
                    Some(serde_json::json!({
                        "code": "spawn_blocking_failed",
                        "op": op_name,
                    })),
                )
            })
        }
        Err(_) => {
            PIPELINE_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
            BLOCKING_TASKS_ABANDONED.fetch_add(1, Ordering::Relaxed);
            // The abandoned task still runs on the blocking pool; a reaper
            // task holds the permit until it completes so its capacity cost
            // stays visible to the semaphore.
            tokio::spawn(async move {
                let _ = handle.await;
                drop(permit);
            });
            Err(McpError::internal_error(
                format!(
                    "pipeline_timeout: {op_name} did not complete within \
                     {pipeline_timeout_ms}ms (TOON_PIPELINE_TIMEOUT_MS)"
                ),
                Some(serde_json::json!({
                    "code": "pipeline_timeout",
                    "timeout_ms": pipeline_timeout_ms,
                    "op": op_name,
                })),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Config → CompressConfig conversion
// ---------------------------------------------------------------------------

impl From<&Config> for CompressConfig {
    fn from(c: &Config) -> Self {
        // `CompressConfig` is `#[non_exhaustive]`, so start from the default and
        // overwrite each field rather than using a struct literal.
        let mut cfg = CompressConfig::default();
        cfg.max_output_ratio = c.max_output_ratio;
        cfg.min_bytes = c.min_bytes;
        cfg.max_input_bytes = c.max_input_bytes;
        cfg.key_folding = c.key_folding;
        cfg.delimiter = c.delimiter;
        cfg.tabular_min_rows = c.tabular_min_rows;
        cfg.fold_min_depth = c.fold_min_depth;
        cfg.primitive_array_min = c.primitive_array_min;
        cfg.csv_numeric_coercion = c.csv_numeric_coercion;
        cfg
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
    detection_metadata: DetectionMetadata,
    numeric_coercion_used: Option<bool>,
    lossy_coercion_possible: Option<bool>,
}

fn detection_candidates_as_strings(metadata: &DetectionMetadata) -> Vec<String> {
    metadata
        .candidates
        .iter()
        .map(|fmt| fmt.as_str().to_owned())
        .collect()
}

/// Compute CSV/TSV coercion metadata for the detect-only path, which does not
/// run the compression pipeline. Returns `None` for non-delimited formats so
/// the caller reports no coercion visibility. Unlike the compression pipeline,
/// this inspects the input without materialising parsed values.
fn detect_coercion_metadata(
    fmt: InputFormat,
    input: &str,
    numeric_coercion: bool,
) -> Option<CsvCoercionMetadata> {
    let parser = match fmt {
        InputFormat::Csv => CsvParser::csv(),
        InputFormat::Tsv => CsvParser::tsv(),
        _ => return None,
    }
    .with_numeric_coercion(numeric_coercion);

    match parser.coercion_metadata(input) {
        Ok(metadata) => Some(metadata),
        Err(err) => {
            warn!(
                %err,
                format = fmt.as_str(),
                "could not compute CSV coercion metadata; reporting coercion visibility as false"
            );
            Some(CsvCoercionMetadata {
                numeric_coercion_used: false,
                lossy_coercion_possible: false,
            })
        }
    }
}

/// Map the CSV/TSV coercion metadata produced by the compression pipeline
/// into the optional booleans the handlers attach to their responses.
///
/// `None` (non-delimited formats, or inputs rejected before parsing) yields
/// `(None, None)`; a delimited parse yields `Some(..)` for each flag.
fn coercion_visibility(coercion: Option<CsvCoercionMetadata>) -> (Option<bool>, Option<bool>) {
    match coercion {
        Some(meta) => (
            Some(meta.numeric_coercion_used),
            Some(meta.lossy_coercion_possible),
        ),
        None => (None, None),
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
    /// Detection confidence: `"certain"` for validated JSON/unknown, or
    /// `"heuristic"` for JSONL/CSV/TSV probes.
    pub detection_confidence: String,
    /// Whether multiple format detectors matched this input.
    pub detection_ambiguous: bool,
    /// All matching formats in detection precedence order.
    pub detection_candidates: Vec<String>,
    /// Whether CSV/TSV numeric coercion would affect at least one field.
    pub numeric_coercion_used: Option<bool>,
    /// Whether CSV/TSV numeric coercion may discard textual intent.
    pub lossy_coercion_possible: Option<bool>,
}

/// Handle the `detect_format` MCP tool.
pub async fn handle_detect_format(
    params: DetectParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<DetectResult, McpError> {
    let tool_name = "detect_format";
    let input = params.input;
    let input_bytes = input.len();

    if let Some(err) = reject_oversized(tool_name, input_bytes, &config, &log_sink).await {
        return Err(err);
    }

    // M1: Queue for up to pipeline_timeout_ms before rejecting — honours TOON_PIPELINE_TIMEOUT_MS.
    let permit = acquire_permit(
        tool_name,
        input_bytes,
        &config,
        &log_sink,
        Arc::clone(&semaphore),
    )
    .await?;

    let event_id = new_event_id();

    // M6: event_id is attached to the span for correlation with LogEvent.
    // Concurrency: use `.instrument(span).await` instead of `span.enter()` so
    // the span is not held across `.await` on the multi-threaded runtime.
    let span = tracing::info_span!("detect_format", event_id = %event_id);

    async move {
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let csv_numeric_coercion = config.csv_numeric_coercion;
        let start = Instant::now();
        // C1: Run the synchronous detect call on a blocking thread — FormatDetector::detect
        // performs a full serde_json::from_str and CSV allocation which must not
        // run on the tokio executor.
        let pipeline_result = run_pipeline("detection", pipeline_timeout_ms, permit, move || {
            let metadata = FormatDetector::detect_with_metadata(&input);
            let fmt = metadata.format;
            let line_count = FormatDetector::jsonl_line_count(fmt, &input);
            let column_count = FormatDetector::column_count(fmt, &input);
            let (numeric_coercion_used, lossy_coercion_possible) =
                coercion_visibility(detect_coercion_metadata(fmt, &input, csv_numeric_coercion));
            (
                metadata,
                line_count,
                column_count,
                numeric_coercion_used,
                lossy_coercion_possible,
            )
        })
        .await;
        let (metadata, line_count, column_count, numeric_coercion_used, lossy_coercion_possible) =
            match pipeline_result {
                Ok(v) => v,
                Err(err) => {
                    let outcome = error_outcome(&err);
                    let duration_us = start.elapsed().as_micros() as u64;
                    return Err(record_failure(
                        err,
                        outcome,
                        tool_name,
                        input_bytes,
                        duration_us,
                        &config,
                        &log_sink,
                    )
                    .await);
                }
            };
        let fmt = metadata.format;

        let duration_us = start.elapsed().as_micros() as u64;

        let event = LogEvent {
            event_id: event_id.clone(),
            ts_us: jiff::Timestamp::now().as_microsecond(),
            tool_name: "detect_format".into(),
            input_format: fmt.as_str().into(),
            shape_class: toon_mcp_core::ShapeClass::PassThrough.as_str().into(),
            input_bytes: input_bytes as u64,
            output_bytes: input_bytes as u64,
            compressed: false,
            savings_pct: 0.0,
            threshold_used: config.max_output_ratio,
            duration_us,
            outcome: "ok".into(),
            pass_reason: None,
            client_hint: config.client_hint.clone(),
        };

        record_success_duration(duration_us);
        record_log_event(&log_sink, event).await;

        Ok(DetectResult {
            format: fmt.as_str().into(),
            input_bytes,
            line_count,
            column_count,
            detection_confidence: metadata.confidence.as_str().into(),
            detection_ambiguous: metadata.ambiguous,
            detection_candidates: detection_candidates_as_strings(&metadata),
            numeric_coercion_used,
            lossy_coercion_possible,
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
    /// Detection confidence for the selected input format.
    pub detection_confidence: String,
    /// Whether multiple format detectors matched this input.
    pub detection_ambiguous: bool,
    /// All matching formats in detection precedence order.
    pub detection_candidates: Vec<String>,
    /// Whether CSV/TSV numeric coercion affected at least one parsed field.
    pub numeric_coercion_used: Option<bool>,
    /// Whether CSV/TSV numeric coercion may have discarded textual intent.
    pub lossy_coercion_possible: Option<bool>,
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
    let tool_name = "compress_content";
    let input = params.input;
    let input_bytes = input.len();

    if let Some(err) = reject_oversized(tool_name, input_bytes, &config, &log_sink).await {
        return Err(err);
    }

    let permit = acquire_permit(
        tool_name,
        input_bytes,
        &config,
        &log_sink,
        Arc::clone(&semaphore),
    )
    .await?;

    let event_id = new_event_id();
    let span = tracing::info_span!("compress_content", event_id = %event_id);

    async move {
        let compress_config = CompressConfig::from(config.as_ref());
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let start = Instant::now();

        // L1: Return (input, decision) so input is available for pass-through output.
        // `decide_with_metadata` runs detection and parsing once, surfacing the
        // detection metadata and CSV/TSV coercion visibility from the same pass.
        let pipeline_result = run_pipeline("compression", pipeline_timeout_ms, permit, move || {
            let result = Compressor::decide_with_metadata(&input, &compress_config);
            let (numeric_coercion_used, lossy_coercion_possible) =
                coercion_visibility(result.coercion);
            (
                input,
                result.decision,
                result.detection,
                numeric_coercion_used,
                lossy_coercion_possible,
            )
        })
        .await;
        let (input, decision, detection_metadata, numeric_coercion_used, lossy_coercion_possible) =
            match pipeline_result {
                Ok(v) => v,
                Err(err) => {
                    let outcome = error_outcome(&err);
                    let duration_us = start.elapsed().as_micros() as u64;
                    return Err(record_failure(
                        err,
                        outcome,
                        tool_name,
                        input_bytes,
                        duration_us,
                        &config,
                        &log_sink,
                    )
                    .await);
                }
            };

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
                detection_metadata,
                numeric_coercion_used,
                lossy_coercion_possible,
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
                detection_metadata,
                numeric_coercion_used,
                lossy_coercion_possible,
            },
        };

        let detection_candidates = detection_candidates_as_strings(&outcome.detection_metadata);

        let event = LogEvent {
            event_id: event_id.clone(),
            ts_us: jiff::Timestamp::now().as_microsecond(),
            tool_name: "compress_content".into(),
            input_format: outcome.format_str.clone(),
            shape_class: outcome.shape_str.clone(),
            input_bytes: input_bytes as u64,
            output_bytes: outcome.output_bytes as u64,
            compressed: outcome.compressed,
            savings_pct: outcome.savings_pct,
            threshold_used: config.max_output_ratio,
            duration_us,
            outcome: "ok".into(),
            pass_reason: outcome.pass_reason_str.clone(),
            client_hint: config.client_hint.clone(),
        };

        record_success_duration(duration_us);
        record_log_event(&log_sink, event).await;

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
            detection_confidence: outcome.detection_metadata.confidence.as_str().into(),
            detection_ambiguous: outcome.detection_metadata.ambiguous,
            detection_candidates,
            numeric_coercion_used: outcome.numeric_coercion_used,
            lossy_coercion_possible: outcome.lossy_coercion_possible,
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
    /// Detection confidence for the selected input format.
    pub detection_confidence: String,
    /// Whether multiple format detectors matched this input.
    pub detection_ambiguous: bool,
    /// All matching formats in detection precedence order.
    pub detection_candidates: Vec<String>,
    /// Whether CSV/TSV numeric coercion would affect at least one parsed field.
    pub numeric_coercion_used: Option<bool>,
    /// Whether CSV/TSV numeric coercion may discard textual intent.
    pub lossy_coercion_possible: Option<bool>,
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
    let tool_name = "compression_stats";
    let input = params.input;
    let input_bytes = input.len();

    if let Some(err) = reject_oversized(tool_name, input_bytes, &config, &log_sink).await {
        return Err(err);
    }

    let permit = acquire_permit(
        tool_name,
        input_bytes,
        &config,
        &log_sink,
        Arc::clone(&semaphore),
    )
    .await?;

    let event_id = new_event_id();
    let span = tracing::info_span!("compression_stats", event_id = %event_id);

    async move {
        let compress_config = CompressConfig::from(config.as_ref());
        let pipeline_timeout_ms = config.pipeline_timeout_ms;
        let start = Instant::now();

        let pipeline_result = run_pipeline("compression", pipeline_timeout_ms, permit, move || {
            let result = Compressor::decide_with_metadata(&input, &compress_config);
            let (numeric_coercion_used, lossy_coercion_possible) =
                coercion_visibility(result.coercion);
            (
                result.decision,
                result.detection,
                numeric_coercion_used,
                lossy_coercion_possible,
            )
        })
        .await;
        let (decision, detection_metadata, numeric_coercion_used, lossy_coercion_possible) =
            match pipeline_result {
                Ok(v) => v,
                Err(err) => {
                    let outcome = error_outcome(&err);
                    let duration_us = start.elapsed().as_micros() as u64;
                    return Err(record_failure(
                        err,
                        outcome,
                        tool_name,
                        input_bytes,
                        duration_us,
                        &config,
                        &log_sink,
                    )
                    .await);
                }
            };

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
                detection_metadata,
                numeric_coercion_used,
                lossy_coercion_possible,
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
                detection_metadata,
                numeric_coercion_used,
                lossy_coercion_possible,
            },
        };

        let detection_candidates = detection_candidates_as_strings(&outcome.detection_metadata);

        let event = LogEvent {
            event_id: event_id.clone(),
            ts_us: jiff::Timestamp::now().as_microsecond(),
            tool_name: "compression_stats".into(),
            input_format: outcome.format_str.clone(),
            shape_class: outcome.shape_str.clone(),
            input_bytes: input_bytes as u64,
            output_bytes: outcome.output_bytes as u64,
            compressed: outcome.compressed,
            savings_pct: outcome.savings_pct,
            threshold_used: config.max_output_ratio,
            duration_us,
            outcome: "ok".into(),
            pass_reason: outcome.pass_reason_str.clone(),
            client_hint: config.client_hint.clone(),
        };

        record_success_duration(duration_us);
        record_log_event(&log_sink, event).await;

        Ok(StatsResult {
            would_compress: outcome.compressed,
            format: outcome.format_str,
            shape_class: outcome.shape_str,
            input_bytes,
            estimated_output_bytes: outcome.output_bytes,
            estimated_savings_pct: outcome.savings_pct,
            threshold: config.max_output_ratio,
            pass_reason: outcome.pass_reason_str,
            detection_confidence: outcome.detection_metadata.confidence.as_str().into(),
            detection_ambiguous: outcome.detection_metadata.ambiguous,
            detection_candidates,
            numeric_coercion_used: outcome.numeric_coercion_used,
            lossy_coercion_possible: outcome.lossy_coercion_possible,
        })
    }
    .instrument(span)
    .await
}

// ---------------------------------------------------------------------------
// toon_diagnostics
// ---------------------------------------------------------------------------

/// Input parameters for `toon_diagnostics`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiagnosticsParams {}

/// Logging diagnostics returned by `toon_diagnostics`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LoggingDiagnosticsResult {
    /// Number of events dropped because the bounded writer queue was full.
    #[schemars(schema_with = "schema_as_integer")]
    pub record_dropped_count: u64,
    /// Number of record attempts that failed because the writer channel closed.
    #[schemars(schema_with = "schema_as_integer")]
    pub record_failed_count: u64,
    /// Number of events skipped because JSON serialization failed.
    #[schemars(schema_with = "schema_as_integer")]
    pub serialization_failed_count: u64,
    /// Number of writer flush attempts that failed after accepting events.
    #[schemars(schema_with = "schema_as_integer")]
    pub writer_failed_count: u64,
    /// Last writer, queue, or channel error observed by the sink.
    pub last_error: Option<String>,
    /// Total bounded queue capacity, if applicable.
    #[schemars(schema_with = "schema_as_integer")]
    pub queue_capacity: Option<usize>,
    /// Currently queued commands, if applicable.
    #[schemars(schema_with = "schema_as_integer")]
    pub queue_queued: Option<usize>,
    /// Currently available queue slots, if applicable.
    #[schemars(schema_with = "schema_as_integer")]
    pub queue_available: Option<usize>,
}

/// Handler-level diagnostics returned by `toon_diagnostics`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HandlerDiagnosticsResult {
    /// Number of logging record failures observed and downgraded by handlers.
    #[schemars(schema_with = "schema_as_integer")]
    pub log_record_failed_count: u64,
    /// Number of logging drops observed and downgraded by handlers.
    #[schemars(schema_with = "schema_as_integer")]
    pub log_record_dropped_count: u64,
    /// Number of pipeline timeout responses returned by handlers.
    #[schemars(schema_with = "schema_as_integer")]
    pub pipeline_timeout_count: u64,
    /// Number of blocking pipeline tasks abandoned after a timeout. Each
    /// abandoned task keeps its concurrency permit until it completes.
    #[schemars(schema_with = "schema_as_integer")]
    pub blocking_tasks_abandoned: u64,
    /// Number of tool requests that returned an error (any failure kind).
    #[schemars(schema_with = "schema_as_integer")]
    pub request_failed_count: u64,
    /// Number of inputs rejected before processing (over the size limit).
    #[schemars(schema_with = "schema_as_integer")]
    pub input_rejected_count: u64,
    /// Number of requests rejected because no concurrency permit became
    /// available within the queue deadline.
    #[schemars(schema_with = "schema_as_integer")]
    pub server_busy_count: u64,
    /// Number of successful tool requests included in duration gauges.
    #[schemars(schema_with = "schema_as_integer")]
    pub request_succeeded_count: u64,
    /// Sum of successful request durations in microseconds.
    #[schemars(schema_with = "schema_as_integer")]
    pub request_duration_us_total: u64,
    /// Maximum successful request duration in microseconds.
    #[schemars(schema_with = "schema_as_integer")]
    pub request_duration_us_max: u64,
    /// Average successful request duration in microseconds.
    pub request_duration_us_avg: f64,
}

/// Output from `toon_diagnostics`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DiagnosticsResult {
    /// Whether structured logging is configured as enabled.
    pub logging_enabled: bool,
    /// Runtime logging sink diagnostics.
    pub logging: LoggingDiagnosticsResult,
    /// Handler-level counters and request-duration gauges.
    pub handler: HandlerDiagnosticsResult,
    /// Current available concurrency permits.
    #[schemars(schema_with = "schema_as_integer")]
    pub semaphore_available_permits: usize,
    /// Configured maximum concurrent calls.
    #[schemars(schema_with = "schema_as_integer")]
    pub max_concurrent_calls: usize,
}

/// Handle the `toon_diagnostics` MCP tool.
pub async fn handle_toon_diagnostics(
    _params: DiagnosticsParams,
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    semaphore: Arc<Semaphore>,
) -> Result<DiagnosticsResult, McpError> {
    let log = log_sink.diagnostics();
    let succeeded = REQUEST_SUCCEEDED_COUNT.load(Ordering::Relaxed);
    let duration_total = REQUEST_DURATION_US_TOTAL.load(Ordering::Relaxed);
    let duration_avg = if succeeded == 0 {
        0.0
    } else {
        duration_total as f64 / succeeded as f64
    };

    Ok(DiagnosticsResult {
        logging_enabled: config.logging_enabled,
        logging: LoggingDiagnosticsResult {
            record_dropped_count: log.record_dropped_count,
            record_failed_count: log.record_failed_count,
            serialization_failed_count: log.serialization_failed_count,
            writer_failed_count: log.writer_failed_count,
            last_error: log.last_error,
            queue_capacity: log.queue_capacity,
            queue_queued: log.queue_queued,
            queue_available: log.queue_available,
        },
        handler: HandlerDiagnosticsResult {
            log_record_failed_count: HANDLER_LOG_RECORD_FAILED_COUNT.load(Ordering::Relaxed),
            log_record_dropped_count: HANDLER_LOG_RECORD_DROPPED_COUNT.load(Ordering::Relaxed),
            pipeline_timeout_count: PIPELINE_TIMEOUT_COUNT.load(Ordering::Relaxed),
            blocking_tasks_abandoned: BLOCKING_TASKS_ABANDONED.load(Ordering::Relaxed),
            request_failed_count: REQUEST_FAILED_COUNT.load(Ordering::Relaxed),
            input_rejected_count: INPUT_REJECTED_COUNT.load(Ordering::Relaxed),
            server_busy_count: SERVER_BUSY_COUNT.load(Ordering::Relaxed),
            request_succeeded_count: succeeded,
            request_duration_us_total: duration_total,
            request_duration_us_max: REQUEST_DURATION_US_MAX.load(Ordering::Relaxed),
            request_duration_us_avg: duration_avg,
        },
        semaphore_available_permits: semaphore.available_permits(),
        max_concurrent_calls: config.max_concurrent_calls,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use toon_mcp_logging::{JsonlSink, JsonlSinkConfig, MemorySink, NoopSink};

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
            strict_config: false,
        })
    }

    fn noop_sink() -> Arc<dyn LogSink> {
        Arc::new(NoopSink)
    }

    fn test_semaphore(config: &Config) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(config.max_concurrent_calls))
    }

    fn schema_has_property<T: JsonSchema>(property: &str) -> bool {
        let schema = schemars::schema_for!(T);
        let value = serde_json::to_value(schema).expect("schema must serialize to JSON");
        value
            .get("properties")
            .and_then(|properties| properties.get(property))
            .is_some()
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
        assert_eq!(result.detection_confidence, "certain");
        assert!(!result.detection_ambiguous);
        assert_eq!(result.detection_candidates, vec!["json".to_owned()]);
        assert_eq!(result.numeric_coercion_used, None);
        assert_eq!(result.lossy_coercion_possible, None);
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
        assert_eq!(result.detection_confidence, "heuristic");
        assert_eq!(result.detection_candidates, vec!["csv".to_owned()]);
        assert_eq!(result.numeric_coercion_used, Some(true));
        assert_eq!(result.lossy_coercion_possible, Some(false));
    }

    #[tokio::test]
    async fn detect_format_csv_flags_lossy_numeric_coercion() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let result = handle_detect_format(
            DetectParams {
                input: "zip,count\n00123,1.0".into(),
            },
            cfg,
            noop_sink(),
            sem,
        )
        .await
        .unwrap();
        assert_eq!(result.format, "csv");
        assert_eq!(result.numeric_coercion_used, Some(true));
        assert_eq!(result.lossy_coercion_possible, Some(true));
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
        assert!(!result.detection_confidence.is_empty());
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
        assert_eq!(result.numeric_coercion_used, None);
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
        assert!(!result.detection_confidence.is_empty());
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
        assert_eq!(events[0].outcome, "ok");
    }

    #[tokio::test]
    async fn oversized_input_records_rejected_failure_event() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let mut base = (*test_config()).clone();
        base.max_input_bytes = 8;
        let cfg = Arc::new(base);
        let sem = test_semaphore(&cfg);

        let err = handle_detect_format(
            DetectParams {
                input: "x".repeat(9),
            },
            cfg,
            Arc::clone(&sink),
            sem,
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("input_exceeds_limit"));

        let events = events.lock().expect("not poisoned");
        assert_eq!(events.len(), 1, "rejection must emit a failure event");
        assert_eq!(events[0].tool_name, "detect_format");
        assert_eq!(events[0].outcome, "rejected");
        assert!(!events[0].compressed);
    }

    #[tokio::test]
    async fn busy_rejection_records_busy_failure_event() {
        let (memory_sink, events) = MemorySink::new();
        let sink: Arc<dyn LogSink> = Arc::new(memory_sink);
        let mut base = (*test_config()).clone();
        // Short queue deadline so the held permit forces a busy rejection.
        base.pipeline_timeout_ms = 50;
        base.max_concurrent_calls = 1;
        let cfg = Arc::new(base);
        let sem = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&sem).acquire_owned().await.expect("permit");

        let err = handle_compression_stats(
            StatsParams {
                input: r#"{"a":1}"#.into(),
            },
            cfg,
            Arc::clone(&sink),
            Arc::clone(&sem),
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("server busy"));
        drop(held);

        let events = events.lock().expect("not poisoned");
        assert_eq!(events.len(), 1, "busy rejection must emit a failure event");
        assert_eq!(events[0].tool_name, "compression_stats");
        assert_eq!(events[0].outcome, "busy");
    }

    #[tokio::test]
    async fn failure_counters_increment_on_rejection() {
        let mut base = (*test_config()).clone();
        base.max_input_bytes = 8;
        let cfg = Arc::new(base);
        let sem = test_semaphore(&cfg);

        let before = handle_toon_diagnostics(
            DiagnosticsParams {},
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await
        .expect("diagnostics succeeds");

        let _ = handle_compress_content_inner(
            CompressParams {
                input: "x".repeat(9),
            },
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await
        .unwrap_err();

        let after = handle_toon_diagnostics(DiagnosticsParams {}, cfg, noop_sink(), sem)
            .await
            .expect("diagnostics succeeds");
        // Counters are process-global until the Metrics refactor lands, so
        // assert deltas with >= (other tests may run concurrently).
        assert!(after.handler.request_failed_count > before.handler.request_failed_count);
        assert!(after.handler.input_rejected_count > before.handler.input_rejected_count);
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

    #[tokio::test]
    async fn handler_preserves_success_when_logging_channel_closed() {
        let dir = std::env::temp_dir().join(format!("toon-mcp-server-test-{}", new_event_id()));
        let config = JsonlSinkConfig {
            log_dir: dir.clone(),
            buffer_size: 4,
            flush_interval: Duration::from_secs(3600),
        };
        let (jsonl_sink, task) = JsonlSink::new(config).expect("JsonlSink constructs");
        drop(task);
        let sink: Arc<dyn LogSink> = Arc::new(jsonl_sink);
        let cfg = test_config();
        let sem = test_semaphore(&cfg);

        let result = handle_detect_format(
            DetectParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            Arc::clone(&sink),
            sem,
        )
        .await
        .expect("tool call succeeds despite logging failure");

        assert_eq!(result.format, "json");
        let diagnostics =
            handle_toon_diagnostics(DiagnosticsParams {}, cfg, sink, Arc::new(Semaphore::new(1)))
                .await
                .expect("diagnostics succeeds");
        assert_eq!(diagnostics.logging.record_failed_count, 1);
        assert!(diagnostics.handler.log_record_failed_count >= 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn diagnostics_reports_request_durations() {
        let cfg = test_config();
        let sem = test_semaphore(&cfg);
        let before = handle_toon_diagnostics(
            DiagnosticsParams {},
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await
        .expect("diagnostics succeeds");

        handle_detect_format(
            DetectParams {
                input: r#"{"a":1}"#.into(),
            },
            Arc::clone(&cfg),
            noop_sink(),
            Arc::clone(&sem),
        )
        .await
        .expect("tool call succeeds");

        let after = handle_toon_diagnostics(DiagnosticsParams {}, cfg, noop_sink(), sem)
            .await
            .expect("diagnostics succeeds");
        assert!(
            after.handler.request_succeeded_count > before.handler.request_succeeded_count,
            "request counter increments"
        );
        assert!(
            after.handler.request_duration_us_total >= before.handler.request_duration_us_total
        );
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

    /// Poll until the semaphore recovers `expected` permits, failing after a
    /// bounded wait. On timeout the permit is released only once the
    /// abandoned blocking task completes, which may lag the handler's return.
    async fn wait_for_permits(sem: &Semaphore, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while sem.available_permits() != expected {
            assert!(
                Instant::now() < deadline,
                "permit not released within 2 s (available: {}, expected: {expected})",
                sem.available_permits()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The semaphore permit acquired at handler entry must be released once
    /// the (possibly abandoned) blocking task completes — even when the
    /// pipeline times out. Otherwise, sustained timeouts would gradually
    /// deplete `max_concurrent_calls` and wedge the server.
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
        // pipeline body, or never — the permit must be released once the
        // abandoned blocking task completes. The invariant is "released on
        // task completion", not "released on handler return", so poll with
        // a bound instead of asserting immediately.
        wait_for_permits(&sem, initial).await;
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

        wait_for_permits(&sem, initial).await;
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

        wait_for_permits(&sem, initial).await;
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

    #[test]
    fn result_schemas_include_semantics_metadata() {
        for property in [
            "detection_confidence",
            "detection_ambiguous",
            "detection_candidates",
            "numeric_coercion_used",
            "lossy_coercion_possible",
        ] {
            assert!(
                schema_has_property::<DetectResult>(property),
                "DetectResult schema missing {property}"
            );
            assert!(
                schema_has_property::<CompressResult>(property),
                "CompressResult schema missing {property}"
            );
            assert!(
                schema_has_property::<StatsResult>(property),
                "StatsResult schema missing {property}"
            );
        }
    }
}
