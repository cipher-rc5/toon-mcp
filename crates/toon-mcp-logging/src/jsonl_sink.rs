// file: crates/toon-mcp-logging/src/jsonl_sink.rs
// description: Lock-free LogSink that appends JSONL to daily-partitioned files queryable by DuckDB

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info, warn};

use crate::{error::LogError, event::LogEvent, sink::LogSink};
use async_trait::async_trait;

/// Configuration for the JSONL sink.
///
/// Events are written as newline-delimited JSON to hive-partitioned files
/// under `log_dir`:
///
/// ```text
/// log_dir/
///   day=2026-04-06/
///     events.jsonl
///   day=2026-04-07/
///     events.jsonl
/// ```
///
/// These files can be queried directly with DuckDB from any process at any
/// time without acquiring a lock:
///
/// ```sql
/// SELECT * FROM read_json('data/logs/**/*.jsonl');
/// ```
#[derive(Debug, Clone)]
pub struct JsonlSinkConfig {
    /// Root directory for partitioned JSONL log files.
    pub log_dir: PathBuf,
    /// Number of events to buffer before flushing to disk.
    pub buffer_size: usize,
    /// Periodic flush interval when the buffer has not filled.
    pub flush_interval: Duration,
}

impl Default for JsonlSinkConfig {
    fn default() -> Self {
        Self {
            log_dir: PathBuf::from("data/logs"),
            buffer_size: 1000,
            flush_interval: Duration::from_secs(300),
        }
    }
}

/// Commands sent from `JsonlSink` to the background writer task.
enum SinkCmd {
    Record(LogEvent),
    Flush(oneshot::Sender<Result<(), LogError>>),
    Shutdown(oneshot::Sender<Result<(), LogError>>),
}

/// A `LogSink` that appends events as JSONL to daily-partitioned files.
///
/// The writer task owns the open file handles and is the sole writer. Readers
/// (e.g. `duckdb data/logs/**/*.jsonl`) never need to acquire a lock.
pub struct JsonlSink {
    sender: mpsc::Sender<SinkCmd>,
    serialization_failed_count: Arc<AtomicU64>,
}

impl JsonlSink {
    /// Construct a new sink and the background task future.
    ///
    /// The caller MUST spawn the returned future before the sink is used.
    /// Most callers should prefer [`JsonlSink::start`] which spawns the
    /// writer task automatically and returns its `JoinHandle`.
    pub fn new(
        config: JsonlSinkConfig,
    ) -> Result<(Self, impl Future<Output = ()> + use<>), LogError> {
        let (tx, rx) = mpsc::channel(config.buffer_size);

        if let Err(e) = std::fs::create_dir_all(&config.log_dir) {
            return Err(LogError::IoError(e));
        }

        let serialization_failed_count = Arc::new(AtomicU64::new(0));
        let sink = JsonlSink {
            sender: tx,
            serialization_failed_count: Arc::clone(&serialization_failed_count),
        };
        let task_future = writer_task(
            rx,
            config.log_dir,
            config.flush_interval,
            config.buffer_size,
            serialization_failed_count,
        );

        Ok((sink, task_future))
    }

    /// Construct a sink and spawn the writer task on the current Tokio
    /// runtime, returning the sink and the spawned task's `JoinHandle`.
    ///
    /// This is the recommended constructor for production code: callers
    /// cannot forget to spawn the writer task. Use [`JsonlSink::new`] only
    /// in tests where the caller wants explicit control over spawning.
    pub fn start(config: JsonlSinkConfig) -> Result<(Self, JoinHandle<()>), LogError> {
        let (sink, task) = Self::new(config)?;
        let handle = tokio::spawn(task);
        Ok((sink, handle))
    }

