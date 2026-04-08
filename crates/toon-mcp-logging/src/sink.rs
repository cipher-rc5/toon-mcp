// file: crates/toon-mcp-logging/src/sink.rs
// description: LogSink trait — the only interface between tool handlers and the logging layer

use crate::{error::LogError, event::LogEvent};
use async_trait::async_trait;

/// Abstraction over a structured event logger.
///
/// Implementations include:
/// - `ParquetSink`: appends events as JSONL to daily-partitioned files,
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
/// ```rust,ignore
/// Box::new(my_sink).shutdown().await
/// ```
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Record a single tool invocation event.
    async fn record(&self, event: LogEvent) -> Result<(), LogError>;

    /// Flush any buffered events to durable storage.
    async fn flush(&self) -> Result<(), LogError>;

    /// Flush, finalise, and cleanly shut down the sink.
    ///
    /// Takes `Box<Self>` to consume ownership while remaining object-safe.
    async fn shutdown(self: Box<Self>) -> Result<(), LogError>;
}
