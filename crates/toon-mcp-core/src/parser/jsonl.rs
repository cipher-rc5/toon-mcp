// file: crates/toon-mcp-core/src/parser/jsonl.rs
// description: JSONL parser — splits on newlines and wraps lines in a Value::Array

use crate::{detector::InputFormat, error::CoreError, parser::Parser};

/// Parses newline-delimited JSON (JSONL) into a `Value::Array` containing one
/// element per non-empty line.
///
/// Each line is parsed independently. If any line fails to parse, a
/// `CoreError::ParseFailed` is returned with the 1-based *physical* line
/// number of the offending line (counting blank/whitespace-only lines, which
/// are otherwise skipped). This matches the line numbers a user sees in an
/// editor when debugging the input.
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
                    // 1-based physical line number (see module doc comment).
                    line: idx + 1,
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
                // `not json` is the 2nd physical line (1-based).
                assert_eq!(line, 2);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn malformed_line_after_blank_lines_reports_physical_line() {
        // Two leading blank lines (skipped at parse time) precede the bad line.
        // The reported `line` must be the 1-based *physical* line number (4),
        // matching what a user sees in an editor — not the count of parsed
        // records (which would be 1).
        let input = "\n\n{\"id\":1}\nnot json";
        let p = JsonlParser;
        let err = p.parse(input).unwrap_err();
        match err {
            CoreError::ParseFailed { format, line, .. } => {
                assert_eq!(format, InputFormat::Jsonl);
                assert_eq!(line, 4);
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

    // --- L6: edge-case tests ---

    #[test]
    fn empty_input_returns_empty_array() {
        let p = JsonlParser;
        let v = p.parse("").unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn whitespace_only_returns_empty_array() {
        let p = JsonlParser;
        let v = p.parse("   \n\t\n  ").unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn unicode_field_names_and_values_parse() {
        let input = "{\"名前\":\"太郎\"}\n{\"名前\":\"花子\"}";
        let p = JsonlParser;
        let v = p.parse(input).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["名前"], "太郎");
    }
}

#[cfg(test)]
mod proptest_tests {
    // file: crates/toon-mcp-core/src/parser/jsonl.rs (proptest_tests)
    // description: Round-trip proptests for JsonlParser using generated object streams.

    use super::*;
    use proptest::prelude::*;
    use serde_json::{Map, Value};

    /// Strategy for a single JSON object whose values are all primitives.
    /// `serde_json::to_string` on such a value is guaranteed to be single-line
    /// so it round-trips cleanly through JSONL.
    fn flat_object_strategy() -> impl Strategy<Value = Value> {
        let primitive = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| Value::Number(n.into())),
            "[a-zA-Z0-9 _-]{0,8}".prop_map(Value::String),
        ];
        prop::collection::hash_map("[a-zA-Z][a-zA-Z0-9_]{0,5}", primitive, 0..6).prop_map(|m| {
            let mut map = Map::new();
            for (k, v) in m {
                map.insert(k, v);
            }
            Value::Object(map)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// A vector of flat objects joined by `\n` parses to a `Value::Array`
        /// with the same elements in the same order.
        #[test]
        fn jsonl_round_trip_preserves_objects(
            items in prop::collection::vec(flat_object_strategy(), 0..8)
        ) {
            let lines: Vec<String> = items
                .iter()
                .map(|v| serde_json::to_string(v).expect("serialize"))
                .collect();
            let input = lines.join("\n");
            let parsed = JsonlParser.parse(&input).expect("parse");
            prop_assert_eq!(parsed, Value::Array(items));
        }
    }
}
