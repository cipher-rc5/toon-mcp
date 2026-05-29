// file: crates/toon-mcp-core/src/classifier.rs
// description: AST-style shape classifier for normalised serde_json::Value trees

use serde_json::Value;

// Default classification thresholds. These are the single internal source of
// truth, surfaced publicly only through `ClassifyConfig::default()` and
// `CompressConfig::default()` so the defaults can change without breaking the
// public API surface.
pub(crate) const TABULAR_MIN_ROWS: usize = 3;
pub(crate) const FOLD_MIN_DEPTH: usize = 3;
pub(crate) const PRIMITIVE_ARRAY_MIN: usize = 5;

/// Hard upper bound on `is_fold_chain` recursion depth.
///
/// `serde_json::from_str` already caps parse recursion at 128 by default,
/// so this constant is defence in depth: even if a future code path feeds
/// the classifier a hand-built `Value` tree, recursion is bounded.
/// Reaching the cap is treated as "not a fold chain".
const FOLD_CHAIN_MAX_DEPTH: usize = 256;

/// Hard upper bound on the descent depth used by
/// `has_beneficial_descendant`. Bounds the classifier's worst-case work
/// on adversarial/very deeply-nested inputs while still reaching typical
/// API envelopes (`{data: {results: [...]}}` style) which sit at depth 2.
const BENEFICIAL_SCAN_MAX_DEPTH: usize = 8;

/// The structural shape classes that the classifier assigns to a parsed value.
///
/// Shape determines whether TOON encoding is likely to yield a token reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeClass {
    /// Array of N uniform objects where all values are primitives.
    /// TOON header compression delivers the highest savings here.
    Tabular,
    /// Singly-keyed object chain of depth >= `FOLD_MIN_DEPTH`.
    /// Key folding eliminates repetitive nesting braces.
    FoldChain,
    /// Flat array of scalar values (no objects or nested arrays).
    PrimitiveArray,
    /// Non-uniform array or array with nested structures.
    /// Encoding is attempted; the threshold gate decides.
    Mixed,
    /// Root is a scalar, array is too short, format is Unknown, or the object
    /// has multiple keys without a nested chain. Pass through unchanged.
    PassThrough,
}

impl ShapeClass {
    /// Return a stable lowercase string identifier for logging and display.
    ///
    /// # Examples
    ///
    /// ```
    /// use toon_mcp_core::ShapeClass;
    ///
    /// assert_eq!(ShapeClass::Tabular.as_str(), "tabular");
    /// assert_eq!(ShapeClass::FoldChain.as_str(), "fold_chain");
    /// assert_eq!(ShapeClass::PassThrough.as_str(), "pass_through");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            ShapeClass::Tabular => "tabular",
            ShapeClass::FoldChain => "fold_chain",
            ShapeClass::PrimitiveArray => "primitive_array",
            ShapeClass::Mixed => "mixed",
            ShapeClass::PassThrough => "pass_through",
        }
    }
}

impl std::fmt::Display for ShapeClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Thresholds controlling shape classification decisions.
#[derive(Debug, Clone, Copy)]
pub struct ClassifyConfig {
    /// Minimum array length for Tabular classification.
    pub tabular_min_rows: usize,
    /// Minimum chain depth for FoldChain classification.
    pub fold_min_depth: usize,
    /// Minimum array length for PrimitiveArray classification.
    pub primitive_array_min: usize,
}

impl Default for ClassifyConfig {
    fn default() -> Self {
        Self {
            tabular_min_rows: TABULAR_MIN_ROWS,
            fold_min_depth: FOLD_MIN_DEPTH,
            primitive_array_min: PRIMITIVE_ARRAY_MIN,
        }
    }
}

/// Stateless shape classifier.
///
/// Operates directly on the parsed `serde_json::Value` tree without
/// allocating any new strings. Classification rules are evaluated in priority
/// order: Tabular > FoldChain > PrimitiveArray > Mixed > PassThrough.
pub struct Classifier;

