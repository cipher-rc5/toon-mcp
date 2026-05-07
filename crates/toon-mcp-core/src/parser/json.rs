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

#[cfg(test)]
mod proptest_tests {
    // file: crates/toon-mcp-core/src/parser/json.rs (proptest_tests)
    // description: Round-trip proptests for JsonParser over generated JSON values.

    use super::*;
    use proptest::prelude::*;
    use serde_json::{Map, Value};

    /// Strategy for arbitrary JSON primitives (no nesting).
    fn primitive_strategy() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            // Restrict to i64 to avoid float NaN/Infinity which serde_json forbids.
            any::<i64>().prop_map(|n| Value::Number(n.into())),
            // Keep strings short and printable to keep the tree compact.
            "[a-zA-Z0-9 _-]{0,8}".prop_map(Value::String),
        ]
    }

    /// Strategy for any JSON value (primitive, object, or array) with bounded
    /// depth.
    fn any_value_strategy() -> impl Strategy<Value = Value> {
        primitive_strategy().prop_recursive(
            3,  // max depth
            16, // max total nodes
            6,  // items per collection
            |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                    prop::collection::hash_map("[a-zA-Z][a-zA-Z0-9_]{0,5}", inner, 0..6).prop_map(
                        |m| {
                            let mut map = Map::new();
                            for (k, v) in m {
                                map.insert(k, v);
                            }
                            Value::Object(map)
                        }
                    ),
                ]
            },
        )
    }

    /// Strategy for a JSON value whose root is always an object or array.
    /// Avoids the filter-rejection cost of using `prop_filter` on top of a
    /// primitive-or-container generator.
    fn object_or_array_strategy() -> impl Strategy<Value = Value> {
        // Box once so we can use a `Clone`able strategy in two arms.
        let inner: BoxedStrategy<Value> = any_value_strategy().boxed();
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::hash_map("[a-zA-Z][a-zA-Z0-9_]{0,5}", inner, 0..6).prop_map(|m| {
                let mut map = Map::new();
                for (k, v) in m {
                    map.insert(k, v);
                }
                Value::Object(map)
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// `serde_json::to_string` then `JsonParser.parse` must yield an
        /// identical `Value` tree.
        #[test]
        fn json_round_trip_preserves_value(v in object_or_array_strategy()) {
            let s = serde_json::to_string(&v).expect("serialize");
            let parsed = JsonParser.parse(&s).expect("parse");
            prop_assert_eq!(parsed, v);
        }
    }
}
