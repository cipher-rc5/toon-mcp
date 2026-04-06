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
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Record a single tool invocation event.
    async fn record(&self, event: LogEvent) -> Result<(), LogError>;

    /// Flush any buffered events to durable storage.
    async fn flush(&self) -> Result<(), LogError>;

    /// Flush, finalise, and cleanly shut down the sink.
    async fn shutdown(self: Box<Self>) -> Result<(), LogError>;
}
