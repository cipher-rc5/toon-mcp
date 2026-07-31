// file: crates/toon-mcp-core/src/parser/json.rs
// description: JSON parser — serde_json deserialization with duplicate-key rejection

use crate::{error::CoreError, parser::Parser};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Parses a raw JSON string into a `serde_json::Value`.
///
/// Unlike a plain `serde_json::from_str`, objects containing the same key
/// more than once are rejected with [`CoreError::DuplicateKey`]: `serde_json`
/// would otherwise keep only the last occurrence, silently dropping data.
/// No other transformation of the value tree is performed.
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
/// // Duplicate keys are rejected rather than silently collapsed.
/// assert!(parser.parse(r#"{"a":1,"a":2}"#).is_err());
/// ```
pub struct JsonParser;

impl Parser for JsonParser {
    fn parse(&self, input: &str) -> Result<Value, CoreError> {
        parse_dup_checked(crate::parser::strip_bom(input))
    }
}

/// Marker embedded in the custom serde error so the duplicate key can be
/// recovered from `serde_json::Error`'s string-only payload.
const DUPLICATE_KEY_MARKER: &str = "toon-mcp duplicate key: ";

/// Parse `input` into a `Value`, rejecting duplicate object keys at any depth.
pub(crate) fn parse_dup_checked(input: &str) -> Result<Value, CoreError> {
    let mut de = serde_json::Deserializer::from_str(input);
    let result = DupCheckedValue
        .deserialize(&mut de)
        .and_then(|value| de.end().map(|()| value));
    result.map_err(|e| {
        let msg = e.to_string();
        match msg.find(DUPLICATE_KEY_MARKER) {
            Some(idx) => {
                // The message is "{marker}{key} at line L column C"; recover
                // the key between the marker and the location suffix.
                let tail = &msg[idx + DUPLICATE_KEY_MARKER.len()..];
                let key = tail.rsplit_once(" at line ").map_or(tail, |(k, _)| k);
                CoreError::DuplicateKey {
                    key: key.to_owned(),
                }
            }
            None => CoreError::JsonError(e),
        }
    })
}

/// Seed that builds a `Value` while rejecting duplicate object keys.
struct DupCheckedValue;

impl<'de> DeserializeSeed<'de> for DupCheckedValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(DupCheckedVisitor)
    }
}

struct DupCheckedVisitor;

impl<'de> Visitor<'de> for DupCheckedVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(v)))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        // JSON text cannot spell NaN/Infinity, so `from_f64` cannot fail for
        // input arriving through the deserializer; fall back to Null anyway.
        Ok(Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(v) = seq.next_element_seed(DupCheckedValue)? {
            values.push(v);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(DupCheckedValue)?;
            if out.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "{DUPLICATE_KEY_MARKER}{key}"
                )));
            }
        }
        Ok(Value::Object(out))
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

    #[test]
    fn parse_truncated_object_returns_error() {
        // JSON object missing the closing brace — must surface a parse error.
        let p = JsonParser;
        assert!(p.parse(r#"{"id":1,"name":"Alice"#).is_err());
    }

    #[test]
    fn parse_truncated_array_returns_error() {
        // JSON array missing the closing bracket — must surface a parse error.
        let p = JsonParser;
        assert!(p.parse(r#"[1,2,3"#).is_err());
    }

    #[test]
    fn parse_truncated_string_returns_error() {
        // Unterminated string literal — must surface a parse error.
        let p = JsonParser;
        assert!(p.parse(r#"{"id":"alice"#).is_err());
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let err = JsonParser
            .parse(r#"{"a":1,"a":2}"#)
            .expect_err("must reject");
        match err {
            crate::error::CoreError::DuplicateKey { key } => assert_eq!(key, "a"),
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
    }

    #[test]
    fn nested_duplicate_keys_are_rejected() {
        let err = JsonParser
            .parse(r#"{"outer":{"k":1,"k":2}}"#)
            .expect_err("must reject");
        match err {
            crate::error::CoreError::DuplicateKey { key } => assert_eq!(key, "k"),
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
    }

    #[test]
    fn same_key_in_distinct_objects_is_allowed() {
        let v = JsonParser.parse(r#"[{"a":1},{"a":2}]"#).expect("parse");
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn leading_bom_is_stripped() {
        let v = JsonParser.parse("\u{feff}{\"a\":1}").expect("parse");
        assert_eq!(v["a"], 1);
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
