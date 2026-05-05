// file: crates/toon-mcp-logging/src/error.rs
// description: Error types for toon-mcp-logging using thiserror

use thiserror::Error;

/// Errors that can occur during logging operations.
#[derive(Debug, Error)]
pub enum LogError {
    /// A filesystem I/O error occurred while writing a log partition file.
    #[error("log I/O error: {0}")]
    IoError(#[source] std::io::Error),

    /// Sending an event to the background writer task failed because the
    /// channel was closed (writer task exited unexpectedly).
    #[error("log channel send failed: {0}")]
    ChannelSend(String),

    /// A shutdown acknowledgement was not received from the writer task.
    #[error("shutdown acknowledgement not received: {0}")]
    ShutdownAck(String),
}
