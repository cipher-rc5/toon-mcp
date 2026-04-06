// file: crates/toon-mcp-bench/benches/compression.rs
// description: Full pipeline benchmarks (detect + parse + classify + encode)
// reference: https://docs.rs/criterion/latest/criterion/

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use toon_mcp_core::{CompressConfig, Compressor};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

fn load_fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/{name}"))
        .unwrap_or_else(|_| panic!("fixture {name} must exist in fixtures/"))
}

/// Default benchmark config with a permissive threshold so all compressible
/// inputs go through the full encode path.
fn bench_config() -> CompressConfig {
    CompressConfig {
        threshold: 0.99,
        ..CompressConfig::default()
    }
}

fn bench_pipeline(c: &mut Criterion) {
    let large_json = load_fixture("large_tabular.json");
    let large_jsonl = load_fixture("large_jsonl.jsonl");
    let large_csv = load_fixture("large_csv.csv");
    let deep_fold = load_fixture("deep_fold.json");

    // Unknown prose — tests the full rejection path.
    let prose = "This is a prose paragraph with no structured data. \
                 It will be rejected immediately at the format detection stage. "
        .repeat(6);

    // A tiny JSON object — hits the byte-count short circuit.
    let tiny = r#"{"x":1}"#.to_string();

    let config = bench_config();

    let mut group = c.benchmark_group("pipeline");

    group.throughput(Throughput::Bytes(large_json.len() as u64));
    group.bench_function("pipeline_json_tabular", |b| {
        b.iter(|| Compressor::decide(black_box(&large_json), black_box(&config)))
    });

    group.throughput(Throughput::Bytes(large_jsonl.len() as u64));
    group.bench_function("pipeline_jsonl_uniform", |b| {
        b.iter(|| Compressor::decide(black_box(&large_jsonl), black_box(&config)))
    });

    group.throughput(Throughput::Bytes(large_csv.len() as u64));
    group.bench_function("pipeline_csv_numeric", |b| {
        b.iter(|| Compressor::decide(black_box(&large_csv), black_box(&config)))
    });

    group.throughput(Throughput::Bytes(deep_fold.len() as u64));
    group.bench_function("pipeline_json_fold_chain", |b| {
        b.iter(|| Compressor::decide(black_box(&deep_fold), black_box(&config)))
    });

    group.throughput(Throughput::Bytes(prose.len() as u64));
    group.bench_function("pipeline_pass_through_unknown", |b| {
        b.iter(|| Compressor::decide(black_box(&prose), black_box(&config)))
    });

    group.throughput(Throughput::Bytes(tiny.len() as u64));
    group.bench_function("pipeline_below_min_bytes", |b| {
        b.iter(|| Compressor::decide(black_box(&tiny), black_box(&config)))
    });

    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