impl Classifier {
    /// Classify the structural shape of `value` using default thresholds.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use toon_mcp_core::{Classifier, ShapeClass};
    ///
    /// // Array of uniform objects with primitive values -> Tabular.
    /// let tabular = json!([
    ///     {"id": 1, "name": "Alice"},
    ///     {"id": 2, "name": "Bob"},
    ///     {"id": 3, "name": "Carol"},
    /// ]);
    /// assert_eq!(Classifier::classify(&tabular), ShapeClass::Tabular);
    ///
    /// // Deeply nested single-key chain -> FoldChain.
    /// let chain = json!({"a": {"b": {"c": "leaf"}}});
    /// assert_eq!(Classifier::classify(&chain), ShapeClass::FoldChain);
    ///
    /// // Scalar root -> PassThrough.
    /// assert_eq!(Classifier::classify(&json!(42)), ShapeClass::PassThrough);
    /// ```
    pub fn classify(value: &Value) -> ShapeClass {
        Self::classify_with(value, &ClassifyConfig::default())
    }

    /// Classify the structural shape of `value` with runtime-configurable thresholds.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use toon_mcp_core::{Classifier, ClassifyConfig, ShapeClass};
    ///
    /// // Lower the tabular row minimum to 2 to classify a small array.
    /// let config = ClassifyConfig { tabular_min_rows: 2, ..ClassifyConfig::default() };
    /// let v = json!([{"id": 1, "name": "Alice"}, {"id": 2, "name": "Bob"}]);
    /// assert_eq!(Classifier::classify_with(&v, &config), ShapeClass::Tabular);
    /// ```
    pub fn classify_with(value: &Value, config: &ClassifyConfig) -> ShapeClass {
        match value {
            Value::Array(arr) => Self::classify_array(arr, config),
            Value::Object(_) if Self::is_fold_chain(value, 0, config.fold_min_depth) => {
                ShapeClass::FoldChain
            }
            // Multi-key wrapper objects that contain a beneficial shape
            // somewhere inside (e.g. `{meta:..., results:[<tabular>...]}`)
            // are escalated to `Mixed` so the encoder runs and the
            // threshold gate decides. Without this, common API envelopes
            // would pass through unchanged and lose all the savings the
            // tabular array would have delivered on its own.
            Value::Object(_) if Self::has_beneficial_descendant(value, config, 0) => {
                ShapeClass::Mixed
            }
            // Empty maps, multi-key objects with only scalar children, and
            // root scalars (Bool, Number, String, Null) all PassThrough.
            _ => ShapeClass::PassThrough,
        }
    }

    // --- private helpers ---

    fn classify_array(arr: &[Value], config: &ClassifyConfig) -> ShapeClass {
        if arr.len() >= config.tabular_min_rows && Self::is_tabular(arr) {
            return ShapeClass::Tabular;
        }

        if arr.len() >= config.primitive_array_min && arr.iter().all(is_primitive) {
            return ShapeClass::PrimitiveArray;
        }

        if arr.len() >= config.tabular_min_rows {
            return ShapeClass::Mixed;
        }

        ShapeClass::PassThrough
    }

    fn is_tabular(arr: &[Value]) -> bool {
        // All elements must be objects.
        let all_objects = arr.iter().all(|v| v.is_object());
        if !all_objects {
            return false;
        }

        // Determine the key set from the first element.
        let first_keys: Vec<&str> = match arr.first() {
            Some(Value::Object(m)) => m.keys().map(String::as_str).collect(),
            _ => return false,
        };

        // All elements must share identical key sets and contain only primitives.
        arr.iter().all(|v| match v {
            Value::Object(map) => {
                let keys_match = map.len() == first_keys.len()
                    && first_keys.iter().all(|k| map.contains_key(*k));
                let all_primitive = map.values().all(is_primitive);
                keys_match && all_primitive
            }
            _ => false,
        })
    }

    /// Walk `value` looking for any sub-tree the classifier would label
    /// `Tabular`, `FoldChain`, `PrimitiveArray`, or `Mixed`. Used to
    /// escalate wrapper-object roots so that documents like
    /// `{meta, total, items: [<tabular>...]}` reach the encoder instead
    /// of passing through. Bounded by `BENEFICIAL_SCAN_MAX_DEPTH`.
    fn has_beneficial_descendant(value: &Value, config: &ClassifyConfig, depth: usize) -> bool {
        if depth >= BENEFICIAL_SCAN_MAX_DEPTH {
            return false;
        }
        match value {
            Value::Array(arr) => {
                if Self::classify_array(arr, config) != ShapeClass::PassThrough {
                    return true;
                }
                arr.iter()
                    .any(|v| Self::has_beneficial_descendant(v, config, depth + 1))
            }
            Value::Object(map) => {
                // An object subtree may itself be a fold chain even when
                // the root isn't; detect that explicitly.
                if Self::is_fold_chain(value, 0, config.fold_min_depth) {
                    return true;
                }
                map.values()
                    .any(|v| Self::has_beneficial_descendant(v, config, depth + 1))
            }
            _ => false,
        }
    }

