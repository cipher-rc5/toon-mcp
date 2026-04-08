// file: crates/toon-mcp-server/src/config.rs
// description: Server configuration loaded from environment variables via dotenvy

use std::path::PathBuf;
use std::time::Duration;

use toon_format::Delimiter;
use toon_mcp_core::DEFAULT_MAX_INPUT_BYTES;
use toon_mcp_logging::ParquetSinkConfig;

/// Top-level configuration for the toon-mcp-server binary.
///
/// All values are loaded once in `main` from environment variables.
/// Callers pass this struct by value or behind an `Arc`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum output-to-input byte ratio accepted as "compressed".
    ///
    /// A value of `0.85` means the TOON output must be at most 85% of the
    /// original input byte count (i.e., at least 15% savings). A value of
    /// `1.0` accepts any output that is strictly smaller than the input.
    pub max_output_ratio: f64,

    /// Minimum input byte count for classification to run.
    pub min_bytes: usize,

    /// Maximum input byte count. Inputs larger than this are rejected
    /// immediately without parsing.
    pub max_input_bytes: usize,

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

    /// Per-call pipeline timeout in milliseconds. Calls exceeding this
    /// duration return an error rather than blocking indefinitely.
    pub pipeline_timeout_ms: u64,
}

impl Config {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// `dotenvy::dotenv().ok()` should be called before this function.
    /// Invalid values emit a `tracing::warn!` and fall back to the default.
    pub fn load() -> Self {
        let max_output_ratio = env_f64("TOON_COMPRESSION_THRESHOLD", 0.85);
        let min_bytes = env_usize("TOON_MIN_BYTES", 256);
        let max_input_bytes = env_usize("TOON_MAX_INPUT_BYTES", DEFAULT_MAX_INPUT_BYTES);
        let key_folding = env_bool("TOON_KEY_FOLDING", true);
        let delimiter = env_delimiter("TOON_DELIMITER", Delimiter::Comma);
        let tabular_min_rows = env_usize("TOON_TABULAR_MIN_ROWS", 3);
        let fold_min_depth = env_usize("TOON_FOLD_MIN_DEPTH", 3);
        let primitive_array_min = env_usize("TOON_PRIMITIVE_ARRAY_MIN", 5);
        let logging_enabled = env_bool("TOON_LOG_ENABLED", true);
        let log_level = std::env::var("TOON_LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let pipeline_timeout_ms = env_u64("TOON_PIPELINE_TIMEOUT_MS", 30_000);
        let client_hint = std::env::var("TOON_CLIENT_HINT")
            .ok()
            .filter(|s| !s.is_empty());

        let flush_interval_secs = env_u64("TOON_LOG_FLUSH_INTERVAL_SECS", 300);
        let buffer_size = env_usize("TOON_LOG_BUFFER_SIZE", 1000);
        let log_dir = std::env::var("TOON_LOG_DIR").unwrap_or_else(|_| "data/logs".into());

        // Validate threshold range.
        if !(0.0..=1.0).contains(&max_output_ratio) {
            tracing::warn!(
                value = max_output_ratio,
                "TOON_COMPRESSION_THRESHOLD is outside [0.0, 1.0]; using {max_output_ratio} but \
                 compression behaviour may be unexpected"
            );
        }
        if min_bytes == 0 {
            tracing::warn!(
                "TOON_MIN_BYTES=0 disables the minimum-bytes gate; \
                            all inputs including empty strings will be processed"
            );
        }

        Self {
            max_output_ratio,
            min_bytes,
            max_input_bytes,
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
            pipeline_timeout_ms,
        }
    }
}

// --- env helpers ---

fn env_f64(key: &str, default: f64) -> f64 {
    match std::env::var(key) {
        Ok(val) => match val.parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid f64 value for {key}; using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(val) => match val.parse::<usize>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid usize value for {key}; using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(val) => match val.parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid u64 value for {key}; using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key).as_deref() {
        Ok("true") | Ok("1") | Ok("yes") => true,
        Ok("false") | Ok("0") | Ok("no") => false,
        Ok(val) => {
            tracing::warn!(
                key,
                raw = val,
                default,
                "invalid bool value for {key}; using default {default}"
            );
            default
        }
        Err(_) => default,
    }
}

fn env_delimiter(key: &str, default: Delimiter) -> Delimiter {
    match std::env::var(key).as_deref() {
        Ok("comma") => Delimiter::Comma,
        Ok("tab") => Delimiter::Tab,
        Ok("pipe") => Delimiter::Pipe,
        Ok(val) => {
            tracing::warn!(
                key,
                raw = val,
                "invalid delimiter value for {key}; accepted: comma, tab, pipe; \
                 using default"
            );
            default
        }
        Err(_) => default,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Serialise all config tests to prevent env var pollution across threads.
    /// All tests that read or write `TOON_*` env vars must lock this before
    /// calling `Config::load()`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: run a closure with a single scoped env var, serialised via
    /// `ENV_LOCK`. The var is removed after `f` returns.
    fn with_env<F: FnOnce()>(key: &str, val: &str, f: F) {
        let _guard = ENV_LOCK.lock().expect("ENV_LOCK is unpoisoned");
        // SAFETY: We hold ENV_LOCK, so no concurrent thread is reading/writing
        // env vars in this test binary at the same time.
        unsafe {
            std::env::set_var(key, val);
        }
        f();
        unsafe {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn defaults_with_no_env() {
        let _guard = ENV_LOCK.lock().expect("ENV_LOCK is unpoisoned");
        let config = Config::load();
        assert!((config.max_output_ratio - 0.85).abs() < f64::EPSILON);
        assert_eq!(config.min_bytes, 256);
        assert_eq!(config.max_input_bytes, DEFAULT_MAX_INPUT_BYTES);
        assert!(config.key_folding);
        assert_eq!(config.tabular_min_rows, 3);
        assert_eq!(config.fold_min_depth, 3);
        assert_eq!(config.primitive_array_min, 5);
        assert!(config.logging_enabled);
        assert_eq!(config.log_level, "info");
        assert!(config.client_hint.is_none());
        assert_eq!(config.pipeline_timeout_ms, 30_000);
    }

    #[test]
    fn threshold_override() {
        with_env("TOON_COMPRESSION_THRESHOLD", "0.7", || {
            let config = Config::load();
            assert!((config.max_output_ratio - 0.7).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn invalid_f64_falls_back_to_default() {
        with_env("TOON_COMPRESSION_THRESHOLD", "not_a_number", || {
            let config = Config::load();
            assert!((config.max_output_ratio - 0.85).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn invalid_usize_falls_back_to_default() {
        with_env("TOON_MIN_BYTES", "abc", || {
            let config = Config::load();
            assert_eq!(config.min_bytes, 256);
        });
    }

    #[test]
    fn bool_truthy_values() {
        for val in ["true", "1", "yes"] {
            with_env("TOON_LOG_ENABLED", val, || {
                assert!(Config::load().logging_enabled, "expected true for {val}");
            });
        }
    }

    #[test]
    fn bool_falsy_values() {
        for val in ["false", "0", "no"] {
            with_env("TOON_LOG_ENABLED", val, || {
                assert!(!Config::load().logging_enabled, "expected false for {val}");
            });
        }
    }

    #[test]
    fn delimiter_pipe() {
        with_env("TOON_DELIMITER", "pipe", || {
            assert_eq!(Config::load().delimiter, Delimiter::Pipe);
        });
    }

    #[test]
    fn delimiter_tab() {
        with_env("TOON_DELIMITER", "tab", || {
            assert_eq!(Config::load().delimiter, Delimiter::Tab);
        });
    }

    #[test]
    fn empty_client_hint_becomes_none() {
        with_env("TOON_CLIENT_HINT", "", || {
            assert!(Config::load().client_hint.is_none());
        });
    }

    #[test]
    fn non_empty_client_hint_is_some() {
        with_env("TOON_CLIENT_HINT", "opencode", || {
            assert_eq!(Config::load().client_hint, Some("opencode".into()));
        });
    }

    #[test]
    fn max_input_bytes_override() {
        with_env("TOON_MAX_INPUT_BYTES", "1048576", || {
            assert_eq!(Config::load().max_input_bytes, 1_048_576);
        });
    }
}
