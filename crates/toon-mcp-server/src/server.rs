// file: crates/toon-mcp-server/src/server.rs
// description: rmcp ServerHandler implementation with tool_router macro
// reference: https://docs.rs/rmcp/latest/rmcp/

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::tool::ToolRouter,
    handler::server::wrapper::Json,
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};

use toon_mcp_logging::LogSink;

use crate::{
    config::Config,
    handler::{
        CompressParams, CompressResult, DetectParams, DetectResult, StatsParams, StatsResult,
        handle_compress_content, handle_compression_stats, handle_detect_format,
    },
};

/// The main MCP server struct. Implements `ServerHandler` via the `tool_handler` macro.
///
/// Holds shared state passed into every tool call. `Clone` is required by rmcp.
#[derive(Clone)]
pub struct ToonMcpServer {
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ToonMcpServer {
    /// Construct a new server with the given configuration and log sink.
    pub fn new(config: Config, log_sink: Arc<dyn LogSink>) -> Self {
        Self {
            config: Arc::new(config),
            log_sink,
            tool_router: Self::tool_router(),
        }
    }

    /// Detect the format of a structured input string.
    #[tool(description = "Detect the format of a structured input string. \
                          Returns the detected format (json, jsonl, csv, tsv, \
                          or unknown) and basic statistics.")]
    async fn detect_format(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<DetectParams>,
    ) -> Result<Json<DetectResult>, McpError> {
        handle_detect_format(params, Arc::clone(&self.config), Arc::clone(&self.log_sink))
            .await
            .map(Json)
    }

    /// Compress structured content to TOON format for token efficiency.
    #[tool(description = "Compress structured content (JSON, JSONL, CSV, TSV) \
                          to TOON format for token efficiency. Returns the \
                          compressed TOON string when savings exceed the \
                          configured threshold, or the original input unchanged. \
                          TOON is human-readable — interpret it directly.")]
    async fn compress_content(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CompressParams>,
    ) -> Result<Json<CompressResult>, McpError> {
        handle_compress_content(params, Arc::clone(&self.config), Arc::clone(&self.log_sink))
            .await
            .map(Json)
    }

    /// Preview compression statistics without encoding.
    #[tool(description = "Preview compression statistics without encoding. \
                          Returns format detection, shape classification, and \
                          estimated token savings. Use before compress_content \
                          to decide whether compression is worthwhile.")]
    async fn compression_stats(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<StatsParams>,
    ) -> Result<Json<StatsResult>, McpError> {
        handle_compression_stats(params, Arc::clone(&self.config), Arc::clone(&self.log_sink))
            .await
            .map(Json)
    }
}

#[tool_handler]
impl ServerHandler for ToonMcpServer {
    /// Return the server capabilities and instructions registered once at session handshake.
    ///
    /// No system prompt injection is performed on individual messages; these
    /// instructions are sent once during the MCP session initialisation.
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Compresses structured data (JSON, JSONL, CSV, TSV) to TOON \
                 format to reduce token consumption in context windows. \
                 Workflow: call detect_format to identify input type, \
                 compression_stats to preview savings, then compress_content \
                 to encode. Pass-through is automatic when savings are \
                 insufficient. TOON output is human-readable — no decoding \
                 step is required before use."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}
