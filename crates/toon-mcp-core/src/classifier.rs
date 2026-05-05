// file: crates/toon-mcp-core/src/classifier.rs
// description: AST-style shape classifier for normalised serde_json::Value trees

use serde_json::Value;

/// Minimum number of rows for an array to qualify as Tabular.
pub const TABULAR_MIN_ROWS: usize = 3;

/// Minimum chain depth for FoldChain classification.
pub const FOLD_MIN_DEPTH: usize = 3;

/// Minimum array length for PrimitiveArray classification.
pub const PRIMITIVE_ARRAY_MIN: usize = 5;

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
            Value::Object(map) => {
                if Self::is_fold_chain(value, 0, config.fold_min_depth) {
                    ShapeClass::FoldChain
                } else if map.is_empty() {
                    ShapeClass::PassThrough
                } else {
                    // A multi-key object with no nested chain — pass through.
                    ShapeClass::PassThrough
                }
            }
            // Scalars: Bool, Number, String, Null
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

    fn is_fold_chain(value: &Value, depth: usize, min_depth: usize) -> bool {
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
        let v = json!({"a": 1, "b": 2});
        assert_eq!(Classifier::classify(&v), ShapeClass::PassThrough);
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
    fn primitive_array_min_zero_does_not_panic() {
        let config = ClassifyConfig {
            primitive_array_min: 0,
            ..ClassifyConfig::default()
        };
        let v = json!([1]);
        let _ = Classifier::classify_with(&v, &config);
    }
}
