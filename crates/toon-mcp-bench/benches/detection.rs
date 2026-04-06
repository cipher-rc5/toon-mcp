// file: crates/toon-mcp-bench/benches/detection.rs
// description: Criterion benchmarks for format detection across all input formats
// reference: https://docs.rs/criterion/latest/criterion/

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use toon_mcp_core::FormatDetector;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

fn load_fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/{name}"))
        .unwrap_or_else(|_| panic!("fixture {name} must exist in fixtures/"))
}

fn bench_detection(c: &mut Criterion) {
    let small_json = load_fixture("small_json.json");
    let large_json = load_fixture("large_tabular.json");
    let large_jsonl = load_fixture("large_jsonl.jsonl");
    let large_csv = load_fixture("large_csv.csv");
    let unknown = "This is plain English prose with no structured format whatsoever. \
                   It contains no JSON, CSV, or TSV delimiters."
        .to_string();

    // Make a TSV version of the CSV fixture.
    let large_tsv = large_csv.replace(',', "\t");

    let mut group = c.benchmark_group("detect_format");

    group.throughput(Throughput::Bytes(small_json.len() as u64));
    group.bench_function("detect_json_small", |b| {
        b.iter(|| FormatDetector::detect(black_box(&small_json)))
    });

    group.throughput(Throughput::Bytes(large_json.len() as u64));
    group.bench_function("detect_json_large", |b| {
        b.iter(|| FormatDetector::detect(black_box(&large_json)))
    });

    group.throughput(Throughput::Bytes(large_jsonl.len() as u64));
    group.bench_function("detect_jsonl_large", |b| {
        b.iter(|| FormatDetector::detect(black_box(&large_jsonl)))
    });

    group.throughput(Throughput::Bytes(large_csv.len() as u64));
    group.bench_function("detect_csv_large", |b| {
        b.iter(|| FormatDetector::detect(black_box(&large_csv)))
    });

    group.throughput(Throughput::Bytes(large_tsv.len() as u64));
    group.bench_function("detect_tsv_large", |b| {
        b.iter(|| FormatDetector::detect(black_box(&large_tsv)))
    });

    group.throughput(Throughput::Bytes(unknown.len() as u64));
    group.bench_function("detect_unknown", |b| {
        b.iter(|| FormatDetector::detect(black_box(&unknown)))
    });

    group.finish();
}

criterion_group!(benches, bench_detection);
criterion_main!(benches);
