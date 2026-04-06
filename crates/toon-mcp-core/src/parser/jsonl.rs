// file: crates/toon-mcp-core/src/parser/jsonl.rs
// description: JSONL parser — splits on newlines and wraps lines in a Value::Array

use crate::{detector::InputFormat, error::CoreError, parser::Parser};

/// Parses newline-delimited JSON (JSONL) into a `Value::Array` containing one
/// element per non-empty line.
///
/// Each line is parsed independently. If any line fails to parse, a
/// `CoreError::ParseFailed` is returned with the zero-based line index.
///
/// The resulting `Value::Array` preserves the stream-of-uniform-objects signal
/// that the classifier uses for Tabular shape detection.
///
/// # Examples
///
/// ```
/// use toon_mcp_core::parser::{Parser, jsonl::JsonlParser};
///
/// let parser = JsonlParser;
/// let val = parser.parse("{\"id\":1}\n{\"id\":2}").unwrap();
/// let arr = val.as_array().unwrap();
/// assert_eq!(arr.len(), 2);
/// assert_eq!(arr[0]["id"], 1);
///
/// // A malformed line produces an error.
/// assert!(parser.parse("{\"ok\":1}\nnot json").is_err());
/// ```
pub struct JsonlParser;

impl Parser for JsonlParser {
    fn parse(&self, input: &str) -> Result<serde_json::Value, CoreError> {
        let mut values: Vec<serde_json::Value> = Vec::new();

        for (idx, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(line).map_err(|e| CoreError::ParseFailed {
                    format: InputFormat::Jsonl,
                    line: idx,
                    detail: e.to_string(),
                })?;
            values.push(v);
        }

        Ok(serde_json::Value::Array(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uniform_objects() {
        let input = r#"{"id":1,"name":"Alice"}
{"id":2,"name":"Bob"}
{"id":3,"name":"Carol"}"#;
        let p = JsonlParser;
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["name"], "Bob");
    }

    #[test]
    fn parse_mixed_types() {
        let input = "42\n\"hello\"\ntrue";
        let p = JsonlParser;
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0], 42);
        assert_eq!(arr[1], "hello");
        assert_eq!(arr[2], true);
    }

    #[test]
    fn parse_malformed_line_returns_error() {
        let input = r#"{"id":1}
not json
{"id":3}"#;
        let p = JsonlParser;
        let err = p.parse(input).unwrap_err();
        match err {
            CoreError::ParseFailed { format, line, .. } => {
                assert_eq!(format, InputFormat::Jsonl);
                assert_eq!(line, 1);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn empty_lines_are_skipped() {
        let input = "\n{\"a\":1}\n\n{\"b\":2}\n";
        let p = JsonlParser;
        let v = p.parse(input).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }
}
