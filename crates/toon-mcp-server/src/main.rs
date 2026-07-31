// file: crates/toon-mcp-server/src/main.rs
// description: Tokio entry point — wires config, logging, and MCP server together

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::{ServiceExt, transport::stdio};
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use toon_mcp_logging::{JsonlSink, LogSink, NoopSink};

use toon_mcp_server::{config::Config, error::ServerError, server::ToonMcpServer};

/// Sink handles wired up at startup: the shared handler sink, an optional
/// owned handle used to issue the acknowledged shutdown command at exit, and
/// the writer-task supervisor handle.
type SinkHandles = (
    Arc<dyn LogSink>,
    Option<Box<dyn LogSink>>,
    Option<tokio::task::JoinHandle<()>>,
);

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    dotenvy::dotenv().ok();

    // Install tracing BEFORE loading config: Config::load collects warnings
    // rather than logging them, and any warning emitted before the subscriber
    // exists would be silently dropped. The filter string is read directly
    // from the environment because the subscriber must exist first.
    init_tracing(&std::env::var("TOON_LOG_LEVEL").unwrap_or_else(|_| "info".into()));

    let (config, config_warnings) = Config::load()?;

    // Replay config warnings now that the subscriber is live.
    for w in &config_warnings {
        warn!(var = w.var, raw = %w.raw, "{}", w.reason);
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "toon-mcp-server starting"
    );

    // Shared flag used by the writer-task supervisor to distinguish a clean
    // shutdown (where main drops the sink) from an unexpected writer exit.
    let shutdown_initiated = Arc::new(AtomicBool::new(false));

    // Construct the log sink and spawn the background writer task.
    // The sink is wrapped in Arc for shared access from the server handlers;
    // main additionally keeps an owned boxed clone so it can issue the
    // acknowledged Shutdown command at exit (clones share the writer task).
    let (sink, sink_shutdown, supervisor_handle): SinkHandles = if config.logging_enabled {
        let logging_config = config.logging.clone();
        // `start` owns the spawn so callers cannot forget the writer task.
        let (jsonl_sink, handle) = JsonlSink::start(logging_config)?;

        // M3: Supervisor — treat unexpected writer task exit as fatal.
        // Log a structured error so operators can detect silent log loss.
        let supervisor = tokio::spawn(supervise_writer_task(
            handle,
            Arc::clone(&shutdown_initiated),
        ));

        let shutdown_handle: Box<dyn LogSink> = Box::new(jsonl_sink.clone());
        (
            Arc::new(jsonl_sink),
            Some(shutdown_handle),
            Some(supervisor),
        )
    } else {
        (Arc::new(NoopSink), None, None)
    };

    let server = ToonMcpServer::new(config.clone(), Arc::clone(&sink));
    let service = server.serve(stdio()).await.map_err(Box::new)?;

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
        // Register each handler gracefully: if registration fails, log a
        // structured error and treat that signal source as never-firing so the
        // server still runs and shuts down on stdin close instead of panicking.
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => Some(s),
            Err(e) => {
                error!(
                    error = %e,
                    signal = "SIGTERM",
                    "failed to register signal handler; shutdown via this signal disabled"
                );
                None
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => Some(s),
            Err(e) => {
                error!(
                    error = %e,
                    signal = "SIGINT",
                    "failed to register signal handler; shutdown via this signal disabled"
                );
                None
            }
        };
        // A `None` stream parks forever via `pending`, so only successfully
        // registered handlers can resolve the `select!`.
        let sigterm_fut = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let sigint_fut = async {
            match sigint.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = sigterm_fut => { info!("received SIGTERM"); }
            _ = sigint_fut  => { info!("received SIGINT");  }
        }
    };

    #[cfg(not(unix))]
    let shutdown_signal = async {
        // If ctrl_c handler registration fails, log a structured error and park
        // forever so shutdown still works via stdin close instead of panicking.
        match tokio::signal::ctrl_c().await {
            Ok(()) => info!("received Ctrl-C"),
            Err(e) => {
                error!(
                    error = %e,
                    "failed to register Ctrl-C handler; shutdown via Ctrl-C disabled"
                );
                std::future::pending::<()>().await
            }
        }
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

    // C2/C4: Issue the acknowledged shutdown() rather than flush(): the
    // writer task flushes all pending events AND emits its counter-summary
    // line before exiting, so operators get the final drop/failure totals.
    // Mark shutdown first so the supervisor classifies the writer exit as a
    // clean drain rather than an unexpected termination.
    shutdown_initiated.store(true, Ordering::Relaxed);
    if let Some(sink_shutdown) = sink_shutdown {
        match tokio::time::timeout(std::time::Duration::from_secs(10), sink_shutdown.shutdown())
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("log sink shutdown failed: {e}"),
            Err(e) => warn!("log sink shutdown timed out after 10 s: {e}"),
        }
    }
    // Drop the handler Arc so the writer task's channel receiver sees closure
    // even on the NoopSink path (where no shutdown command exists).
    drop(sink);

    // Await the supervisor task so the writer fully drains before exit. The
    // supervisor itself only completes once the writer JoinHandle resolves,
    // so this guarantees no pending events are lost on shutdown.
    if let Some(supervisor_handle) = supervisor_handle
        && tokio::time::timeout(std::time::Duration::from_secs(5), supervisor_handle)
            .await
            .is_err()
    {
        warn!("writer-task supervisor did not complete within 5 s");
    }

    info!("toon-mcp-server exiting");
    Ok(())
}

