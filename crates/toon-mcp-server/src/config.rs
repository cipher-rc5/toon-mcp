// file: crates/toon-mcp-server/src/config.rs
// description: Server configuration loaded from environment variables via dotenvy

use std::path::PathBuf;
use std::time::Duration;

use toon_format::Delimiter;
use toon_mcp_core::DEFAULT_MAX_INPUT_BYTES;
use toon_mcp_logging::JsonlSinkConfig;

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

    /// Whether CSV/TSV parsing coerces numeric-looking fields into numbers.
    ///
    /// Set to `false` for inputs containing identifiers, postal codes, or
    /// leading-zero values that should not be silently coerced.
    pub csv_numeric_coercion: bool,

    /// Whether structured logging is enabled.
    pub logging_enabled: bool,

    /// JSONL sink configuration (only meaningful when `logging_enabled`).
    pub logging: JsonlSinkConfig,

    /// tracing filter string (e.g. `"info"`, `"debug"`).
    pub log_level: String,

    /// Optional client identifier tag written to every log row.
    pub client_hint: Option<String>,

    /// Per-call pipeline timeout in milliseconds. Calls exceeding this
    /// duration return an error rather than blocking indefinitely.
    pub pipeline_timeout_ms: u64,

    /// Maximum number of concurrent blocking pipeline calls.
    ///
    /// Controls how many `spawn_blocking` dispatches can be in-flight at
    /// once. When the limit is reached, new calls wait up to
    /// `pipeline_timeout_ms` for a permit before returning a busy error.
    pub max_concurrent_calls: usize,
}

/// `Default` differs from `Config::load`: it has `logging_enabled: false` so
/// library consumers do not silently inherit a relative `data/logs` path. Use
/// `Config::load` for env-driven behaviour.
impl Default for Config {
    /// Returns a `Config` populated with safe defaults for library consumers.
    ///
    /// Unlike `Config::load`, this does NOT enable logging — a relative
    /// `data/logs` path would silently misdirect log files when the process
    /// working directory is unexpected (e.g. under a desktop process
    /// supervisor). Callers that want env-driven behaviour should use
    /// [`Config::load`] instead.
    fn default() -> Self {
        Self {
            max_output_ratio: 0.85,
            min_bytes: 256,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            key_folding: true,
            delimiter: Delimiter::Comma,
            tabular_min_rows: 3,
            fold_min_depth: 3,
            primitive_array_min: 5,
            csv_numeric_coercion: true,
            logging_enabled: false,
            logging: JsonlSinkConfig::default(),
            log_level: "info".into(),
            client_hint: None,
            pipeline_timeout_ms: 30_000,
            max_concurrent_calls: 8,
        }
    }
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
        let csv_numeric_coercion = env_bool("TOON_CSV_NUMERIC_COERCION", true);
        let logging_enabled = env_bool("TOON_LOG_ENABLED", true);
        let log_level = std::env::var("TOON_LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let pipeline_timeout_ms = env_u64("TOON_PIPELINE_TIMEOUT_MS", 30_000);
        let max_concurrent_calls = env_usize("TOON_MAX_CONCURRENT_CALLS", 8);
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

        // M2: Warn on relative log directory paths. Claude Desktop and many
        // process supervisors have an unpredictable working directory;
        // a relative path will silently misdirect log files.
        let log_dir_path = PathBuf::from(&log_dir);
        if logging_enabled && !log_dir_path.is_absolute() {
            tracing::warn!(
                path = log_dir,
                "TOON_LOG_DIR is a relative path; log files will be created relative to the \
                 process working directory which may be unexpected. Set an absolute path to \
                 avoid silent log misdirection (required for Claude Desktop)."
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
            csv_numeric_coercion,
            logging_enabled,
            logging: JsonlSinkConfig {
                log_dir: log_dir_path,
                buffer_size,
                flush_interval: Duration::from_secs(flush_interval_secs),
            },
            log_level,
            client_hint,
            pipeline_timeout_ms,
            max_concurrent_calls,
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
    use super::*;

    /// Helper: run a closure with a single scoped env var.
    /// Uses the `temp-env` crate for safe, thread-isolated env var overrides.
    fn with_env<F: FnOnce()>(key: &str, val: &str, f: F) {
        temp_env::with_var(key, Some(val), f);
    }

    #[test]
    fn defaults_with_no_env() {
        // Clear all TOON_* vars that could bleed from the environment.
        temp_env::with_vars(
            [
                ("TOON_COMPRESSION_THRESHOLD", None::<&str>),
                ("TOON_MIN_BYTES", None::<&str>),
                ("TOON_MAX_INPUT_BYTES", None::<&str>),
                ("TOON_KEY_FOLDING", None::<&str>),
                ("TOON_DELIMITER", None::<&str>),
                ("TOON_TABULAR_MIN_ROWS", None::<&str>),
                ("TOON_FOLD_MIN_DEPTH", None::<&str>),
                ("TOON_PRIMITIVE_ARRAY_MIN", None::<&str>),
                ("TOON_CSV_NUMERIC_COERCION", None::<&str>),
                ("TOON_LOG_ENABLED", None::<&str>),
                ("TOON_LOG_LEVEL", None::<&str>),
                ("TOON_CLIENT_HINT", None::<&str>),
                ("TOON_PIPELINE_TIMEOUT_MS", None::<&str>),
                ("TOON_MAX_CONCURRENT_CALLS", None::<&str>),
            ],
            || {
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
                assert_eq!(config.max_concurrent_calls, 8);
            },
        );
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

    #[test]
    fn max_concurrent_calls_override() {
        with_env("TOON_MAX_CONCURRENT_CALLS", "16", || {
            assert_eq!(Config::load().max_concurrent_calls, 16);
        });
    }

    #[test]
    fn csv_numeric_coercion_default() {
        // With no env var set, csv_numeric_coercion defaults to true to
        // preserve historical behaviour and maximise compression.
        temp_env::with_var("TOON_CSV_NUMERIC_COERCION", None::<&str>, || {
            assert!(Config::load().csv_numeric_coercion);
        });
    }

    #[test]
    fn csv_numeric_coercion_override() {
        with_env("TOON_CSV_NUMERIC_COERCION", "false", || {
            assert!(!Config::load().csv_numeric_coercion);
        });
    }
}
