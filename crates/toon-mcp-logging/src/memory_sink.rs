// file: crates/toon-mcp-logging/src/memory_sink.rs
// description: In-memory LogSink for integration tests

use crate::{
    error::LogError,
    event::LogEvent,
    sink::{LogDiagnostics, LogSink, RecordOutcome},
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// A `LogSink` that collects all events into an in-memory `Vec`.
///
/// Intended for integration tests that need to assert on logged events
/// without touching the filesystem.
pub struct MemorySink {
    events: Arc<Mutex<Vec<LogEvent>>>,
}

impl MemorySink {
    /// Create a new empty sink and return both the sink and a shared handle
    /// to the event vector.
    pub fn new() -> (Self, Arc<Mutex<Vec<LogEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                events: Arc::clone(&events),
            },
            events,
        )
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new().0
    }
}

#[async_trait]
impl LogSink for MemorySink {
    async fn record(&self, event: LogEvent) -> Result<RecordOutcome, LogError> {
        self.events
            .lock()
            // Postcondition: only this task holds the lock; poisoning cannot
            // occur because we never panic while holding it.
            .expect("MemorySink mutex is unpoisoned")
            .push(event);
        Ok(RecordOutcome::ACCEPTED)
    }

    async fn flush(&self) -> Result<(), LogError> {
        Ok(())
    }

    fn diagnostics(&self) -> LogDiagnostics {
        LogDiagnostics {
            queue_queued: Some(
                self.events
                    .lock()
                    .expect("MemorySink mutex is unpoisoned")
                    .len(),
            ),
            ..LogDiagnostics::default()
        }
    }

    async fn shutdown(self: Box<Self>) -> Result<(), LogError> {
        Ok(())
    }
}