/// Supervise the JSONL writer task: log a structured error if it exits or
/// panics so operators can detect silent log loss.
///
/// The `shutdown_initiated` flag distinguishes a graceful shutdown (main
/// flushes the sink and drops the Arc, causing the writer to drain and exit
/// Ok normally) from an unexpected exit. A clean `Ok(())` during shutdown is
/// logged at `info`; outside of shutdown it remains an `error`.
pub(crate) async fn supervise_writer_task(
    handle: tokio::task::JoinHandle<()>,
    shutdown_initiated: Arc<AtomicBool>,
) {
    match handle.await {
        Ok(()) if shutdown_initiated.load(Ordering::Relaxed) => {
            info!(
                component = "jsonl_sink_writer",
                "writer task drained and exited cleanly during shutdown"
            );
        }
        Ok(()) => {
            // Normal exit outside of shutdown is unexpected — the task should
            // only complete after main sets the shutdown flag and drops the
            // sink.
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
}

/// Initialise the tracing subscriber with the given filter string.
fn init_tracing(log_level: &str) {
    // A malformed filter directive must not silently degrade to "info" — emit a
    // visible warning so operators notice the misconfiguration. This runs before
    // the tracing subscriber is installed, so `tracing::warn!` would be a no-op;
    // write to stderr directly (the project logs to stderr only, never stdout).
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| {
        eprintln!("warning: invalid log-filter directive {log_level:?}; falling back to \"info\"");
        EnvFilter::new("info")
    });

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer-task supervisor must absorb panics from the writer task
    /// without propagating them. Otherwise a panicking writer would also
    /// take down the supervisor task and leave operators with no signal.
    #[tokio::test]
    async fn supervise_writer_task_absorbs_panic() {
        let h = tokio::spawn(async { panic!("boom") });
        let flag = Arc::new(AtomicBool::new(false));
        // Call returns normally — no panic propagation.
        supervise_writer_task(h, flag).await;
    }

    /// The supervisor must also handle a normal (clean) task exit without
    /// panicking, logging the unexpected-exit message instead.
    #[tokio::test]
    async fn supervise_writer_task_handles_clean_exit() {
        let h = tokio::spawn(async {});
        let flag = Arc::new(AtomicBool::new(false));
        supervise_writer_task(h, flag).await;
    }

    /// A clean exit during shutdown (flag set) must not emit an error log.
    /// We can't easily intercept tracing here without subscriber wiring, so
    /// this exercise verifies the flag-set arm terminates cleanly. Code review
    /// confirms the path logs at `info!` rather than `error!`.
    #[tokio::test]
    async fn supervise_writer_task_clean_exit_during_shutdown() {
        let h = tokio::spawn(async {});
        let flag = Arc::new(AtomicBool::new(true));
        // If this path took the error arm it would still terminate, but the
        // contract is that the info arm is taken — verified by inspection of
        // supervise_writer_task above.
        supervise_writer_task(h, flag).await;
    }
}
