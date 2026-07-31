// file: crates/toon-mcp-core/tests/toon_fidelity.rs
// description: Round-trip fidelity harness — encode/decode with pipeline options must preserve values

use proptest::prelude::*;
use serde_json::{Map, Value, json};
use toon_format::types::{KeyFoldingMode, PathExpansionMode};
use toon_format::{DecodeOptions, Delimiter, EncodeOptions};
use toon_mcp_core::values_equiv;

/// Encode with the exact options the compressor builds for its default
/// configuration: comma delimiter, safe key folding.
fn encode_pipeline(value: &Value) -> String {
    let opts = EncodeOptions::new()
        .with_delimiter(Delimiter::Comma)
        .with_key_folding(KeyFoldingMode::Safe);
    toon_format::encode(value, &opts).expect("encode must succeed")
}

/// Decode with safe path expansion so keys folded by `KeyFoldingMode::Safe`
/// are reconstructed into nested objects.
fn decode_pipeline(toon: &str) -> Value {
    let opts = DecodeOptions {
        expand_paths: PathExpansionMode::Safe,
        ..DecodeOptions::default()
    };
    toon_format::decode(toon, &opts).expect("decode must succeed")
}

/// Assert that a value survives the pipeline encode/decode cycle.
fn assert_round_trips(value: &Value) {
    let toon = encode_pipeline(value);
    let decoded = decode_pipeline(&toon);
    assert!(
        values_equiv(value, &decoded),
        "round trip drifted\noriginal: {value}\nencoded:\n{toon}\ndecoded: {decoded}"
    );
}

// ---------------------------------------------------------------------------
// Proptest strategies
// ---------------------------------------------------------------------------

/// Object keys: simple identifiers, the common case for tabular data.
fn key_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,7}"
}

/// Primitive leaf values covering every JSON scalar type. Integers stay in
/// the f64-exact range; floats are built from small rationals so equivalence
/// is not fighting representation noise.
fn primitive_strategy() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-9_007_199_254_740_992i64..=9_007_199_254_740_992i64).prop_map(|i| json!(i)),
        (-1_000_000i64..=1_000_000i64, 1u32..=4u32)
            .prop_map(|(n, d)| json!(n as f64 / f64::from(10u32.pow(d)))),
        "[ -~]{0,16}".prop_map(Value::String),
    ]
}

/// A tabular array: 3-8 rows sharing the same 1-4 column keys, each cell a
/// primitive. This is the shape the classifier labels `Tabular`.
fn tabular_strategy() -> impl Strategy<Value = Value> {
    prop::collection::vec(key_strategy(), 1..5)
        .prop_filter("keys must be unique", |ks| {
            let mut seen = std::collections::HashSet::new();
            ks.iter().all(|k| seen.insert(k.as_str()))
        })
        .prop_flat_map(|keys| {
            let cols = keys.len();
            prop::collection::vec(
                prop::collection::vec(primitive_strategy(), cols..=cols),
                3..9,
            )
            .prop_map(move |rows| {
                Value::Array(
                    rows.into_iter()
                        .map(|cells| {
                            let mut map = Map::new();
                            for (k, v) in keys.iter().zip(cells) {
                                map.insert(k.clone(), v);
                            }
                            Value::Object(map)
                        })
                        .collect(),
                )
            })
        })
}

/// A fold chain: single-key objects nested 3-6 deep ending in a primitive.
/// This is the shape `KeyFoldingMode::Safe` folds into dotted keys.
fn fold_chain_strategy() -> impl Strategy<Value = Value> {
    (
        prop::collection::vec(key_strategy(), 3..7),
        primitive_strategy(),
    )
        .prop_map(|(keys, leaf)| {
            keys.into_iter().rev().fold(leaf, |inner, key| {
                let mut map = Map::new();
                map.insert(key, inner);
                Value::Object(map)
            })
        })
}

/// A primitive array: 5-20 scalar values.
fn primitive_array_strategy() -> impl Strategy<Value = Value> {
    prop::collection::vec(primitive_strategy(), 5..21).prop_map(Value::Array)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn tabular_arrays_round_trip(value in tabular_strategy()) {
        let toon = encode_pipeline(&value);
        let decoded = decode_pipeline(&toon);
        prop_assert!(
            values_equiv(&value, &decoded),
            "round trip drifted\noriginal: {}\nencoded:\n{}\ndecoded: {}",
            value, toon, decoded
        );
    }

    #[test]
    fn fold_chains_round_trip(value in fold_chain_strategy()) {
        let toon = encode_pipeline(&value);
        let decoded = decode_pipeline(&toon);
        prop_assert!(
            values_equiv(&value, &decoded),
            "round trip drifted\noriginal: {}\nencoded:\n{}\ndecoded: {}",
            value, toon, decoded
        );
    }

    #[test]
    fn primitive_arrays_round_trip(value in primitive_array_strategy()) {
        let toon = encode_pipeline(&value);
        let decoded = decode_pipeline(&toon);
        prop_assert!(
            values_equiv(&value, &decoded),
            "round trip drifted\noriginal: {}\nencoded:\n{}\ndecoded: {}",
            value, toon, decoded
        );
    }
}

// ---------------------------------------------------------------------------
// Explicit regression cases
// ---------------------------------------------------------------------------

#[test]
fn unicode_keys_round_trip() {
    assert_round_trips(&json!([
        {"名前": "Alice", "都市": "東京"},
        {"名前": "Bob", "都市": "大阪"},
        {"名前": "Eve", "都市": "京都"},
    ]));
}

#[test]
fn empty_strings_round_trip() {
    assert_round_trips(&json!([
        {"a": "", "b": "x"},
        {"a": "y", "b": ""},
        {"a": "", "b": ""},
    ]));
}

#[test]
fn null_values_round_trip() {
    assert_round_trips(&json!([
        {"id": 1, "note": null},
        {"id": 2, "note": "set"},
        {"id": 3, "note": null},
    ]));
}

#[test]
fn nested_arrays_inside_tabular_rows_round_trip() {
    assert_round_trips(&json!([
        {"id": 1, "tags": ["a", "b"]},
        {"id": 2, "tags": []},
        {"id": 3, "tags": ["c", "d", "e"]},
    ]));
}

#[test]
fn keys_containing_active_delimiter_round_trip() {
    // The pipeline delimiter is a comma; keys containing one must survive.
    assert_round_trips(&json!([
        {"last,first": "Doe,Jane", "id": 1},
        {"last,first": "Roe,Rick", "id": 2},
        {"last,first": "Poe,Edgar", "id": 3},
    ]));
}
