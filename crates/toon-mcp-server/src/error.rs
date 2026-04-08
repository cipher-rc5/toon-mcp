// file: crates/toon-mcp-server/src/error.rs
// description: Error types for toon-mcp-server using thiserror

use thiserror::Error;

/// Errors that can occur at the server binary level.
#[derive(Debug, Error)]
pub enum ServerError {
    /// The logging sink failed to initialise.
    #[error("logging initialisation failed: {0}")]
    LoggingInit(#[from] toon_mcp_logging::LogError),

    /// The MCP service encountered a transport or protocol error.
    #[error("MCP service error: {0}")]
    McpService(String),
}