    /// Number of `LogEvent`s the writer failed to serialise as JSON.
    ///
    /// Increments monotonically over the lifetime of the sink. A non-zero
    /// value is unexpected — `serde_json` failures on a flat owned-string
    /// struct indicate either non-finite floats or memory pressure.
    pub fn serialization_failed_count(&self) -> u64 {
        self.serialization_failed_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl LogSink for JsonlSink {
    async fn record(&self, event: LogEvent) -> Result<(), LogError> {
        self.sender
            .send(SinkCmd::Record(event))
            .await
            .map_err(|e| LogError::ChannelSend(e.to_string()))
    }

    /// Flush all buffered events to disk and wait for acknowledgement.
    ///
    /// This method blocks until the writer task confirms the flush is
    /// complete — unlike a fire-and-forget send, the caller can rely on
    /// events being durable after this returns `Ok(())`.
    async fn flush(&self) -> Result<(), LogError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(SinkCmd::Flush(tx))
            .await
            .map_err(|e| LogError::ChannelSend(e.to_string()))?;
        rx.await.map_err(|e| LogError::ShutdownAck(e.to_string()))?
    }

    async fn shutdown(self: Box<Self>) -> Result<(), LogError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(SinkCmd::Shutdown(tx))
            .await
            .map_err(|e| LogError::ChannelSend(e.to_string()))?;
        rx.await.map_err(|e| LogError::ShutdownAck(e.to_string()))?
    }
}

/// Background writer task that owns the open file handles.
///
/// Holds one `std::fs::File` per active day partition and re-opens only when
/// the UTC day rolls over, reducing syscall overhead under sustained load.
async fn writer_task(
    mut rx: mpsc::Receiver<SinkCmd>,
    log_dir: PathBuf,
    flush_interval: Duration,
    buffer_size: usize,
    serialization_failed_count: Arc<AtomicU64>,
) {
    let mut pending: Vec<LogEvent> = Vec::with_capacity(buffer_size);
    // Open file handles keyed by YYYY-MM-DD partition string.
    let mut file_handles: HashMap<String, std::fs::File> = HashMap::new();
    let mut interval = time::interval(flush_interval);
    // Skip the first tick to avoid flushing immediately on startup.
    interval.tick().await;

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(SinkCmd::Record(event)) => {
                        pending.push(event);
                        if pending.len() >= buffer_size
                            && let Err(e) = flush_pending(
                                &mut pending,
                                &log_dir,
                                &mut file_handles,
                                &serialization_failed_count,
                            ).await {
                                warn!("JsonlSink flush (buffer full) failed: {e}");
                            }
                    }
                    Some(SinkCmd::Flush(ack)) => {
                        let result = flush_pending(
                            &mut pending,
                            &log_dir,
                            &mut file_handles,
                            &serialization_failed_count,
                        )
                        .await;
                        let _ = ack.send(result);
                    }
                    Some(SinkCmd::Shutdown(ack)) => {
                        let result = flush_pending(
                            &mut pending,
                            &log_dir,
                            &mut file_handles,
                            &serialization_failed_count,
                        )
                        .await;
                        let _ = ack.send(result);
                        info!("JsonlSink writer task: shutdown complete");
                        return;
                    }
                    None => {
                        if let Err(e) = flush_pending(
                            &mut pending,
                            &log_dir,
                            &mut file_handles,
                            &serialization_failed_count,
                        ).await {
                            warn!("JsonlSink flush (channel closed) failed: {e}");
                        }
                        info!("JsonlSink writer task: channel closed, exiting");
                        return;
                    }
                }
            }

            _ = interval.tick() => {
                if !pending.is_empty()
                    && let Err(e) = flush_pending(
                        &mut pending,
                        &log_dir,
                        &mut file_handles,
                        &serialization_failed_count,
                    ).await {
                        warn!("JsonlSink flush (periodic) failed: {e}");
                    }
            }
        }
    }
}

