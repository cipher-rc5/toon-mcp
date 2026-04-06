// file: crates/toon-mcp-logging/src/noop_sink.rs
// description: No-op LogSink implementation — discards all events

use crate::{error::LogError, event::LogEvent, sink::LogSink};
use async_trait::async_trait;

/// A `LogSink` that silently discards every event.
///
/// Used when `TOON_LOG_ENABLED=false`. Has zero overhead after construction.
pub struct NoopSink;

#[async_trait]
impl LogSink for NoopSink {
    async fn record(&self, _event: LogEvent) -> Result<(), LogError> {
        Ok(())
    }

    async fn flush(&self) -> Result<(), LogError> {
        Ok(())
    }

    async fn shutdown(self: Box<Self>) -> Result<(), LogError> {
        Ok(())
    }
}
