// file: crates/toon-mcp-logging/src/noop_sink.rs
// description: No-op LogSink implementation — discards all events

use crate::{
    error::LogError,
    event::LogEvent,
    sink::{LogDiagnostics, LogSink},
};
use async_trait::async_trait;

/// A `LogSink` that silently discards every event.
///
/// Used when `TOON_LOG_ENABLED=false`. Has zero overhead after construction.
pub struct NoopSink;

/// `NoopSink` reports zero events for every counter because nothing is
/// recorded — the diagnostics override signals "no telemetry available"
/// by leaving every queue gauge as `None`.
#[async_trait]
impl LogSink for NoopSink {
    async fn record(&self, _event: LogEvent) -> Result<(), LogError> {
        Ok(())
    }

    async fn flush(&self) -> Result<(), LogError> {
        Ok(())
    }

    fn diagnostics(&self) -> LogDiagnostics {
        LogDiagnostics::default()
    }

    async fn shutdown(self: Box<Self>) -> Result<(), LogError> {
        Ok(())
    }
}
