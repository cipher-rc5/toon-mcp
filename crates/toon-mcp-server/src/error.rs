// file: crates/toon-mcp-server/src/error.rs
// description: Error types for toon-mcp-server using thiserror

use thiserror::Error;

/// Errors that can occur at the server binary level.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The logging sink failed to initialise.
    #[error("logging initialisation failed: {0}")]
    LoggingInit(#[from] toon_mcp_logging::LogError),

    /// The MCP service encountered a typed initialisation error.
    ///
    /// Preserves the upstream `source()` chain for downstream diagnostics
    /// (e.g. `anyhow::Error::chain` or `tracing` field capture).
    #[error("MCP service error: {0}")]
    McpServiceTyped(#[from] Box<rmcp::service::ServerInitializeError>),

    /// Invalid value for an environment variable.
    #[error("invalid config: {var} = {value:?}: {reason}")]
    InvalidConfig {
        /// The env var name.
        var: &'static str,
        /// The raw string value that was rejected.
        value: String,
        /// Human-readable explanation of why it was rejected.
        reason: &'static str,
    },
}
