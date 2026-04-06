// file: crates/toon-mcp-server/src/config.rs
// description: Server configuration loaded from environment variables via dotenvy

use std::path::PathBuf;
use std::time::Duration;

use toon_format::Delimiter;
use toon_mcp_logging::ParquetSinkConfig;

/// Top-level configuration for the toon-mcp-server binary.
///
/// All values are loaded once in `main` from environment variables.
/// Callers pass this struct by value or behind an `Arc`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Fraction threshold below which TOON must reduce the input to be accepted.
    /// `toon_bytes < input_bytes * threshold` triggers compression.
    pub compression_threshold: f64,

    /// Minimum input byte count for classification to run.
    pub min_bytes: usize,

    /// Whether TOON key folding is enabled for FoldChain shapes.
    pub key_folding: bool,

    /// Array delimiter used in TOON output.
    pub delimiter: Delimiter,

    /// Minimum array length for Tabular classification.
    pub tabular_min_rows: usize,

    /// Minimum chain depth for FoldChain classification.
    pub fold_min_depth: usize,

    /// Minimum array length for PrimitiveArray classification.
    pub primitive_array_min: usize,

    /// Whether structured logging is enabled.
    pub logging_enabled: bool,

    /// Parquet sink configuration (only meaningful when `logging_enabled`).
    pub logging: ParquetSinkConfig,

    /// tracing filter string (e.g. `"info"`, `"debug"`).
    pub log_level: String,

    /// Optional client identifier tag written to every log row.
    pub client_hint: Option<String>,
}

impl Config {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// `dotenvy::dotenv().ok()` should be called before this function.
    pub fn load() -> Self {
        let threshold = env_f64("TOON_COMPRESSION_THRESHOLD", 0.85);
        let min_bytes = env_usize("TOON_MIN_BYTES", 256);
        let key_folding = env_bool("TOON_KEY_FOLDING", true);
        let delimiter = env_delimiter("TOON_DELIMITER", Delimiter::Comma);
        let tabular_min_rows = env_usize("TOON_TABULAR_MIN_ROWS", 3);
        let fold_min_depth = env_usize("TOON_FOLD_MIN_DEPTH", 3);
        let primitive_array_min = env_usize("TOON_PRIMITIVE_ARRAY_MIN", 5);
        let logging_enabled = env_bool("TOON_LOG_ENABLED", true);
        let log_level = std::env::var("TOON_LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let client_hint = std::env::var("TOON_CLIENT_HINT")
            .ok()
            .filter(|s| !s.is_empty());

        let flush_interval_secs = env_u64("TOON_LOG_FLUSH_INTERVAL_SECS", 300);
        let buffer_size = env_usize("TOON_LOG_BUFFER_SIZE", 1000);
        let log_dir = std::env::var("TOON_LOG_DIR").unwrap_or_else(|_| "data/logs".into());

        Self {
            compression_threshold: threshold,
            min_bytes,
            key_folding,
            delimiter,
            tabular_min_rows,
            fold_min_depth,
            primitive_array_min,
            logging_enabled,
            logging: ParquetSinkConfig {
                log_dir: PathBuf::from(log_dir),
                buffer_size,
                flush_interval: Duration::from_secs(flush_interval_secs),
            },
            log_level,
            client_hint,
        }
    }
}

// --- env helpers ---

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key).as_deref() {
        Ok("true") | Ok("1") | Ok("yes") => true,
        Ok("false") | Ok("0") | Ok("no") => false,
        _ => default,
    }
}

fn env_delimiter(key: &str, default: Delimiter) -> Delimiter {
    match std::env::var(key).as_deref() {
        Ok("comma") => Delimiter::Comma,
        Ok("tab") => Delimiter::Tab,
        Ok("pipe") => Delimiter::Pipe,
        _ => default,
    }
}
