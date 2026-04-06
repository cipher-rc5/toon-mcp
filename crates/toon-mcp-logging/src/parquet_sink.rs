// file: crates/toon-mcp-logging/src/parquet_sink.rs
// description: Lock-free LogSink that appends JSONL to daily-partitioned files queryable by DuckDB

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tracing::{info, warn};

use crate::{error::LogError, event::LogEvent, sink::LogSink};
use async_trait::async_trait;

/// Configuration for the Parquet-compatible JSONL sink.
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
pub struct ParquetSinkConfig {
    /// Root directory for partitioned JSONL log files.
    pub log_dir: PathBuf,
    /// Number of events to buffer before flushing to disk.
    pub buffer_size: usize,
    /// Periodic flush interval when the buffer has not filled.
    pub flush_interval: Duration,
}

impl Default for ParquetSinkConfig {
    fn default() -> Self {
        Self {
            log_dir: PathBuf::from("data/logs"),
            buffer_size: 1000,
            flush_interval: Duration::from_secs(300),
        }
    }
}

/// Commands sent from `ParquetSink` to the background writer task.
enum SinkCmd {
    Record(LogEvent),
    Flush,
    Shutdown(oneshot::Sender<Result<(), LogError>>),
}

/// A `LogSink` that appends events as JSONL to daily-partitioned files.
///
/// The writer task owns the file handle and is the sole writer. Readers
/// (e.g. `duckdb data/logs/**/*.jsonl`) never need to acquire a lock.
pub struct ParquetSink {
    sender: mpsc::Sender<SinkCmd>,
}

impl ParquetSink {
    /// Construct a new sink and the background task future.
    ///
    /// The caller MUST spawn the returned future before the sink is used.
    pub fn new(
        config: ParquetSinkConfig,
    ) -> Result<(Self, impl Future<Output = ()> + use<>), LogError> {
        let (tx, rx) = mpsc::channel(config.buffer_size);

        if let Err(e) = std::fs::create_dir_all(&config.log_dir) {
            return Err(LogError::IoError(e.to_string()));
        }

        let sink = ParquetSink { sender: tx };
        let task_future = writer_task(
            rx,
            config.log_dir,
            config.flush_interval,
            config.buffer_size,
        );

        Ok((sink, task_future))
    }
}

#[async_trait]
impl LogSink for ParquetSink {
    async fn record(&self, event: LogEvent) -> Result<(), LogError> {
        self.sender
            .send(SinkCmd::Record(event))
            .await
            .map_err(|e| LogError::ChannelSend(e.to_string()))
    }

    async fn flush(&self) -> Result<(), LogError> {
        self.sender
            .send(SinkCmd::Flush)
            .await
            .map_err(|e| LogError::ChannelSend(e.to_string()))
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

/// Background writer task that owns the append file handles.
async fn writer_task(
    mut rx: mpsc::Receiver<SinkCmd>,
    log_dir: PathBuf,
    flush_interval: Duration,
    buffer_size: usize,
) {
    let mut pending: Vec<LogEvent> = Vec::with_capacity(buffer_size);
    let mut interval = time::interval(flush_interval);
    // Skip the first tick to avoid flushing immediately on startup.
    interval.tick().await;

    loop {
        tokio::select! {
            biased;

            cmd = rx.recv() => {
                match cmd {
                    Some(SinkCmd::Record(event)) => {
                        pending.push(event);
                        if pending.len() >= buffer_size {
                            if let Err(e) = flush_pending(&mut pending, &log_dir).await {
                                warn!("ParquetSink flush (buffer full) failed: {e}");
                            }
                        }
                    }
                    Some(SinkCmd::Flush) => {
                        if let Err(e) = flush_pending(&mut pending, &log_dir).await {
                            warn!("ParquetSink flush (explicit) failed: {e}");
                        }
                    }
                    Some(SinkCmd::Shutdown(ack)) => {
                        let result = flush_pending(&mut pending, &log_dir)
                            .await
                            .map_err(|e| LogError::IoError(e.to_string()));
                        let _ = ack.send(result);
                        info!("ParquetSink writer task: shutdown complete");
                        return;
                    }
                    None => {
                        if let Err(e) = flush_pending(&mut pending, &log_dir).await {
                            warn!("ParquetSink flush (channel closed) failed: {e}");
                        }
                        info!("ParquetSink writer task: channel closed, exiting");
                        return;
                    }
                }
            }

            _ = interval.tick() => {
                if !pending.is_empty() {
                    if let Err(e) = flush_pending(&mut pending, &log_dir).await {
                        warn!("ParquetSink flush (periodic) failed: {e}");
                    }
                }
            }
        }
    }
}

/// Append all pending events as JSONL to the appropriate day partition file.
async fn flush_pending(pending: &mut Vec<LogEvent>, log_dir: &Path) -> Result<(), LogError> {
    if pending.is_empty() {
        return Ok(());
    }

    // Group events by UTC day to write to the correct partition.
    // Events are typically from the same day, but we handle day-boundary rolls.
    let mut by_day: std::collections::HashMap<String, Vec<&LogEvent>> =
        std::collections::HashMap::new();

    for event in pending.iter() {
        let day = day_partition_key(event.ts_us);
        by_day.entry(day).or_default().push(event);
    }

    for (day, events) in &by_day {
        let partition_dir = log_dir.join(format!("day={day}"));
        // spawn_blocking because std::fs writes must not run on the tokio executor.
        let partition_dir_clone = partition_dir.clone();
        let lines: String = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap_or_else(|_| "{}".into()))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        tokio::task::spawn_blocking(move || -> Result<(), LogError> {
            std::fs::create_dir_all(&partition_dir_clone)
                .map_err(|e| LogError::IoError(e.to_string()))?;
            let file_path = partition_dir_clone.join("events.jsonl");
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
                .map_err(|e| LogError::IoError(e.to_string()))?;
            file.write_all(lines.as_bytes())
                .map_err(|e| LogError::IoError(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| LogError::IoError(e.to_string()))??;
    }

    pending.clear();
    Ok(())
}

/// Returns a `YYYY-MM-DD` string from a microsecond Unix timestamp.
fn day_partition_key(ts_us: i64) -> String {
    use chrono::{DateTime, Utc};
    let secs = ts_us / 1_000_000;
    let nanos = ((ts_us % 1_000_000) * 1_000) as u32;
    let dt = DateTime::<Utc>::from_timestamp(secs, nanos).unwrap_or_else(|| {
        DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is a valid timestamp")
    });
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
    async fn parquet_sink_flushes_to_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir created successfully");
        let config = ParquetSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = ParquetSink::new(config).expect("ParquetSink constructs successfully");
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
        let config = ParquetSinkConfig {
            log_dir: dir.path().to_path_buf(),
            buffer_size: 100,
            flush_interval: Duration::from_secs(3600),
        };

        let (sink, task) = ParquetSink::new(config).expect("ParquetSink constructs successfully");
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

    #[test]
    fn day_partition_key_correct() {
        // 2023-11-14 00:00:00 UTC in microseconds
        assert_eq!(day_partition_key(1_699_920_000_000_000), "2023-11-14");
    }
}
