// file: crates/toon-mcp-core/src/lib.rs
// description: Public API surface for toon-mcp-core

pub mod classifier;
pub mod compressor;
pub mod detector;
pub mod error;
pub mod parser;

pub use classifier::{
    Classifier, ClassifyConfig, FOLD_MIN_DEPTH, PRIMITIVE_ARRAY_MIN, ShapeClass, TABULAR_MIN_ROWS,
};
pub use compressor::{CompressConfig, CompressDecision, Compressor, PassThroughReason};
pub use detector::{FormatDetector, InputFormat};
pub use error::CoreError;
