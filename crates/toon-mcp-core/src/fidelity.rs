// file: crates/toon-mcp-core/src/fidelity.rs
// description: Structural value equivalence for encode/decode round-trip checks

use serde_json::Value;

/// Structural equality for encode-decode round-trip checks, where `a` is the
/// original value and `b` is the decoded result.
///
/// Number comparison is direction-aware:
///
/// - Integer original, float result: **rejected**. This is the drift the CSV
///   integer-coercion bug produced (`30` becoming `30.0`); tolerating it is
///   what let that bug hide.
/// - Integer original, integer result: compared exactly.
/// - Float original, float result: compared with a small relative epsilon to
///   absorb decimal round-tripping noise.
/// - Whole-number float original, integer result: **tolerated** when the
///   values match exactly. The TOON encoder canonicalises whole floats to
///   integer spelling (`1.0` encodes as `1`), so this normalisation is
///   inherent to the format, not pipeline corruption.
///
/// Everything else must match structurally (wrong value, missing key, and
/// reordered arrays all fail).
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
/// // Integer -> float drift is rejected; the reverse normalisation is not.
/// assert!(!values_equiv(&json!({"a": 30}), &json!({"a": 30.0})));
/// assert!(values_equiv(&json!({"a": 30.0}), &json!({"a": 30})));
/// assert!(values_equiv(&json!({"a": 30}), &json!({"a": 30})));
/// assert!(!values_equiv(&json!({"a": 30}), &json!({"a": 31})));
/// assert!(!values_equiv(&json!([1, 2]), &json!([2, 1])));
/// ```
pub fn values_equiv(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            match (x.is_f64(), y.is_f64()) {
                // Integer comparison is exact (serde_json compares i64/u64
                // representations correctly across the sign boundary).
                (false, false) => x == y,
                (true, true) => match (x.as_f64(), y.as_f64()) {
                    (Some(fx), Some(fy)) => {
                        (fx - fy).abs() <= 1e-9 * fx.abs().max(fy.abs()).max(1.0)
                    }
                    _ => x == y,
                },
                // TOON canonicalises whole floats to integer spelling
                // (1.0 encodes as 1); tolerate that exact normalisation.
                (true, false) => x
                    .as_f64()
                    .zip(y.as_f64())
                    .is_some_and(|(fx, fy)| fx == fy && fx.fract() == 0.0),
                // An integer decoding as a float is pipeline drift.
                (false, true) => false,
            }
        }
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
    fn integer_to_float_drift_is_rejected() {
        assert!(!values_equiv(&json!(30), &json!(30.0)));
        assert!(!values_equiv(&json!({"n": 1}), &json!({"n": 1.0})));
    }

    #[test]
    fn whole_float_to_integer_normalisation_is_tolerated() {
        // The TOON encoder spells 30.0 as 30; decoding restores an integer.
        assert!(values_equiv(&json!(30.0), &json!(30)));
        assert!(!values_equiv(&json!(30.5), &json!(30)));
        assert!(!values_equiv(&json!(30.0), &json!(31)));
    }

    #[test]
    fn matching_kinds_still_compare() {
        assert!(values_equiv(&json!(30), &json!(30)));
        assert!(values_equiv(&json!(0.5), &json!(0.5)));
        assert!(!values_equiv(&json!(0.5), &json!(0.6)));
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
