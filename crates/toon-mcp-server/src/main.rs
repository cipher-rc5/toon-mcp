// file: crates/toon-mcp-server/src/main.rs
// description: Tokio entry point — wires config, logging, and MCP server together

mod config;
mod error;
mod handler;
mod server;

use std::sync::Arc;

use rmcp::{ServiceExt, transport::stdio};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

use toon_mcp_logging::{LogSink, NoopSink, ParquetSink};

use crate::{config::Config, server::ToonMcpServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let config = Config::load();

    init_tracing(&config.log_level);

    info!("toon-mcp-server starting");

    let sink: Arc<dyn LogSink> = if config.logging_enabled {
        let logging_config = config.logging.clone();
        let (sink, task) = ParquetSink::new(logging_config)?;
        tokio::spawn(task);
        Arc::new(sink)
    } else {
        Arc::new(NoopSink)
    };

    let server = ToonMcpServer::new(config, sink);
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("failed to start MCP service: {e}");
    })?;

    info!("toon-mcp-server ready");
    service.waiting().await?;
    info!("toon-mcp-server shutting down");

    Ok(())
}

/// Initialise the tracing subscriber with the given filter string.
fn init_tracing(log_level: &str) {
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
