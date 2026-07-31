// file: crates/toon-mcp-core/src/fidelity.rs
// description: Structural value equivalence for encode/decode round-trip checks

use serde_json::Value;

/// Structural equality that tolerates integer/float representation drift
/// (e.g. `30` vs `30.0`) introduced by the encode-decode cycle, while still
/// catching real corruption (wrong value, missing key, reordered array).
///
/// This is the single definition shared by the production round-trip tests
/// and the offline eval harness, so both measure fidelity the same way.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use toon_mcp_core::fidelity::values_equiv;
///
/// assert!(values_equiv(&json!({"a": 30}), &json!({"a": 30.0})));
/// assert!(!values_equiv(&json!({"a": 30}), &json!({"a": 31})));
/// assert!(!values_equiv(&json!([1, 2]), &json!([2, 1])));
/// ```
pub fn values_equiv(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => (fx - fy).abs() <= 1e-9 * fx.abs().max(fy.abs()).max(1.0),
            _ => x == y,
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| values_equiv(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| values_equiv(x, y)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_values_are_equiv() {
        let v = json!({"a": [1, "x", null, {"b": true}]});
        assert!(values_equiv(&v, &v.clone()));
    }

    #[test]
    fn integer_float_drift_is_tolerated() {
        assert!(values_equiv(&json!(30), &json!(30.0)));
        assert!(values_equiv(&json!({"n": 1}), &json!({"n": 1.0})));
    }

    #[test]
    fn wrong_value_is_not_equiv() {
        assert!(!values_equiv(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn missing_key_is_not_equiv() {
        assert!(!values_equiv(&json!({"a": 1, "b": 2}), &json!({"a": 1})));
    }

    #[test]
    fn reordered_array_is_not_equiv() {
        assert!(!values_equiv(&json!([1, 2, 3]), &json!([3, 2, 1])));
    }

    #[test]
    fn type_mismatch_is_not_equiv() {
        assert!(!values_equiv(&json!("1"), &json!(1)));
        assert!(!values_equiv(&json!(null), &json!(0)));
    }
}
