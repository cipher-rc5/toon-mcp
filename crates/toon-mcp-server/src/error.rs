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
    ///
    /// This variant carries a stringified description because the historical
    /// call site in `main.rs` flattens the upstream error via
    /// `.map_err(|e| ServerError::McpService(e.to_string()))`. Source-chain
    /// preservation requires updating `main.rs` to use `?` over the typed
    /// [`Self::McpServiceTyped`] variant instead.
    #[error("MCP service error: {0}")]
    McpService(String),

    /// The MCP service encountered a typed initialisation error.
    ///
    /// Prefer this variant when the call site can propagate via `?`; it
    /// preserves the upstream `source()` chain for downstream diagnostics
    /// (e.g. `anyhow::Error::chain` or `tracing` field capture).
    #[error("MCP service error: {0}")]
    McpServiceTyped(#[from] Box<rmcp::service::ServerInitializeError>),
}