    fn is_fold_chain(value: &Value, depth: usize, min_depth: usize) -> bool {
        // Defence in depth: refuse to recurse beyond FOLD_CHAIN_MAX_DEPTH so
        // a deeply-nested adversarial value tree cannot blow the stack.
        if depth >= FOLD_CHAIN_MAX_DEPTH {
            return false;
        }
        match value {
            Value::Object(map) if map.len() == 1 => {
                // The map.len() == 1 guard above guarantees exactly one entry.
                if let Some(child) = map.values().next() {
                    match child {
                        Value::Object(_) => Self::is_fold_chain(child, depth + 1, min_depth),
                        _ => depth + 1 >= min_depth,
                    }
                } else {
                    // Unreachable: len == 1 implies values().next() is Some.
                    depth >= min_depth
                }
            }
            _ => depth >= min_depth,
        }
    }
}

/// Returns `true` if `value` is a JSON primitive (bool, number, string, or null).
fn is_primitive(value: &Value) -> bool {
    matches!(
        value,
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Null
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_tabular() {
        let v = json!([
            {"id": 1, "name": "Alice", "score": 9.5},
            {"id": 2, "name": "Bob",   "score": 8.0},
            {"id": 3, "name": "Carol", "score": 7.5},
        ]);
        assert_eq!(Classifier::classify(&v), ShapeClass::Tabular);
    }

    #[test]
    fn tabular_requires_min_rows() {
        let v = json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"},
        ]);
        // Only 2 rows — below TABULAR_MIN_ROWS (3)
        assert_ne!(Classifier::classify(&v), ShapeClass::Tabular);
    }

    #[test]
    fn tabular_requires_uniform_keys() {
        let v = json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "extra": "foo"},
            {"id": 3, "name": "Carol"},
        ]);
        assert_ne!(Classifier::classify(&v), ShapeClass::Tabular);
    }

    #[test]
    fn tabular_requires_primitive_values() {
        let v = json!([
            {"id": 1, "nested": {"x": 1}},
            {"id": 2, "nested": {"x": 2}},
            {"id": 3, "nested": {"x": 3}},
        ]);
        assert_ne!(Classifier::classify(&v), ShapeClass::Tabular);
    }

    #[test]
    fn classify_fold_chain() {
        let v = json!({"a": {"b": {"c": {"d": "leaf"}}}});
        assert_eq!(Classifier::classify(&v), ShapeClass::FoldChain);
    }

    #[test]
    fn fold_chain_requires_min_depth() {
        // depth 2 — below FOLD_MIN_DEPTH (3)
        let v = json!({"a": {"b": "leaf"}});
        assert_ne!(Classifier::classify(&v), ShapeClass::FoldChain);
    }

    #[test]
    fn classify_primitive_array() {
        let v = json!([1, 2, 3, 4, 5, 6]);
        assert_eq!(Classifier::classify(&v), ShapeClass::PrimitiveArray);
    }

    #[test]
    fn primitive_array_requires_min_length() {
        let v = json!([1, 2, 3, 4]);
        // Only 4 items — below PRIMITIVE_ARRAY_MIN (5)
        assert_ne!(Classifier::classify(&v), ShapeClass::PrimitiveArray);
    }

    #[test]
    fn classify_mixed() {
        let v = json!([
            {"id": 1},
            {"id": 2, "extra": "foo"},
            {"id": 3},
        ]);
        assert_eq!(Classifier::classify(&v), ShapeClass::Mixed);
    }

    #[test]
    fn classify_pass_through_scalar() {
        assert_eq!(Classifier::classify(&json!(42)), ShapeClass::PassThrough);
        assert_eq!(
            Classifier::classify(&json!("hello")),
            ShapeClass::PassThrough
        );
    }

    #[test]
    fn classify_pass_through_short_array() {
        let v = json!([1, 2]);
        assert_eq!(Classifier::classify(&v), ShapeClass::PassThrough);
    }

    #[test]
    fn classify_pass_through_multi_key_object() {
        // Multi-key object whose children are all scalars — nothing
        // beneficial to encode. Must still PassThrough.
        let v = json!({"a": 1, "b": 2});
        assert_eq!(Classifier::classify(&v), ShapeClass::PassThrough);
    }

    #[test]
    fn wrapper_object_with_tabular_array_escalates_to_mixed() {
        // Common API envelope: scalars at the root plus a tabular array
        // payload. Without descendant escalation this would PassThrough
        // and the tabular savings would be lost.
        let v = json!({
            "exported_at": "2026-02-18T17:09:35.757Z",
            "total_tweets": 3,
            "tweets": [
                {"id": 1, "user": "a", "text": "hello"},
                {"id": 2, "user": "b", "text": "world"},
                {"id": 3, "user": "c", "text": "!"},
            ],
        });
        assert_eq!(Classifier::classify(&v), ShapeClass::Mixed);
    }

    #[test]
    fn wrapper_object_with_fold_chain_escalates_to_mixed() {
        let v = json!({
            "meta": "anything",
            "deep": {"a": {"b": {"c": "leaf"}}},
        });
        assert_eq!(Classifier::classify(&v), ShapeClass::Mixed);
    }

    #[test]
    fn wrapper_object_with_primitive_array_escalates_to_mixed() {
        let v = json!({
            "name": "ids",
            "values": [1, 2, 3, 4, 5, 6],
        });
        assert_eq!(Classifier::classify(&v), ShapeClass::Mixed);
    }

    #[test]
    fn deeply_nested_envelope_reaches_encoder() {
        // {data: {results: {items: [<tabular>...]}}}. The wrapper is a
        // single-key chain so it classifies as `FoldChain`; either way
        // the tree reaches the encoder rather than passing through.
        let v = json!({
            "data": {
                "results": {
                    "items": [
                        {"id": 1, "name": "a"},
                        {"id": 2, "name": "b"},
                        {"id": 3, "name": "c"},
                    ],
                },
            },
        });
        assert_ne!(Classifier::classify(&v), ShapeClass::PassThrough);
    }

    #[test]
    fn multi_key_envelope_with_deep_tabular_escalates_to_mixed() {
        // Same payload but the wrapper has two keys, so it is not a
        // fold chain. Must still escalate to `Mixed`.
        let v = json!({
            "version": 1,
            "data": {"results": [
                {"id": 1, "name": "a"},
                {"id": 2, "name": "b"},
                {"id": 3, "name": "c"},
            ]},
        });
        assert_eq!(Classifier::classify(&v), ShapeClass::Mixed);
    }

    #[test]
    fn tabular_min_rows_zero_does_not_panic() {
        let config = ClassifyConfig {
            tabular_min_rows: 0,
            ..ClassifyConfig::default()
        };
        let v = json!([{"id": 1}]);
        let _ = Classifier::classify_with(&v, &config);
    }

    #[test]
    fn fold_min_depth_zero_does_not_panic() {
        let config = ClassifyConfig {
            fold_min_depth: 0,
            ..ClassifyConfig::default()
        };
        let v = json!({"a": "leaf"});
        let _ = Classifier::classify_with(&v, &config);
    }

    #[test]
    fn deep_fold_chain_exceeding_internal_cap_does_not_overflow() {
        // Build a singly-keyed nested object chain whose depth exceeds the
        // internal FOLD_CHAIN_MAX_DEPTH cap (256). The classifier must return
        // a ShapeClass without overflowing the stack. The exact variant is
        // implementation-defined at this boundary; we only assert that the
        // call returns. Constructing the tree iteratively avoids any test-side
        // recursion that could obscure a regression.
        const DEPTH: usize = 300;
        let mut acc = serde_json::Value::String("leaf".into());
        for i in 0..DEPTH {
            let mut m = serde_json::Map::new();
            m.insert(format!("k{i}"), acc);
            acc = serde_json::Value::Object(m);
        }
        // If FOLD_CHAIN_MAX_DEPTH were missing, this would stack-overflow.
        let _ = Classifier::classify(&acc);
    }

    #[test]
    fn primitive_array_min_zero_does_not_panic() {
        let config = ClassifyConfig {
            primitive_array_min: 0,
            ..ClassifyConfig::default()
        };
        let v = json!([1]);
        let _ = Classifier::classify_with(&v, &config);
    }
}

