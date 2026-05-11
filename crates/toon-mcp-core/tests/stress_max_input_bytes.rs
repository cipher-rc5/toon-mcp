// file: crates/toon-mcp-core/tests/stress_max_input_bytes.rs
// description: Boundary stress tests asserting Compressor::decide behaviour at max_input_bytes ±1

use toon_mcp_core::{CompressConfig, CompressDecision, Compressor, PassThroughReason};

/// Build a tabular JSON array string of approximately `target_bytes` bytes.
/// Returns a string whose `.len()` is exactly `target_bytes`.
fn build_json_of_exact_size(target_bytes: usize) -> String {
    // Start with a minimal valid JSON array of objects; pad the last row's
    // string field to hit the exact byte target.
    // Rows look like {"id":N,"v":"…"} so we can grow the string field freely.
    let mut s = String::from("[");
    let mut id: u64 = 0;
    while s.len() + 32 < target_bytes {
        if id > 0 {
            s.push(',');
        }
        s.push_str(&format!(r#"{{"id":{id},"v":"x"}}"#));
        id += 1;
    }
    // Now pad inside the last value to reach exact target.
    // Strip trailing ']' if present; we haven't added it yet.
    s.push_str(&format!(r#",{{"id":{id},"v":""#));
    let needed = target_bytes - s.len() - 3; // -3 for closing `"}` and `]`
    for _ in 0..needed {
        s.push('p');
    }
    s.push_str(r#""}]"#);
    debug_assert_eq!(s.len(), target_bytes, "json builder is exact");
    s
}

fn build_jsonl_of_exact_size(target_bytes: usize) -> String {
    let mut s = String::new();
    let mut id: u64 = 0;
    while s.len() + 32 < target_bytes {
        s.push_str(&format!(r#"{{"id":{id},"v":"x"}}"#));
        s.push('\n');
        id += 1;
    }
    // Pad the final line.
    s.push_str(&format!(r#"{{"id":{id},"v":""#));
    let needed = target_bytes - s.len() - 3; // closing `"}` and trailing newline? we omit newline.
    for _ in 0..needed {
        s.push('p');
    }
    s.push_str(r#""}"#);
    while s.len() < target_bytes {
        s.push('p'); // safety pad inside the same string field
    }
    s.truncate(target_bytes);
    s
}

fn build_csv_of_exact_size(target_bytes: usize, delim: char) -> String {
    let mut s = String::new();
    s.push_str(&format!("id{d}v\n", d = delim));
    let mut id: u64 = 0;
    while s.len() + 32 < target_bytes {
        s.push_str(&format!("{id}{d}row\n", id = id, d = delim));
        id += 1;
    }
    s.push_str(&format!("{id}{d}", id = id, d = delim));
    while s.len() < target_bytes {
        s.push('p');
    }
    s.truncate(target_bytes);
    s
}

fn limit_config(limit: usize) -> CompressConfig {
    CompressConfig {
        max_input_bytes: limit,
        // Keep min_bytes well below the test sizes so the lower gate doesn't fire.
        min_bytes: 16,
        // Permissive savings threshold so well-formed inputs compress.
        max_output_ratio: 0.99,
        ..CompressConfig::default()
    }
}

const LIMIT: usize = 1024;

#[test]
fn json_just_below_limit_proceeds() {
    let input = build_json_of_exact_size(LIMIT - 1);
    let config = limit_config(LIMIT);
    let decision = Compressor::decide(&input, &config);
    assert!(
        !matches!(
            decision,
            CompressDecision::PassedThrough {
                reason: PassThroughReason::InputExceedsLimit { .. },
                ..
            }
        ),
        "input under limit must not trigger InputExceedsLimit"
    );
}

#[test]
fn json_at_limit_proceeds() {
    let input = build_json_of_exact_size(LIMIT);
    let config = limit_config(LIMIT);
    let decision = Compressor::decide(&input, &config);
    assert!(
        !matches!(
            decision,
            CompressDecision::PassedThrough {
                reason: PassThroughReason::InputExceedsLimit { .. },
                ..
            }
        ),
        "input exactly at limit (>=) must NOT trigger InputExceedsLimit (compressor uses strict >)"
    );
}

#[test]
fn json_just_above_limit_rejects() {
    let input = build_json_of_exact_size(LIMIT + 1);
    let config = limit_config(LIMIT);
    match Compressor::decide(&input, &config) {
        CompressDecision::PassedThrough {
            reason: PassThroughReason::InputExceedsLimit { actual, limit },
            ..
        } => {
            assert_eq!(actual, LIMIT + 1);
            assert_eq!(limit, LIMIT);
        }
        other => panic!("expected InputExceedsLimit, got {other:?}"),
    }
}

#[test]
fn jsonl_just_above_limit_rejects() {
    let input = build_jsonl_of_exact_size(LIMIT + 1);
    let config = limit_config(LIMIT);
    assert!(matches!(
        Compressor::decide(&input, &config),
        CompressDecision::PassedThrough {
            reason: PassThroughReason::InputExceedsLimit { .. },
            ..
        }
    ));
}

#[test]
fn csv_just_above_limit_rejects() {
    let input = build_csv_of_exact_size(LIMIT + 1, ',');
    let config = limit_config(LIMIT);
    assert!(matches!(
        Compressor::decide(&input, &config),
        CompressDecision::PassedThrough {
            reason: PassThroughReason::InputExceedsLimit { .. },
            ..
        }
    ));
}

#[test]
fn tsv_just_above_limit_rejects() {
    let input = build_csv_of_exact_size(LIMIT + 1, '\t');
    let config = limit_config(LIMIT);
    assert!(matches!(
        Compressor::decide(&input, &config),
        CompressDecision::PassedThrough {
            reason: PassThroughReason::InputExceedsLimit { .. },
            ..
        }
    ));
}
