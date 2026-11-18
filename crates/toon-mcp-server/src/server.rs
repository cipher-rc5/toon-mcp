// file: crates/toon-mcp-server/src/server.rs
// description: rmcp ServerHandler implementation and tool router for ToonMcpServer

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::tool::IntoCallToolResult,
    handler::server::wrapper::{Json, Parameters},
    model::{
        CallToolResponse, CallToolResult, ContentBlock, ErrorCode, GetSkillRequestParams,
        GetSkillResult, Implementation, ListSkillsResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
        ServerCapabilities, ServerConfig, Skill, SkillFrontmatter, SkillResource, SkillResources,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use tokio::sync::Semaphore;

use toon_mcp_logging::LogSink;

use crate::{
    config::Config,
    handler::{
        CompressParams, CompressResult, DetectParams, DetectResult, DiagnosticsParams,
        DiagnosticsResult, Metrics, StatsParams, StatsResult, handle_compress_content_inner,
        handle_compression_stats, handle_detect_format, handle_toon_diagnostics,
    },
};

/// The caller can correct rejected tool arguments. Keep infrastructure
/// failures on the JSON-RPC error channel.
struct ToolFailure(McpError);

impl IntoCallToolResult for ToolFailure {
    fn into_call_tool_result(self) -> Result<CallToolResponse, McpError> {
        if self.0.code == ErrorCode::INVALID_PARAMS {
            Ok(CallToolResult::error(vec![ContentBlock::text(self.0.message)]).into())
        } else {
            Err(self.0)
        }
    }
}

/// MCP server. Implements `ServerHandler` via the `tool_handler` macro.
/// `Clone` is required by rmcp.
#[derive(Clone)]
pub struct ToonMcpServer {
    config: Arc<Config>,
    log_sink: Arc<dyn LogSink>,
    metrics: Arc<Metrics>,
    semaphore: Arc<Semaphore>,
}

#[tool_router]
impl ToonMcpServer {
    /// Construct a new server with the given configuration and log sink.
    pub fn new(config: Config, log_sink: Arc<dyn LogSink>) -> Self {
        let max_concurrent = config.max_concurrent_calls;
        Self {
            config: Arc::new(config),
            log_sink,
            metrics: Arc::new(Metrics::default()),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    #[tool(description = "Detect the format of a structured input string. \
                          Returns the detected format (json, jsonl, csv, tsv, \
                          or unknown) and basic statistics.")]
    async fn detect_format(
        &self,
        Parameters(params): Parameters<DetectParams>,
    ) -> Result<Json<DetectResult>, ToolFailure> {
        handle_detect_format(
            params,
            Arc::clone(&self.config),
            Arc::clone(&self.log_sink),
            Arc::clone(&self.metrics),
            Arc::clone(&self.semaphore),
        )
        .await
        .map(Json)
        .map_err(ToolFailure)
    }

    #[tool(description = "Compress structured content (JSON, JSONL, CSV, TSV) \
                          to TOON format for token efficiency. Returns the \
                          compressed TOON string when savings exceed the \
                          configured threshold, or the original input unchanged. \
                          TOON is human-readable — interpret it directly.")]
    async fn compress_content(
        &self,
        Parameters(params): Parameters<CompressParams>,
    ) -> Result<Json<CompressResult>, ToolFailure> {
        handle_compress_content_inner(
            params,
            Arc::clone(&self.config),
            Arc::clone(&self.log_sink),
            Arc::clone(&self.metrics),
            Arc::clone(&self.semaphore),
        )
        .await
        .map(Json)
        .map_err(ToolFailure)
    }

    #[tool(
        description = "Preview compression statistics without returning the encoded text. \
                          Returns format detection, shape classification, and \
                          estimated token savings. Use before compress_content \
                          to decide whether compression is worthwhile."
    )]
    async fn compression_stats(
        &self,
        Parameters(params): Parameters<StatsParams>,
    ) -> Result<Json<StatsResult>, ToolFailure> {
        handle_compression_stats(
            params,
            Arc::clone(&self.config),
            Arc::clone(&self.log_sink),
            Arc::clone(&self.metrics),
            Arc::clone(&self.semaphore),
        )
        .await
        .map(Json)
        .map_err(ToolFailure)
    }

    #[tool(
        description = "Return production diagnostics for logging health, log-loss counters, \
                          writer failures, queue saturation, pipeline timeouts, request \
                          durations, and concurrency permits."
    )]
    async fn toon_diagnostics(
        &self,
        Parameters(params): Parameters<DiagnosticsParams>,
    ) -> Result<Json<DiagnosticsResult>, McpError> {
        // Deliberately takes no concurrency permit: this tool runs no
        // blocking pipeline work, and it must stay responsive when the
        // semaphore is saturated — diagnosing that saturation is its job.
        handle_toon_diagnostics(
            params,
            Arc::clone(&self.config),
            Arc::clone(&self.log_sink),
            Arc::clone(&self.metrics),
            Arc::clone(&self.semaphore),
        )
        .await
        .map(Json)
    }
}

#[tool_handler]
impl ServerHandler for ToonMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().enable_skills().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions("Use compress_content to compress structured text to TOON. Use compression_stats for a preview or detect_format to identify an input.")
    }

    async fn list_skills(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        if request.and_then(|params| params.cursor).is_some() {
            return Err(McpError::invalid_params("Unknown skill cursor", None));
        }
        Ok(ListSkillsResult::with_all_items(vec![compression_skill()]))
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        if request.uri != SKILL_URI {
            return Err(McpError::invalid_params("Unknown skill URI", None));
        }
        Ok(GetSkillResult::new(compression_skill()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != SKILL_URI {
            return Err(McpError::resource_not_found("Unknown resource URI", None));
        }
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(SKILL_MD, SKILL_URI).with_mime_type("text/markdown"),
        ])
        .into())
    }
}

const SKILL_URI: &str = "skill://toon-mcp/SKILL.md";
const SKILL_MD: &str = include_str!("../skills/toon-mcp/SKILL.md");

fn compression_skill() -> Skill {
    Skill::new(
        SKILL_URI,
        SkillFrontmatter::new(
            "toon-mcp",
            "Compress JSON, JSONL, CSV, or TSV with the toon-mcp server when structured data needs to fit in an agent's context window.",
        ),
        SkillResources::Files(vec![SkillResource::from_bytes(
            SKILL_URI,
            SKILL_MD.as_bytes(),
        )]),
    )
}