/// Append all pending events as JSONL to the appropriate day partition file.
///
/// File handles are cached in `file_handles` and re-used across calls to avoid
/// repeated `open(2)` / `close(2)` syscalls. A handle is opened the first time
/// a partition key is seen; it is closed only when the writer task exits.
async fn flush_pending(
    pending: &mut Vec<LogEvent>,
    log_dir: &Path,
    file_handles: &mut HashMap<String, std::fs::File>,
    serialization_failed_count: &AtomicU64,
) -> Result<(), LogError> {
    if pending.is_empty() {
        return Ok(());
    }

    // Group events by UTC day to write to the correct partition.
    let mut by_day: HashMap<String, Vec<&LogEvent>> = HashMap::new();
    for event in pending.iter() {
        let day = day_partition_key(event.ts_us);
        by_day.entry(day).or_default().push(event);
    }

    // Serialise all lines on the async task before entering spawn_blocking.
    let mut day_lines: Vec<(String, PathBuf, String)> = Vec::new();
    for (day, events) in &by_day {
        let partition_dir = log_dir.join(format!("day={day}"));
        let lines: String = events
            .iter()
            .map(|e| match serde_json::to_string(e) {
                Ok(s) => s,
                Err(err) => {
                    // H1: visible error — never silently drop events.
                    serialization_failed_count.fetch_add(1, Ordering::Relaxed);
                    error!(
                        event_id = %e.event_id,
                        "JsonlSink: serialization failed, event skipped: {err}"
                    );
                    String::new()
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        if lines.is_empty() {
            continue;
        }

        day_lines.push((day.clone(), partition_dir, lines + "\n"));
    }

    if day_lines.is_empty() {
        pending.clear();
        return Ok(());
    }

    // Move file handles into spawn_blocking and get them back after.
    let mut handles_snapshot = std::mem::take(file_handles);

    let (handles_snapshot, result) = tokio::task::spawn_blocking(
        move || -> (HashMap<String, std::fs::File>, Result<(), LogError>) {
            for (day, partition_dir, lines) in day_lines {
                if let Err(e) = std::fs::create_dir_all(&partition_dir) {
                    return (handles_snapshot, Err(LogError::IoError(e)));
                }

                let file_path = partition_dir.join("events.jsonl");
                // L3: re-use open handle; open once per partition per process run.
                let entry = handles_snapshot.entry(day);
                let file = match entry {
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => {
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&file_path)
                        {
                            Ok(f) => v.insert(f),
                            Err(e) => return (handles_snapshot, Err(LogError::IoError(e))),
                        }
                    }
                };

                if let Err(e) = file.write_all(lines.as_bytes()) {
                    return (handles_snapshot, Err(LogError::IoError(e)));
                }
            }
            (handles_snapshot, Ok(()))
        },
    )
    .await
    .map_err(|e| {
        error!("JsonlSink spawn_blocking task failed: {e}");
        LogError::IoError(std::io::Error::other(format!("spawn_blocking failed: {e}")))
    })?;

    // Restore the file handles regardless of outcome.
    *file_handles = handles_snapshot;

    result?;
    pending.clear();
    Ok(())
}

/// Returns a `YYYY-MM-DD` string from a microsecond Unix timestamp.
fn day_partition_key(ts_us: i64) -> String {
    use chrono::{DateTime, TimeZone, Utc};
    let secs = ts_us / 1_000_000;
    let nanos = ((ts_us % 1_000_000).unsigned_abs() * 1_000) as u32;
    // Use timestamp_opt which returns LocalResult; fall back to Unix epoch on
    // out-of-range values (not expected in practice).
    let dt: DateTime<Utc> = Utc
        .timestamp_opt(secs, nanos)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().expect("epoch is valid"));
    dt.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::LogEvent;

    fn make_event(n: u64) -> LogEvent {
        LogEvent {
            event_id: format!("event-{n}"),
            ts_us: 1_700_000_000_000_000_i64, // 2023-11-14 in microseconds
            tool_name: "compress_content".into(),
            input_format: "jsonl".into(),
            shape_class: "tabular".into(),
            input_bytes: n * 100,
            output_bytes: n * 44,
            compressed: true,
            savings_pct: 0.56,
            threshold_used: 0.85,
            duration_us: n * 10,
            pass_reason: None,
            client_hint: Some("test".into()),
        }
    }

    #[tokio::test]
    async fn jsonl_sink_flushes_to_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs successfully");
        tokio::spawn(task);

        for i in 1..=5 {
            sink.record(make_event(i)).await.expect("record succeeds");
        }

        Box::new(sink).shutdown().await.expect("shutdown succeeds");

        // Find the written JSONL file.
        let jsonl_path = dir.path().join("day=2023-11-14").join("events.jsonl");
        assert!(jsonl_path.exists(), "JSONL partition file was created");

        let content = std::fs::read_to_string(&jsonl_path).expect("file is readable");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5, "five events were written");
    }

    #[tokio::test]
    async fn event_fields_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs successfully");
        tokio::spawn(task);

        let event = make_event(42);
        sink.record(event.clone()).await.expect("record succeeds");
        Box::new(sink).shutdown().await.expect("shutdown succeeds");

        let jsonl_path = dir.path().join("day=2023-11-14").join("events.jsonl");
        let content = std::fs::read_to_string(&jsonl_path).expect("file is readable");
        let parsed: LogEvent =
            serde_json::from_str(content.trim()).expect("JSONL line deserialises");

        assert_eq!(parsed.event_id, event.event_id);
        assert_eq!(parsed.tool_name, "compress_content");
        assert!((parsed.savings_pct - 0.56).abs() < 1e-9);
    }

    #[tokio::test]
    async fn flush_is_acknowledged() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 1000, // large buffer — will not auto-flush
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs successfully");
        tokio::spawn(task);

        sink.record(make_event(1)).await.expect("record succeeds");
        // Explicit flush — must block until data is on disk.
        sink.flush().await.expect("flush succeeds");

        let jsonl_path = dir.path().join("day=2023-11-14").join("events.jsonl");
        assert!(
            jsonl_path.exists(),
            "JSONL file exists after acknowledged flush"
        );

        Box::new(sink).shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn file_handle_reused_across_flushes() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 1000,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs successfully");
        tokio::spawn(task);

        // Two separate explicit flushes — both events must reach the same file.
        sink.record(make_event(1)).await.expect("record 1 succeeds");
        sink.flush().await.expect("flush 1 succeeds");
        sink.record(make_event(2)).await.expect("record 2 succeeds");
        sink.flush().await.expect("flush 2 succeeds");

        Box::new(sink).shutdown().await.expect("shutdown succeeds");

        let jsonl_path = dir.path().join("day=2023-11-14").join("events.jsonl");
        let content = std::fs::read_to_string(&jsonl_path).expect("file is readable");
        assert_eq!(content.lines().count(), 2, "both events persisted");
    }

    #[test]
    fn day_partition_key_correct() {
        // 2023-11-14 00:00:00 UTC in microseconds
        assert_eq!(day_partition_key(1_699_920_000_000_000), "2023-11-14");
    }

    /// Day rollover: events whose `ts_us` straddle a UTC date boundary must
    /// land in the right `day=YYYY-MM-DD` partitions and the writer task's
    /// file-handle cache must hold a separate handle per day.
    #[tokio::test]
    async fn day_rollover_writes_to_separate_partitions() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 1000,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs");
        tokio::spawn(task);

        // 2023-11-14 23:59:59.999999 UTC
        let end_of_day_us: i64 = 1_700_006_399_999_999;
        // 2023-11-15 00:00:00.000000 UTC
        let start_of_next_day_us: i64 = 1_700_006_400_000_000;

        let mut e1 = make_event(1);
        e1.ts_us = end_of_day_us;
        let mut e2 = make_event(2);
        e2.ts_us = start_of_next_day_us;
        let mut e3 = make_event(3);
        // Write back to first day after the rollover — exercises cache reuse
        // across days within a single flush.
        e3.ts_us = end_of_day_us - 1_000_000;

        sink.record(e1).await.expect("record 1");
        sink.record(e2).await.expect("record 2");
        sink.flush().await.expect("flush 1");

        sink.record(e3).await.expect("record 3");
        sink.flush().await.expect("flush 2");

        Box::new(sink).shutdown().await.expect("shutdown");

        let day_a = dir.path().join("day=2023-11-14").join("events.jsonl");
        let day_b = dir.path().join("day=2023-11-15").join("events.jsonl");

        assert!(day_a.exists(), "first day's partition exists");
        assert!(day_b.exists(), "second day's partition exists");

        let a = std::fs::read_to_string(&day_a).expect("read day a");
        let b = std::fs::read_to_string(&day_b).expect("read day b");
        assert_eq!(a.lines().count(), 2, "two events in first day");
        assert_eq!(b.lines().count(), 1, "one event in second day");
    }

    #[tokio::test]
    async fn start_spawns_writer_task() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, _handle) = JsonlSink::start(config).expect("JsonlSink::start");
        sink.record(make_event(1)).await.expect("record");
        sink.flush().await.expect("flush");

        let jsonl_path = dir.path().join("day=2023-11-14").join("events.jsonl");
        assert!(jsonl_path.exists());
        Box::new(sink).shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn serialization_failed_count_starts_zero() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = JsonlSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };
        let (sink, task) = JsonlSink::new(config).expect("JsonlSink constructs");
        tokio::spawn(task);
        sink.record(make_event(1)).await.expect("record");
        sink.flush().await.expect("flush");
        assert_eq!(sink.serialization_failed_count(), 0);
        Box::new(sink).shutdown().await.expect("shutdown");
    }
}
