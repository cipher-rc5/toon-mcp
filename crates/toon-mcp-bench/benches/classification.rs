// file: crates/toon-mcp-bench/benches/classification.rs
// description: Criterion benchmarks for shape classification on pre-parsed values
// reference: https://docs.rs/criterion/latest/criterion/

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use toon_mcp_core::{Classifier, FormatDetector};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

fn load_fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/{name}"))
        .unwrap_or_else(|_| panic!("fixture {name} must exist in fixtures/"))
}

fn bench_classification(c: &mut Criterion) {
    // Pre-parse all fixtures outside the benchmark timing loop.
    let large_tabular_raw = load_fixture("large_tabular.json");
    let large_tabular: serde_json::Value =
        serde_json::from_str(&large_tabular_raw).expect("large_tabular.json is valid JSON");

    let deep_fold_raw = load_fixture("deep_fold.json");
    let deep_fold: serde_json::Value =
        serde_json::from_str(&deep_fold_raw).expect("deep_fold.json is valid JSON");

    let mixed_raw = load_fixture("mixed_array.json");
    let mixed: serde_json::Value =
        serde_json::from_str(&mixed_raw).expect("mixed_array.json is valid JSON");

    // Build a large primitive array value in memory.
    let primitive_arr: serde_json::Value = serde_json::Value::Array(
        (0..1000)
            .map(|i| serde_json::Value::Number(serde_json::Number::from(i)))
            .collect(),
    );

    // Scalar root — fastest path (immediate PassThrough).
    let scalar = serde_json::Value::Number(serde_json::Number::from(42));

    let mut group = c.benchmark_group("classify_shape");

    group.bench_function("classify_tabular", |b| {
        b.iter(|| Classifier::classify(black_box(&large_tabular)))
    });

    group.bench_function("classify_fold_chain", |b| {
        b.iter(|| Classifier::classify(black_box(&deep_fold)))
    });

    group.bench_function("classify_primitive_arr", |b| {
        b.iter(|| Classifier::classify(black_box(&primitive_arr)))
    });

    group.bench_function("classify_mixed", |b| {
        b.iter(|| Classifier::classify(black_box(&mixed)))
    });

    group.bench_function("classify_pass_through", |b| {
        b.iter(|| Classifier::classify(black_box(&scalar)))
    });

    // JSONL fixture to verify classify_with_config path.
    let large_jsonl_raw = load_fixture("large_jsonl.jsonl");
    let (_, jsonl_val) = FormatDetector::detect_and_parse(&large_jsonl_raw)
        .expect("large_jsonl.jsonl parses successfully");
    group.bench_function("classify_jsonl_tabular", |b| {
        b.iter(|| Classifier::classify(black_box(&jsonl_val)))
    });

    group.finish();
}

fn build_wide_object(width: usize) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for i in 0..width {
        map.insert(format!("k{i}"), serde_json::Value::String(format!("v{i}")));
    }
    serde_json::Value::Object(map)
}

fn build_wide_array_scalars(width: usize) -> serde_json::Value {
    let mut arr = Vec::with_capacity(width);
    for i in 0..width {
        arr.push(serde_json::Value::String(format!("v{i}")));
    }
    serde_json::Value::Array(arr)
}

fn build_nested_wide(width: usize, depth: usize) -> serde_json::Value {
    let mut acc = build_wide_object(width);
    for i in 0..depth {
        let mut m = serde_json::Map::new();
        m.insert(format!("wrap{i}"), acc);
        acc = serde_json::Value::Object(m);
    }
    acc
}

fn bench_descendant_worst_case(c: &mut Criterion) {
    let mut group = c.benchmark_group("descendant_scan_worst_case");
    for width in [10, 100, 1000] {
        let input = build_wide_object(width);
        group.bench_with_input(
            BenchmarkId::new("wide_object_all_scalars", width),
            &input,
            |b, v| b.iter(|| Classifier::classify(black_box(v))),
        );
        let input = build_wide_array_scalars(width);
        group.bench_with_input(
            BenchmarkId::new("wide_array_of_scalars", width),
            &input,
            |b, v| b.iter(|| Classifier::classify(black_box(v))),
        );
    }
    // Depth-cap interaction: one fixed-width, fixed-depth measurement.
    let input = build_nested_wide(100, 9);
    group.bench_with_input(
        BenchmarkId::new("nested_wide_object_at_cap", "100x9"),
        &input,
        |b, v| b.iter(|| Classifier::classify(black_box(v))),
    );
    group.finish();
}

criterion_group!(benches, bench_classification, bench_descendant_worst_case);
criterion_main!(benches);
