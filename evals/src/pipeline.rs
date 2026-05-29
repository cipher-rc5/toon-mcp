//! Deterministic pipeline evaluation: runs each corpus item through the real
//! `toon-mcp-core` compressor and records byte savings, token savings (exact,
//! via the model tokenizer), classifier shape, and encode→decode fidelity.

use crate::corpus::CorpusItem;
use crate::llm::LlmClient;
use crate::toonkit::{decode_pipeline, encode_pipeline, parse_to_value, values_equiv};
use serde::{Deserialize, Serialize};
use toon_mcp_core::{
    Classifier, CompressConfig, CompressDecision, Compressor, InputFormat, ShapeClass,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub id: String,
    pub format: String,
    pub category: String,
    pub intended_shape: String,
    pub original_bytes: usize,

    pub detected_format: String,
    /// Actual classifier shape for the parsed value (independent of byte gate).
    pub actual_shape: Option<String>,
    pub shape_correct: Option<bool>,

    /// What the *server* would do (gated on byte ratio).
    pub compressed: bool,
    pub pass_reason: Option<String>,
    pub pipeline_toon_bytes: Option<usize>,
    pub pipeline_byte_savings_pct: Option<f64>,

    /// Self-encoded TOON metrics computed for every parseable item, so we can
    /// see token wins even when the byte gate declined to compress.
    pub toon_bytes: Option<usize>,
    pub byte_savings_pct: Option<f64>,
    pub orig_tokens: Option<usize>,
    pub toon_tokens: Option<usize>,
    pub token_savings_pct: Option<f64>,

    /// encode→decode round-trip against the parsed value.
    pub roundtrip_ok: Option<bool>,
    pub roundtrip_detail: Option<String>,

    pub error: Option<String>,
}

fn shape_str(s: ShapeClass) -> &'static str {
    match s {
        ShapeClass::Tabular => "tabular",
        ShapeClass::FoldChain => "fold_chain",
        ShapeClass::PrimitiveArray => "primitive_array",
        ShapeClass::Mixed => "mixed",
        ShapeClass::PassThrough => "pass_through",
    }
}

fn fmt_str(f: InputFormat) -> &'static str {
    match f {
        InputFormat::Json => "json",
        InputFormat::Jsonl => "jsonl",
        InputFormat::Csv => "csv",
        InputFormat::Tsv => "tsv",
        InputFormat::Unknown => "unknown",
    }
}

/// `tokens` is optional: when `None` the token-savings columns are skipped
/// (lets the pipeline stage run without a live server if desired).
pub fn evaluate_item(
    item: &CorpusItem,
    config: &CompressConfig,
    tokens: Option<&LlmClient>,
) -> PipelineResult {
    let mut r = PipelineResult {
        id: item.id.clone(),
        format: item.format.clone(),
        category: item.category.clone(),
        intended_shape: item.intended_shape.clone(),
        original_bytes: item.payload.len(),
        detected_format: "unknown".into(),
        actual_shape: None,
        shape_correct: None,
        compressed: false,
        pass_reason: None,
        pipeline_toon_bytes: None,
        pipeline_byte_savings_pct: None,
        toon_bytes: None,
        byte_savings_pct: None,
        orig_tokens: None,
        toon_tokens: None,
        token_savings_pct: None,
        roundtrip_ok: None,
        roundtrip_detail: None,
        error: None,
    };

    // --- Real pipeline decision (byte-gated, mirrors the server) ---
    match Compressor::decide(&item.payload, config) {
        CompressDecision::Compressed {
            toon_bytes,
            savings_pct,
            input_format,
            shape_class,
            ..
        } => {
            r.compressed = true;
            r.detected_format = fmt_str(input_format).into();
            r.pipeline_toon_bytes = Some(toon_bytes);
            r.pipeline_byte_savings_pct = Some(savings_pct * 100.0);
            r.actual_shape = Some(shape_str(shape_class).into());
        }
        CompressDecision::PassedThrough {
            reason,
            input_format,
        } => {
            r.pass_reason = Some(reason.as_str().to_string());
            if let Some(f) = input_format {
                r.detected_format = fmt_str(f).into();
            }
        }
    }

    // --- Parse + classify + self-encode (independent of the byte gate) ---
    let value = match parse_to_value(&item.format, &item.payload, config.csv_numeric_coercion) {
        Ok(v) => v,
        Err(e) => {
            r.error = Some(e.to_string());
            return r;
        }
    };

    let actual = shape_str(Classifier::classify(&value));
    r.actual_shape = Some(actual.to_string());
    r.shape_correct = Some(actual == item.intended_shape);

    let toon = match encode_pipeline(&value) {
        Ok(t) => t,
        Err(e) => {
            r.error = Some(e.to_string());
            return r;
        }
    };
    r.toon_bytes = Some(toon.len());
    r.byte_savings_pct = Some((1.0 - toon.len() as f64 / item.payload.len() as f64) * 100.0);

    // Round-trip fidelity against the parsed value.
    match decode_pipeline(&toon) {
        Ok(decoded) => {
            if values_equiv(&value, &decoded) {
                r.roundtrip_ok = Some(true);
            } else {
                r.roundtrip_ok = Some(false);
                r.roundtrip_detail = Some(roundtrip_diff(&value, &decoded));
            }
        }
        Err(e) => {
            r.roundtrip_ok = Some(false);
            r.roundtrip_detail = Some(format!("decode error: {e}"));
        }
    }

    // Exact token savings under the model tokenizer.
    if let Some(client) = tokens {
        match (
            client.count_tokens(&item.payload),
            client.count_tokens(&toon),
        ) {
            (Ok(o), Ok(t)) => {
                r.orig_tokens = Some(o);
                r.toon_tokens = Some(t);
                if o > 0 {
                    r.token_savings_pct = Some((1.0 - t as f64 / o as f64) * 100.0);
                }
            }
            _ => { /* tokenizer unavailable; leave token columns empty */ }
        }
    }

    r
}

fn roundtrip_diff(orig: &serde_json::Value, decoded: &serde_json::Value) -> String {
    let o = serde_json::to_string(orig).unwrap_or_default();
    let d = serde_json::to_string(decoded).unwrap_or_default();
    let clip = |s: &str| s.chars().take(160).collect::<String>();
    format!("orig={} decoded={}", clip(&o), clip(&d))
}
