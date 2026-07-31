// file: crates/toon-mcp-logging/src/sink.rs
// description: LogSink trait — the only interface between tool handlers and the logging layer

use crate::{error::LogError, event::LogEvent};
use async_trait::async_trait;

/// Snapshot of runtime logging health counters.
///
/// All counters are monotonic for the lifetime of a sink instance. Queue
/// fields are point-in-time gauges and are `None` for sinks that do not use a
/// bounded background channel.
#[derive(Debug, Clone, Default)]
pub struct LogDiagnostics {
    /// Number of events dropped because the bounded writer queue was full.
    pub record_dropped_count: u64,
    /// Number of record attempts that failed because the writer channel closed.
    pub record_failed_count: u64,
    /// Number of events skipped because JSON serialization failed.
    pub serialization_failed_count: u64,
    /// Number of writer flush attempts that failed after accepting events.
    pub writer_failed_count: u64,
    /// Last writer or channel error observed by the sink, if any.
    pub last_error: Option<String>,
    /// Total bounded queue capacity, if applicable.
    pub queue_capacity: Option<usize>,
    /// Currently queued commands, if applicable.
    pub queue_queued: Option<usize>,
    /// Currently available queue slots, if applicable.
    pub queue_available: Option<usize>,
}

/// Abstraction over a structured event logger.
///
/// Implementations include:
/// - `JsonlSink`: appends events as JSONL to daily-partitioned files,
///   queryable by DuckDB without acquiring a lock.
/// - `NoopSink`: discards all events (used when logging is disabled).
/// - `MemorySink`: accumulates events in memory (used in integration tests).
///
/// All tool handlers hold an `Arc<dyn LogSink>` and call `record` after each
/// invocation. The send is fire-and-forget from the handler's perspective.
///
/// # Shutdown ergonomics
///
/// `shutdown` takes `Box<Self>` rather than `&mut self` or `self` because
/// the trait must remain object-safe (`dyn LogSink` must be dispatchable)
/// while also consuming the sink to signal that no further calls are valid.
/// Callers should hold the concrete type in a `Box<dyn LogSink>` and call
/// `boxed_sink.shutdown().await`. The pattern at call sites is:
///
/// ```rust,no_run
/// # use toon_mcp_logging::sink::LogSink;
/// # use toon_mcp_logging::noop_sink::NoopSink;
/// # async fn example() {
/// let my_sink = NoopSink;
/// Box::new(my_sink).shutdown().await.unwrap();
/// # }
/// ```
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Record a single tool invocation event.
    async fn record(&self, event: LogEvent) -> Result<(), LogError>;

    /// Flush any buffered events to durable storage and wait for acknowledgement.
    ///
    /// Unlike a fire-and-forget send, the caller can rely on events being
    /// durable after this returns `Ok(())`. Disk-backed implementations call
    /// `sync_data` on the written files before acknowledging, so acknowledged
    /// events survive power loss, not merely process exit.
    async fn flush(&self) -> Result<(), LogError>;

    /// Return a point-in-time diagnostics snapshot for this sink.
    fn diagnostics(&self) -> LogDiagnostics {
        LogDiagnostics::default()
    }

    /// Flush, finalise, and cleanly shut down the sink.
    ///
    /// Takes `Box<Self>` to consume ownership while remaining object-safe.
    async fn shutdown(self: Box<Self>) -> Result<(), LogError>;
}
