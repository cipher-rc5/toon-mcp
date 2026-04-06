// file: crates/toon-mcp-server/src/handler.rs
// description: Tool handler functions and input/output types for the three MCP tools

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema helper: emit `{ "type": "integer" }` without a Go-style `"format"`
/// annotation (`"uint"`, `"uint64"`, etc.) that AJV does not recognise.
fn schema_as_integer(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "integer" })
}

use toon_mcp_core::{
    Classifier, CompressConfig, CompressDecision, Compressor, FormatDetector, InputFormat,
    ShapeClass,
};
use toon_mcp_logging::{LogEvent, LogSink};

use crate::config::Config;

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
) -> DetectResult {
    let start = Instant::now();
    let input = &params.input;
    let input_bytes = input.len();
    let fmt = FormatDetector::detect(input);

    let line_count = match fmt {
        InputFormat::Jsonl => Some(input.lines().filter(|l| !l.trim().is_empty()).count()),
        _ => None,
    };

    let column_count: Option<usize> = match fmt {
        InputFormat::Csv | InputFormat::Tsv => {
            let delim = if fmt == InputFormat::Tsv { b'\t' } else { b',' };
            csv::ReaderBuilder::new()
                .delimiter(delim)
                .from_reader(input.as_bytes())
                .headers()
                .ok()
                .map(|h: &csv::StringRecord| h.len())
        }
        _ => None,
    };

    let duration_us = start.elapsed().as_micros() as u64;

    let event = LogEvent {
        event_id: Uuid::new_v4().to_string(),
        ts_us: Utc::now().timestamp_micros(),
        tool_name: "detect_format".into(),
        input_format: fmt.as_str().into(),
        shape_class: ShapeClass::PassThrough.as_str().into(),
        input_bytes: input_bytes as u64,
        output_bytes: input_bytes as u64,
        compressed: false,
        savings_pct: 0.0,
        threshold_used: config.compression_threshold,
        duration_us,
        pass_reason: None,
        client_hint: config.client_hint.clone(),
    };

    // Fire-and-forget; backpressure is handled by the bounded channel.
    let _ = log_sink.record(event).await;

    DetectResult {
        format: fmt.as_str().into(),
        input_bytes,
        line_count,
        column_count,
    }
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
) -> CompressResult {
    let start = Instant::now();
    let input = params.input;
    let input_bytes = input.len();

    let compress_config = CompressConfig {
        threshold: config.compression_threshold,
        min_bytes: config.min_bytes,
        key_folding: config.key_folding,
        delimiter: config.delimiter,
        tabular_min_rows: config.tabular_min_rows,
        fold_min_depth: config.fold_min_depth,
        primitive_array_min: config.primitive_array_min,
    };

    let decision = Compressor::decide(&input, &compress_config);
    let duration_us = start.elapsed().as_micros() as u64;

    // Derive structured fields for logging and response.
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
            original_bytes: _,
            toon_bytes,
            savings_pct,
        } => {
            // Re-detect format for logging (cheap — value already parsed inside Compressor).
            let fmt: String = FormatDetector::detect(&input).as_str().into();
            // Re-classify the parsed value to get the shape class for logging.
            let shape: String =
                match toon_mcp_core::detector::FormatDetector::detect_and_parse(&input) {
                    Ok((_, val)) => Classifier::classify(&val).as_str().into(),
                    Err(_) => ShapeClass::PassThrough.as_str().into(),
                };
            (
                toon.clone(),
                true,
                fmt,
                shape,
                *toon_bytes,
                *savings_pct,
                None,
            )
        }
        CompressDecision::PassedThrough { reason } => {
            let fmt: String = FormatDetector::detect(&input).as_str().into();
            let reason_str: String = reason.as_str();
            (
                input.clone(),
                false,
                fmt,
                ShapeClass::PassThrough.as_str().into(),
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
        threshold_used: config.compression_threshold,
        duration_us,
        pass_reason: pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    CompressResult {
        output,
        compressed,
        format: format_str,
        shape_class: shape_str,
        input_bytes,
        output_bytes,
        savings_pct,
        duration_us,
        pass_reason: pass_reason_str,
    }
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
    /// Active compression threshold.
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
) -> StatsResult {
    let start = Instant::now();
    let input = params.input;
    let input_bytes = input.len();

    let compress_config = CompressConfig {
        threshold: config.compression_threshold,
        min_bytes: config.min_bytes,
        key_folding: config.key_folding,
        delimiter: config.delimiter,
        tabular_min_rows: config.tabular_min_rows,
        fold_min_depth: config.fold_min_depth,
        primitive_array_min: config.primitive_array_min,
    };

    let decision = Compressor::decide(&input, &compress_config);
    let duration_us = start.elapsed().as_micros() as u64;

    let fmt = FormatDetector::detect(&input);

    let (would_compress, shape_str, estimated_output_bytes, estimated_savings_pct, pass_reason_str):
        (bool, String, usize, f64, Option<String>) =
        match &decision {
            CompressDecision::Compressed {
                toon_bytes,
                savings_pct,
                ..
            } => {
                let shape: String =
                    match toon_mcp_core::detector::FormatDetector::detect_and_parse(&input) {
                        Ok((_, val)) => Classifier::classify(&val).as_str().into(),
                        Err(_) => ShapeClass::PassThrough.as_str().into(),
                    };
                (true, shape, *toon_bytes, *savings_pct, None)
            }
            CompressDecision::PassedThrough { reason } => {
                let reason_str: String = reason.as_str();
                (
                    false,
                    ShapeClass::PassThrough.as_str().into(),
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
        threshold_used: config.compression_threshold,
        duration_us,
        pass_reason: pass_reason_str.clone(),
        client_hint: config.client_hint.clone(),
    };

    let _ = log_sink.record(event).await;

    StatsResult {
        would_compress,
        format: fmt.as_str().into(),
        shape_class: shape_str,
        input_bytes,
        estimated_output_bytes,
        estimated_savings_pct,
        threshold: config.compression_threshold,
        pass_reason: pass_reason_str,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use toon_mcp_logging::NoopSink;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            compression_threshold: 0.85,
            min_bytes: 256,
            key_folding: true,
            delimiter: toon_format::Delimiter::Comma,
            tabular_min_rows: 3,
            fold_min_depth: 3,
            primitive_array_min: 5,
            logging_enabled: false,
            logging: toon_mcp_logging::ParquetSinkConfig::default(),
            log_level: "info".into(),
            client_hint: None,
        })
    }

    fn noop_sink() -> Arc<dyn LogSink> {
        Arc::new(NoopSink)
    }

    #[tokio::test]
    async fn detect_format_json() {
        let config = test_config();
        let sink = noop_sink();
        let result = handle_detect_format(
            DetectParams {
                input: r#"{"key":"value"}"#.into(),
            },
            config,
            sink,
        )
        .await;
        assert_eq!(result.format, "json");
        assert_eq!(result.input_bytes, 15);
        assert!(result.line_count.is_none());
    }

    #[tokio::test]
    async fn detect_format_unknown() {
        let config = test_config();
        let sink = noop_sink();
        let result = handle_detect_format(
            DetectParams {
                input: "this is plain text".into(),
            },
            config,
            sink,
        )
        .await;
        assert_eq!(result.format, "unknown");
    }

    #[tokio::test]
    async fn compress_below_threshold_passes_through() {
        let config = test_config();
        let sink = noop_sink();
        let result = handle_compress_content(
            CompressParams {
                input: r#"{"x":1}"#.into(),
            },
            config,
            sink,
        )
        .await;
        assert!(!result.compressed);
        assert_eq!(result.output, r#"{"x":1}"#);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
    }

    #[tokio::test]
    async fn compression_stats_below_threshold() {
        let config = test_config();
        let sink = noop_sink();
        let result = handle_compression_stats(
            StatsParams {
                input: r#"{"x":1}"#.into(),
            },
            config,
            sink,
        )
        .await;
        assert!(!result.would_compress);
        assert_eq!(result.pass_reason, Some("below_min_bytes".into()));
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
            compression_threshold: 0.99,
            ..(*test_config()).clone()
        });
        let sink = noop_sink();
        let result = handle_compress_content(CompressParams { input }, config, sink).await;
        assert!(result.compressed);
        assert!(result.savings_pct > 0.0);
    }
}
