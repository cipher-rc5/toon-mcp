// file: crates/toon-mcp-server/src/main.rs
// description: Tokio entry point — wires config, logging, and MCP server together

mod config;
mod error;
mod handler;
mod server;

use std::sync::Arc;

use rmcp::{ServiceExt, transport::stdio};
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use toon_mcp_logging::{JsonlSink, LogSink, NoopSink};

use crate::{config::Config, error::ServerError, server::ToonMcpServer};

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    dotenvy::dotenv().ok();

    let config = Config::load();

    init_tracing(&config.log_level);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "toon-mcp-server starting"
    );

    // Construct the log sink and spawn the background writer task.
    // The sink is wrapped in Arc for shared access from the server handlers.
    let sink: Arc<dyn LogSink> = if config.logging_enabled {
        let logging_config = config.logging.clone();
        let (jsonl_sink, task) = JsonlSink::new(logging_config)?;
        let handle = tokio::spawn(task);

        // M3: Supervisor — treat unexpected writer task exit as fatal.
        // Log a structured error so operators can detect silent log loss.
        tokio::spawn(async move {
            match handle.await {
                Ok(()) => {
                    // Normal exit only happens on Shutdown command; unexpected
                    // here means the task completed without being told to.
                    error!(
                        component = "jsonl_sink_writer",
                        "writer task exited unexpectedly; subsequent log events will be dropped"
                    );
                }
                Err(e) => {
                    error!(
                        component = "jsonl_sink_writer",
                        error = %e,
                        "writer task panicked; subsequent log events will be dropped"
                    );
                }
            }
        });

        Arc::new(jsonl_sink)
    } else {
        Arc::new(NoopSink)
    };

    let server = ToonMcpServer::new(config.clone(), Arc::clone(&sink));
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| ServerError::McpService(e.to_string()))?;

    // M4: Structured readiness — stable anchor for log scrapers and monitors.
    info!(
        status = "ready",
        component = "toon-mcp-server",
        version = env!("CARGO_PKG_VERSION"),
        "toon-mcp-server ready"
    );

    // C3: Race the MCP service future against OS signals.
    // Either a clean stdin close or SIGTERM/SIGINT triggers graceful shutdown.
    #[cfg(unix)]
    let shutdown_signal = async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("SIGTERM handler registration is valid");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("SIGINT handler registration is valid");
        tokio::select! {
            _ = sigterm.recv() => { info!("received SIGTERM"); }
            _ = sigint.recv()  => { info!("received SIGINT");  }
        }
    };

    #[cfg(not(unix))]
    let shutdown_signal = async {
        tokio::signal::ctrl_c()
            .await
            .expect("ctrl_c handler registration is valid");
        info!("received Ctrl-C");
    };

    tokio::select! {
        result = service.waiting() => {
            if let Err(e) = result {
                error!(error = %e, "MCP service terminated with error");
            } else {
                info!("MCP service terminated cleanly (stdin closed)");
            }
        }
        _ = shutdown_signal => {
            info!("toon-mcp-server shutting down — flushing log sink");
        }
    }

    info!("toon-mcp-server shutting down — flushing log sink");

    // C2/C4: Use the acknowledged flush() which now sends a Flush command and
    // waits on a oneshot channel for the writer task to confirm all pending
    // events have been flushed to disk before returning Ok(()).
    // After the acknowledged flush we drop the Arc, which closes the channel
    // and causes the writer task to drain and exit cleanly on its next loop.
    if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(10), sink.flush()).await {
        warn!("log sink flush timed out after 10 s: {e}");
    }
    // Drop the Arc so the writer task's channel receiver sees closure.
    drop(sink);

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
