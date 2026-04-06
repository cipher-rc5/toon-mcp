// file: crates/toon-mcp-core/src/error.rs
// description: Error types for toon-mcp-core using thiserror

use crate::detector::InputFormat;
use thiserror::Error;

/// Errors that can occur in the core detection, parsing, and compression pipeline.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A line in a JSONL input failed to parse as valid JSON.
    #[error("JSONL parse failed at line {line}: {detail}")]
    ParseFailed {
        /// The input format that triggered the failure.
        format: InputFormat,
        /// Zero-based line index where the failure occurred.
        line: usize,
        /// Human-readable description of the parse error.
        detail: String,
    },

    /// A JSON value could not be parsed from the input.
    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// A CSV/TSV record could not be read.
    #[error("CSV parse error: {0}")]
    CsvError(#[from] csv::Error),

    /// The TOON encoder returned an error.
    #[error("TOON encode error: {0}")]
    ToonError(String),
}
