// file: crates/toon-mcp-core/src/parser/json.rs
// description: JSON parser — thin wrapper around serde_json::from_str

use crate::{error::CoreError, parser::Parser};

/// Parses a raw JSON string into a `serde_json::Value`.
///
/// This is a stateless unit struct that delegates entirely to `serde_json`.
/// No transformation of the value tree is performed.
///
/// # Examples
///
/// ```
/// use toon_mcp_core::parser::{Parser, json::JsonParser};
///
/// let parser = JsonParser;
/// let val = parser.parse(r#"{"a":1}"#).unwrap();
/// assert_eq!(val["a"], 1);
///
/// assert!(parser.parse("not json").is_err());
/// ```
pub struct JsonParser;

impl Parser for JsonParser {
    fn parse(&self, input: &str) -> Result<serde_json::Value, CoreError> {
        let value = serde_json::from_str(input)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_object() {
        let p = JsonParser;
        let v = p.parse(r#"{"a":1,"b":"two"}"#).unwrap();
        assert!(v.is_object());
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_array() {
        let p = JsonParser;
        let v = p.parse(r#"[1,2,3]"#).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
    }

    #[test]
    fn parse_invalid_returns_error() {
        let p = JsonParser;
        assert!(p.parse("not json").is_err());
    }
}
