// file: crates/toon-mcp-core/src/parser/mod.rs
// description: Parser trait definition shared by all format-specific parsers

/// CSV and TSV parser.
pub mod csv;
/// JSON parser.
pub mod json;
/// JSONL (newline-delimited JSON) parser.
pub mod jsonl;

use crate::error::CoreError;

/// Strip a single leading UTF-8 byte-order mark.
///
/// Editors and Windows tooling commonly prepend U+FEFF; every parser strips
/// it before parsing so a BOM never turns an otherwise valid document into a
/// parse failure.
pub(crate) fn strip_bom(input: &str) -> &str {
    input.strip_prefix('\u{feff}').unwrap_or(input)
}

/// A stateless parser that converts a raw input string into a normalised
/// `serde_json::Value`.
///
/// All parsers produce a `Value::Array` or `Value::Object` root. The classifier
/// and compressor operate exclusively on the normalised value tree and have no
/// awareness of the original format.
///
/// # Examples
///
/// ```
/// use toon_mcp_core::parser::{Parser, json::JsonParser};
///
/// let parser = JsonParser;
/// let val = parser.parse(r#"[1,2,3]"#).unwrap();
/// assert!(val.is_array());
/// ```
pub trait Parser: Send + Sync {
    /// Parse `input` into a normalised `serde_json::Value`.
    fn parse(&self, input: &str) -> Result<serde_json::Value, CoreError>;
}
