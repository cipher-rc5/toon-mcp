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

use crate::{config::Config, error::ServerError, server::ToonMcpServer};

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    dotenvy::dotenv().ok();

    let config = Config::load();

    init_tracing(&config.log_level);

    info!("toon-mcp-server starting");

    // Construct the log sink and spawn the background writer task.
    // The sink is wrapped in Arc for shared access from the server handlers.
    let sink: Arc<dyn LogSink> = if config.logging_enabled {
        let logging_config = config.logging.clone();
        let (parquet_sink, task) = ParquetSink::new(logging_config)?;
        let handle = tokio::spawn(task);

        // Supervisor: emit an error if the writer task exits unexpectedly so
        // the operator knows log events are being silently dropped.
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                tracing::error!(
                    "ParquetSink writer task terminated unexpectedly: {e}; \
                     subsequent log events will be silently dropped"
                );
            }
        });

        Arc::new(parquet_sink)
    } else {
        Arc::new(NoopSink)
    };

    let server = ToonMcpServer::new(config, Arc::clone(&sink));
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| ServerError::McpService(e.to_string()))?;

    info!("toon-mcp-server ready");
    service
        .waiting()
        .await
        .map_err(|e| ServerError::McpService(e.to_string()))?;

    info!("toon-mcp-server shutting down — flushing log sink");

    // Graceful shutdown: flush buffered log events before the process exits.
    // We call flush() on the Arc'd sink rather than the consuming shutdown()
    // because the server handlers may still hold Arc clones at this point.
    // The writer task will drain remaining events when its channel is dropped.
    if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(5), sink.flush()).await {
        tracing::warn!("log sink flush timed out after 5 s: {e}");
    }

    // Drop the Arc so the writer task's receiver sees channel closure and
    // performs a final flush before exiting.
    drop(sink);

    // Give the writer task a moment to complete its final flush.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    info!("toon-mcp-server exiting");
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
