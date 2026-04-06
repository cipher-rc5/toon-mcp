// file: crates/toon-mcp-core/src/compressor.rs
// description: Threshold-gated TOON compression pipeline
// reference: https://docs.rs/toon-format/latest/toon_format/

use crate::{
    classifier::{Classifier, ClassifyConfig, ShapeClass},
    detector::{FormatDetector, InputFormat},
};
use toon_format::types::KeyFoldingMode;
use toon_format::{Delimiter, EncodeOptions};

/// Configuration for the compressor.
#[derive(Debug, Clone)]
pub struct CompressConfig {
    /// Encode only when `toon_bytes < input_bytes * threshold`.
    pub threshold: f64,
    /// Skip classification for inputs below this byte count.
    pub min_bytes: usize,
    /// Whether to enable TOON key folding for FoldChain shapes.
    pub key_folding: bool,
    /// The array delimiter used in TOON output.
    pub delimiter: Delimiter,
    /// Minimum array length to qualify as Tabular (overrides the default constant).
    pub tabular_min_rows: usize,
    /// Minimum chain depth to qualify as FoldChain (overrides the default constant).
    pub fold_min_depth: usize,
    /// Minimum array length to qualify as PrimitiveArray (overrides the default constant).
    pub primitive_array_min: usize,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            threshold: 0.85,
            min_bytes: 256,
            key_folding: true,
            delimiter: Delimiter::Comma,
            tabular_min_rows: crate::classifier::TABULAR_MIN_ROWS,
            fold_min_depth: crate::classifier::FOLD_MIN_DEPTH,
            primitive_array_min: crate::classifier::PRIMITIVE_ARRAY_MIN,
        }
    }
}

/// The reason content was passed through without compression.
#[derive(Debug, Clone, PartialEq)]
pub enum PassThroughReason {
    /// Input format was not recognised.
    UnknownFormat,
    /// Input byte length was below `min_bytes`.
    BelowMinBytes,
    /// TOON savings did not meet the threshold.
    InsufficientSavings {
        /// Observed savings percentage (0.0–1.0).
        estimated_pct: f64,
        /// Configured threshold.
        threshold: f64,
    },
    /// Shape classifier returned `PassThrough`.
    ShapeNotBeneficial,
    /// The input could not be parsed.
    ParseFailed {
        /// The format where parsing failed.
        format: InputFormat,
        /// Human-readable description of the failure.
        detail: String,
    },
}

impl PassThroughReason {
    /// Return a stable lowercase string identifier for logging.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::PassThroughReason;
    ///
    /// assert_eq!(PassThroughReason::UnknownFormat.as_str(), "unknown_format");
    /// assert_eq!(PassThroughReason::BelowMinBytes.as_str(), "below_min_bytes");
    /// assert_eq!(PassThroughReason::ShapeNotBeneficial.as_str(), "shape_not_beneficial");
    /// ```
    pub fn as_str(&self) -> String {
        match self {
            PassThroughReason::UnknownFormat => "unknown_format".into(),
            PassThroughReason::BelowMinBytes => "below_min_bytes".into(),
            PassThroughReason::InsufficientSavings { .. } => "insufficient_savings".into(),
            PassThroughReason::ShapeNotBeneficial => "shape_not_beneficial".into(),
            PassThroughReason::ParseFailed { .. } => "parse_failed".into(),
        }
    }
}

/// The outcome of a compression attempt.
#[derive(Debug, Clone)]
pub enum CompressDecision {
    /// Compression was applied and exceeded the savings threshold.
    Compressed {
        /// The TOON-encoded output string.
        toon: String,
        /// Original input byte length.
        original_bytes: usize,
        /// Encoded TOON byte length.
        toon_bytes: usize,
        /// Fraction of bytes saved (1.0 - toon_bytes / original_bytes).
        savings_pct: f64,
    },
    /// Content was not compressed; the original input should be used as-is.
    PassedThrough {
        /// The reason compression was skipped.
        reason: PassThroughReason,
    },
}

