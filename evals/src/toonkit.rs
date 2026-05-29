//! Helpers bridging the corpus to `toon-mcp-core` and the raw `toon-format`
//! encode/decode round-trip.

use anyhow::{Result, anyhow};
use serde_json::Value;
use toon_format::types::{KeyFoldingMode, PathExpansionMode};
use toon_format::{DecodeOptions, Delimiter, EncodeOptions};
use toon_mcp_core::parser::{Parser, csv::CsvParser, json::JsonParser, jsonl::JsonlParser};

/// Parse a payload to the normalised `Value` the pipeline would classify,
/// using the same parser the compressor dispatches for that format.
pub fn parse_to_value(format: &str, payload: &str, numeric_coercion: bool) -> Result<Value> {
    let v = match format {
        "json" => JsonParser.parse(payload),
        "jsonl" => JsonlParser.parse(payload),
        "csv" => CsvParser::csv()
            .with_numeric_coercion(numeric_coercion)
            .parse(payload),
        "tsv" => CsvParser::tsv()
            .with_numeric_coercion(numeric_coercion)
            .parse(payload),
        other => return Err(anyhow!("unknown format `{other}`")),
    };
    v.map_err(|e| anyhow!("parse failed: {e}"))
}

/// Encode with the default pipeline options (comma delimiter, safe key folding).
pub fn encode_pipeline(value: &Value) -> Result<String> {
    let opts = EncodeOptions::new()
        .with_delimiter(Delimiter::Comma)
        .with_key_folding(KeyFoldingMode::Safe);
    toon_format::encode(value, &opts).map_err(|e| anyhow!("encode failed: {e}"))
}

/// Decode pipeline-encoded TOON back to a `Value`. Path expansion is enabled so
/// keys folded by `KeyFoldingMode::Safe` are reconstructed into nested objects.
pub fn decode_pipeline(toon: &str) -> Result<Value> {
    let opts = DecodeOptions {
        expand_paths: PathExpansionMode::Safe,
        ..DecodeOptions::default()
    };
    toon_format::decode(toon, &opts).map_err(|e| anyhow!("decode failed: {e}"))
}

/// Structural equality that tolerates integer/float representation drift
/// (e.g. `30` vs `30.0`) introduced by the encode→decode cycle, while still
/// catching real corruption (wrong value, missing key, reordered array).
pub fn values_equiv(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => (fx - fy).abs() <= 1e-9 * fx.abs().max(fy.abs()).max(1.0),
            _ => x == y,
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| values_equiv(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| values_equiv(x, y)))
        }
        _ => a == b,
    }
}
