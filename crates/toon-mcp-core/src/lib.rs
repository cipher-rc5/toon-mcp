// file: crates/toon-mcp-core/src/lib.rs
// description: Public API surface for toon-mcp-core

#![deny(missing_docs)]

//! Pure detection, parsing, classification, and compression pipeline for
//! structured data formats (JSON, JSONL, CSV, TSV).
//!
//! This crate is intentionally pure — no I/O, no async, no network. It can
//! be used from any context including sync test runners and Criterion
//! benchmarks. The server crate wraps calls via `spawn_blocking`.

/// Shape classification of parsed JSON value trees.
pub mod classifier;

/// Threshold-gated TOON compression pipeline.
pub mod compressor;

/// Format detection and parser dispatch.
pub mod detector;

/// Error types for the core pipeline.
pub mod error;

/// Format-specific parser implementations.
pub mod parser;

pub use classifier::{
    Classifier, ClassifyConfig, FOLD_MIN_DEPTH, PRIMITIVE_ARRAY_MIN, ShapeClass, TABULAR_MIN_ROWS,
};
pub use compressor::{
    CompressConfig, CompressDecision, Compressor, DEFAULT_MAX_INPUT_BYTES, PassThroughReason,
};
pub use detector::{FormatDetector, InputFormat};
pub use error::CoreError;
pub use parser::Parser;