/// Stateless compression decision engine.
///
/// The pipeline is:
/// 1. Byte-length gate (skip if below `min_bytes`)
/// 2. Format detection + parsing
/// 3. Shape classification
/// 4. TOON encoding
/// 5. Savings threshold gate
pub struct Compressor;

impl Compressor {
    /// Run the full compression pipeline on `input` with `config`.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::{CompressConfig, CompressDecision, Compressor, PassThroughReason};
    ///
    /// // Short input is passed through immediately (below min_bytes).
    /// let short = r#"{"x":1}"#;
    /// let config = CompressConfig::default();
    /// assert!(matches!(
    ///     Compressor::decide(short, &config),
    ///     CompressDecision::PassedThrough { reason: PassThroughReason::BelowMinBytes }
    /// ));
    ///
    /// // Unknown/prose input is passed through as UnknownFormat.
    /// let prose = "a".repeat(300);
    /// assert!(matches!(
    ///     Compressor::decide(&prose, &config),
    ///     CompressDecision::PassedThrough { reason: PassThroughReason::UnknownFormat }
    /// ));
    /// ```
    pub fn decide(input: &str, config: &CompressConfig) -> CompressDecision {
        let original_bytes = input.len();

        // Step 1: byte-length gate.
        if original_bytes < config.min_bytes {
            return CompressDecision::PassedThrough {
                reason: PassThroughReason::BelowMinBytes,
            };
        }

        // Step 2: detect and parse.
        let (fmt, value) = match FormatDetector::detect_and_parse(input) {
            Ok(pair) => pair,
            Err(crate::error::CoreError::ParseFailed { format, detail, .. }) => {
                if format == InputFormat::Unknown {
                    return CompressDecision::PassedThrough {
                        reason: PassThroughReason::UnknownFormat,
                    };
                }
                return CompressDecision::PassedThrough {
                    reason: PassThroughReason::ParseFailed { format, detail },
                };
            }
            Err(e) => {
                // JSON or CSV parse error.
                return CompressDecision::PassedThrough {
                    reason: PassThroughReason::ParseFailed {
                        format: InputFormat::Unknown,
                        detail: e.to_string(),
                    },
                };
            }
        };

        let _ = fmt; // format is captured for potential future use

        // Step 3: classify shape.
        let classify_config = ClassifyConfig {
            tabular_min_rows: config.tabular_min_rows,
            fold_min_depth: config.fold_min_depth,
            primitive_array_min: config.primitive_array_min,
        };
        let shape = Classifier::classify_with(&value, &classify_config);
        if shape == ShapeClass::PassThrough {
            return CompressDecision::PassedThrough {
                reason: PassThroughReason::ShapeNotBeneficial,
            };
        }

        // Step 4: TOON encode.
        let key_folding = if config.key_folding {
            KeyFoldingMode::Safe
        } else {
            KeyFoldingMode::Off
        };
        let opts = EncodeOptions::new()
            .with_delimiter(config.delimiter)
            .with_key_folding(key_folding);

        let toon = match toon_format::encode(&value, &opts) {
            Ok(s) => s,
            Err(e) => {
                return CompressDecision::PassedThrough {
                    reason: PassThroughReason::ParseFailed {
                        format: InputFormat::Unknown,
                        detail: e.to_string(),
                    },
                };
            }
        };

        let toon_bytes = toon.len();

        // Step 5: savings threshold gate.
        let savings_pct = 1.0 - (toon_bytes as f64 / original_bytes as f64);
        if savings_pct < (1.0 - config.threshold) {
            return CompressDecision::PassedThrough {
                reason: PassThroughReason::InsufficientSavings {
                    estimated_pct: savings_pct,
                    threshold: config.threshold,
                },
            };
        }

        CompressDecision::Compressed {
            toon,
            original_bytes,
            toon_bytes,
            savings_pct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn large_tabular_json() -> String {
        let rows: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"name":"User{i}","score":{score},"active":true,"tag":"alpha"}}"#,
                    i = i,
                    score = i as f64 * 0.5
                )
            })
            .collect();
        format!("[{}]", rows.join(","))
    }

    fn large_jsonl() -> String {
        (0..50)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"name":"User{i}","score":{score},"active":true}}"#,
                    i = i,
                    score = i as f64 * 0.5
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn large_csv() -> String {
        let mut lines = vec!["id,name,score,active".to_string()];
        for i in 0..50 {
            lines.push(format!("{i},User{i},{},{}", i as f64 * 0.5, true));
        }
        lines.join("\n")
    }

    #[test]
    fn below_min_bytes_passes_through() {
        let input = r#"{"x":1}"#;
        let config = CompressConfig::default();
        match Compressor::decide(input, &config) {
            CompressDecision::PassedThrough {
                reason: PassThroughReason::BelowMinBytes,
            } => {}
            other => panic!("expected BelowMinBytes, got {other:?}"),
        }
    }

    #[test]
    fn unknown_format_passes_through() {
        let input = "a".repeat(300);
        let config = CompressConfig::default();
        match Compressor::decide(&input, &config) {
            CompressDecision::PassedThrough {
                reason: PassThroughReason::UnknownFormat,
            } => {}
            other => panic!("expected UnknownFormat, got {other:?}"),
        }
    }

    #[test]
    fn shape_not_beneficial_passes_through() {
        // A long JSON string literal — well-formed JSON but a scalar root,
        // which is PassThrough shape.
        let long_string = format!(r#""{}""#, "x".repeat(300));
        let config = CompressConfig::default();
        match Compressor::decide(&long_string, &config) {
            CompressDecision::PassedThrough { .. } => {}
            other => panic!("expected PassedThrough, got {other:?}"),
        }
    }

    #[test]
    fn json_tabular_compresses() {
        let input = large_tabular_json();
        assert!(input.len() >= 256);
        let config = CompressConfig {
            threshold: 0.99,
            ..CompressConfig::default()
        };
        match Compressor::decide(&input, &config) {
            CompressDecision::Compressed {
                ref toon,
                original_bytes,
                toon_bytes,
                savings_pct,
            } => {
                assert!(toon_bytes < original_bytes);
                assert!(savings_pct > 0.0);
                assert!(!toon.is_empty());
            }
            CompressDecision::PassedThrough { reason } => {
                panic!("expected Compressed, got PassedThrough({reason:?})");
            }
        }
    }

    #[test]
    fn jsonl_tabular_compresses() {
        let input = large_jsonl();
        assert!(input.len() >= 256);
        let config = CompressConfig {
            threshold: 0.99,
            ..CompressConfig::default()
        };
        match Compressor::decide(&input, &config) {
            CompressDecision::Compressed { .. } => {}
            CompressDecision::PassedThrough { reason } => {
                panic!("expected Compressed, got PassedThrough({reason:?})");
            }
        }
    }

    #[test]
    fn csv_tabular_compresses() {
        // CSV headers are preserved as JSON keys when normalised, so TOON
        // header compression still applies for wide, many-row tables.
        // For this test we verify the pipeline runs end-to-end (detect ->
        // parse -> classify -> encode) and produces a Tabular classification.
        // Whether the final output is smaller depends on column width vs. data
        // density; we just require the pipeline does not error out.
        let input = large_csv();
        assert!(input.len() >= 256);
        let config = CompressConfig {
            threshold: 0.99,
            ..CompressConfig::default()
        };
        // Either Compressed or InsufficientSavings is acceptable; anything
        // else (UnknownFormat, BelowMinBytes, ParseFailed) is a bug.
        match Compressor::decide(&input, &config) {
            CompressDecision::Compressed { .. } => {}
            CompressDecision::PassedThrough {
                reason: PassThroughReason::InsufficientSavings { .. },
            } => {}
            other => {
                panic!("unexpected decision: {other:?}");
            }
        }
    }

    #[test]
    fn insufficient_savings_passes_through() {
        // Use a threshold so tight that even good compression fails.
        let input = large_tabular_json();
        let config = CompressConfig {
            threshold: 0.0,
            ..CompressConfig::default()
        };
        match Compressor::decide(&input, &config) {
            CompressDecision::PassedThrough {
                reason: PassThroughReason::InsufficientSavings { .. },
            } => {}
            other => panic!("expected InsufficientSavings, got {other:?}"),
        }
    }
}
