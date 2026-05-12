// file: crates/toon-mcp-logging/src/lib.rs
// description: Public API surface for toon-mcp-logging

#![deny(missing_docs)]

//! Async `LogSink` trait and implementations for structured event logging.
//!
//! Events are written as newline-delimited JSON to hive-partitioned files
//! queryable by DuckDB. The `LogSink` trait decouples tool handlers from
//! the concrete sink implementation.

/// Error types for logging operations.
pub mod error;
/// `LogEvent` struct representing one tool invocation record.
pub mod event;
/// Production JSONL sink backed by a background writer task.
pub mod jsonl_sink;
/// In-memory sink for integration tests.
pub mod memory_sink;
/// No-op sink that discards all events.
pub mod noop_sink;
/// `LogSink` async trait.
pub mod sink;

pub use error::LogError;
pub use event::LogEvent;
pub use jsonl_sink::{JsonlSink, JsonlSinkConfig};
pub use memory_sink::MemorySink;
pub use noop_sink::NoopSink;
pub use sink::{LogDiagnostics, LogSink};
