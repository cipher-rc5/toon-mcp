// file: crates/toon-mcp-core/src/parser/mod.rs
// description: Parser trait definition shared by all format-specific parsers

/// CSV and TSV parser.
pub mod csv;
/// JSON parser.
pub mod json;
/// JSONL (newline-delimited JSON) parser.
pub mod jsonl;

use crate::error::CoreError;

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