#[cfg(test)]
mod proptest_tests {
    // file: crates/toon-mcp-core/src/classifier.rs (proptest_tests)
    // description: Property tests for shape classification rules over generated trees.

    use super::*;
    use proptest::prelude::*;
    use serde_json::{Map, Value};

    /// Strategy for JSON primitive values (the only kind allowed in a Tabular row).
    fn primitive_strategy() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| Value::Number(n.into())),
            "[a-zA-Z0-9_-]{0,6}".prop_map(Value::String),
        ]
    }

    /// Strategy producing a uniform array of length >= TABULAR_MIN_ROWS where
    /// every element is an object with the same set of keys, all primitive
    /// values. Such arrays must classify as `Tabular`.
    fn tabular_array_strategy() -> impl Strategy<Value = Value> {
        // 1 to 5 unique header keys.
        prop::collection::vec("[a-zA-Z][a-zA-Z0-9_]{0,4}", 1..6)
            .prop_filter("keys must be unique", |hs| {
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                hs.iter().all(|h| seen.insert(h.as_str()))
            })
            .prop_flat_map(|keys| {
                // Build at least TABULAR_MIN_ROWS rows.
                let row_count = TABULAR_MIN_ROWS..(TABULAR_MIN_ROWS + 4);
                let n_keys = keys.len();
                let row_strategy = prop::collection::vec(primitive_strategy(), n_keys..=n_keys)
                    .prop_map({
                        let keys = keys.clone();
                        move |values| {
                            let mut map = Map::new();
                            for (k, v) in keys.iter().zip(values) {
                                map.insert(k.clone(), v);
                            }
                            Value::Object(map)
                        }
                    });
                prop::collection::vec(row_strategy, row_count).prop_map(Value::Array)
            })
    }

    /// Build a left-nested singly-keyed object chain of the given depth.
    /// Depth 1 = `{k: leaf}`, depth 2 = `{k: {k: leaf}}`, etc.
    fn build_fold_chain(depth: usize, leaf: Value) -> Value {
        let mut acc = leaf;
        for i in 0..depth {
            let mut m = Map::new();
            m.insert(format!("k{i}"), acc);
            acc = Value::Object(m);
        }
        acc
    }

    /// Strategy producing a singly-keyed nested object chain of depth at least
    /// `FOLD_MIN_DEPTH`, terminated by a primitive leaf (not an object).
    fn fold_chain_strategy() -> impl Strategy<Value = Value> {
        (
            FOLD_MIN_DEPTH..(FOLD_MIN_DEPTH + 4),
            // Use a non-object leaf so the chain ends cleanly. Using an object
            // here would extend the chain by one and is therefore redundant.
            primitive_strategy(),
        )
            .prop_map(|(depth, leaf)| build_fold_chain(depth, leaf))
    }

    /// Strategy for any scalar JSON value at the root.
    fn scalar_root_strategy() -> impl Strategy<Value = Value> {
        primitive_strategy()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Any uniform array of objects with primitive values and length >=
        /// TABULAR_MIN_ROWS classifies as `Tabular`.
        #[test]
        fn uniform_object_arrays_classify_as_tabular(v in tabular_array_strategy()) {
            prop_assert_eq!(Classifier::classify(&v), ShapeClass::Tabular);
        }

        /// A singly-keyed object chain of depth >= FOLD_MIN_DEPTH classifies
        /// as `FoldChain`.
        #[test]
        fn deep_single_key_chains_classify_as_fold_chain(v in fold_chain_strategy()) {
            prop_assert_eq!(Classifier::classify(&v), ShapeClass::FoldChain);
        }

        /// Any scalar value at the root classifies as `PassThrough`.
        #[test]
        fn scalar_roots_classify_as_pass_through(v in scalar_root_strategy()) {
            prop_assert_eq!(Classifier::classify(&v), ShapeClass::PassThrough);
        }
    }
}
