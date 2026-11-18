// file: crates/toon-mcp-bench/benches/jsonl_sink.rs
// description: Throughput and back-pressure benchmarks for JsonlSink (record + flush)
// reference: https://docs.rs/criterion/latest/criterion/

//! Criterion benchmarks for the JSONL log sink write path.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Runtime;
use toon_mcp_logging::event::LogEvent;
use toon_mcp_logging::jsonl_sink::{JsonlSink, JsonlSinkConfig};
use toon_mcp_logging::sink::LogSink;

/// Construct a representative `LogEvent` for use in throughput measurements.
///
/// The timestamp is fixed so all events land in the same day partition; this
/// keeps the benchmark focused on channel + write throughput rather than
/// partition-key churn.
fn make_event(n: u64) -> LogEvent {
    LogEvent {
        event_id: format!("event-{n}"),
        ts_us: 1_700_000_000_000_000_i64, // 2023-11-14
        tool_name: "compress_content".into(),
        input_format: "jsonl".into(),
        shape_class: "tabular".into(),
        input_bytes: n * 100,
        output_bytes: n * 44,
        compressed: true,
        savings_pct: 0.56,
        threshold_used: 0.85,
        duration_us: n * 10,
        outcome: "ok".into(),
        pass_reason: None,
        client_hint: Some("bench".into()),
    }
}

/// Build a single-threaded tokio runtime to drive the writer task and the
/// async `record` / `flush` calls inside `b.iter`.
fn build_runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds successfully")
}

/// Throughput benchmark: push N events through `record(...)` with a buffer
/// that will fill several times during the run, then `flush()` to wait for
/// the writer task to drain. This measures the combined cost of channel
/// hand-off, periodic mid-loop flushes from the writer, and the final
/// acknowledged flush.
fn bench_throughput_buffer_full(c: &mut Criterion) {
    const EVENT_COUNT: u64 = 10_000;
    const BUFFER_SIZE: usize = 1_000;

    let mut group = c.benchmark_group("jsonl_sink_throughput");
    group.throughput(Throughput::Elements(EVENT_COUNT));
    group.sample_size(10);

    group.bench_function("record_10k_buffer_1k", |b| {
        b.iter_custom(|iters| {
            let rt = build_runtime();
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dir = tempfile::tempdir().expect("tempdir created successfully");
                let config = JsonlSinkConfig {
                    log_dir: dir.path().to_path_buf(),
                    buffer_size: BUFFER_SIZE,
                    flush_interval: Duration::from_secs(3600),
                };

                let elapsed = rt.block_on(async {
                    let (sink, task) =
                        JsonlSink::new(config).expect("JsonlSink constructs successfully");
                    let task_handle = tokio::spawn(task);

                    let start = std::time::Instant::now();
                    for i in 0..EVENT_COUNT {
                        sink.record(black_box(make_event(i)))
                            .await
                            .expect("record succeeds");
                    }
                    sink.flush().await.expect("flush succeeds");
                    let elapsed = start.elapsed();

                    Box::new(sink).shutdown().await.expect("shutdown succeeds");
                    task_handle.await.expect("writer task exits cleanly");
                    elapsed
                });

                total += elapsed;
            }
            total
        });
    });

    group.finish();
}

/// Single-flush latency: record exactly one event and acknowledge-flush.
/// This isolates the round-trip cost of `record` + `flush` (channel send
/// + oneshot ack + a single file write) from the bulk-throughput path.
fn bench_single_flush_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("jsonl_sink_latency");
    group.throughput(Throughput::Elements(1));
    group.sample_size(20);

    group.bench_function("record_one_then_flush", |b| {
        b.iter_custom(|iters| {
            let rt = build_runtime();

            let dir = tempfile::tempdir().expect("tempdir created successfully");
            let config = JsonlSinkConfig {
                log_dir: dir.path().to_path_buf(),
                buffer_size: 1_000,
                flush_interval: Duration::from_secs(3600),
            };

            rt.block_on(async {
                let (sink, task) =
                    JsonlSink::new(config).expect("JsonlSink constructs successfully");
                let task_handle = tokio::spawn(task);

                let start = std::time::Instant::now();
                for i in 0..iters {
                    sink.record(black_box(make_event(i)))
                        .await
                        .expect("record succeeds");
                    sink.flush().await.expect("flush succeeds");
                }
                let elapsed = start.elapsed();

                Box::new(sink).shutdown().await.expect("shutdown succeeds");
                task_handle.await.expect("writer task exits cleanly");
                elapsed
            })
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_throughput_buffer_full,
    bench_single_flush_latency
);
criterion_main!(benches);
