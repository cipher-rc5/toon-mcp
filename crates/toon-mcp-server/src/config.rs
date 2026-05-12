// file: crates/toon-mcp-server/src/config.rs
// description: Server configuration loaded from environment variables via dotenvy

use std::path::PathBuf;
use std::time::Duration;

use toon_format::Delimiter;
use toon_mcp_core::DEFAULT_MAX_INPUT_BYTES;
use toon_mcp_logging::JsonlSinkConfig;

use crate::error::ServerError;

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

    /// Whether unparseable environment variables fail startup instead of
    /// falling back to defaults.
    pub strict_config: bool,
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
            strict_config: false,
        }
    }
}

impl Config {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// `dotenvy::dotenv().ok()` should be called before this function.
    ///
    /// Returns `Err(ServerError::InvalidConfig { .. })` when an env var is set
    /// to a value that violates a documented constraint (e.g. zero for a
    /// positive-integer variable, or a compression threshold outside
    /// `[0.0, 1.0]`). Unparseable values (typos) are logged at `warn` and fall
    /// back to the default unless `TOON_CONFIG_STRICT=true`, in which case
    /// they are rejected.
    pub fn load() -> Result<Self, ServerError> {
        let strict_config = env_bool_result("TOON_CONFIG_STRICT", false, false)?;
        let max_output_ratio =
            env_f64_in_range("TOON_COMPRESSION_THRESHOLD", 0.85, 0.0, 1.0, strict_config)?;
        let min_bytes = env_usize("TOON_MIN_BYTES", 256, strict_config)?;
        let max_input_bytes = env_usize_positive(
            "TOON_MAX_INPUT_BYTES",
            DEFAULT_MAX_INPUT_BYTES,
            strict_config,
        )?;
        let key_folding = env_bool_result("TOON_KEY_FOLDING", true, strict_config)?;
        let delimiter = env_delimiter("TOON_DELIMITER", Delimiter::Comma, strict_config)?;
        let tabular_min_rows = env_usize("TOON_TABULAR_MIN_ROWS", 3, strict_config)?;
        let fold_min_depth = env_usize("TOON_FOLD_MIN_DEPTH", 3, strict_config)?;
        let primitive_array_min = env_usize("TOON_PRIMITIVE_ARRAY_MIN", 5, strict_config)?;
        let csv_numeric_coercion =
            env_bool_result("TOON_CSV_NUMERIC_COERCION", true, strict_config)?;
        let logging_enabled = env_bool_result("TOON_LOG_ENABLED", true, strict_config)?;
        let log_level = std::env::var("TOON_LOG_LEVEL").unwrap_or_else(|_| "info".into());
        let pipeline_timeout_ms =
            env_u64_positive("TOON_PIPELINE_TIMEOUT_MS", 30_000, strict_config)?;
        let max_concurrent_calls =
            env_usize_positive("TOON_MAX_CONCURRENT_CALLS", 8, strict_config)?;
        let client_hint = std::env::var("TOON_CLIENT_HINT")
            .ok()
            .filter(|s| !s.is_empty());

        let flush_interval_secs =
            env_u64_positive("TOON_LOG_FLUSH_INTERVAL_SECS", 300, strict_config)?;
        let buffer_size = env_usize_positive("TOON_LOG_BUFFER_SIZE", 1000, strict_config)?;
        let log_dir = std::env::var("TOON_LOG_DIR").unwrap_or_else(|_| "data/logs".into());

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

        Ok(Self {
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
            strict_config,
        })
    }
}

// --- env helpers ---

fn env_f64_in_range(
    key: &'static str,
    default: f64,
    min: f64,
    max: f64,
    strict: bool,
) -> Result<f64, ServerError> {
    match std::env::var(key) {
        Ok(val) => match val.parse::<f64>() {
            Ok(v) => {
                if (min..=max).contains(&v) {
                    Ok(v)
                } else {
                    Err(ServerError::InvalidConfig {
                        var: key,
                        value: val,
                        reason: "value must be within [0.0, 1.0] inclusive",
                    })
                }
            }
            Err(_) => {
                if strict {
                    return Err(ServerError::InvalidConfig {
                        var: key,
                        value: val,
                        reason: "value must be parseable as f64",
                    });
                }
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid f64 value for {key}; using default {default}"
                );
                Ok(default)
            }
        },
        Err(_) => Ok(default),
    }
}

fn env_usize(key: &'static str, default: usize, strict: bool) -> Result<usize, ServerError> {
    match std::env::var(key) {
        Ok(val) => match val.parse::<usize>() {
            Ok(v) => Ok(v),
            Err(_) => {
                if strict {
                    return Err(ServerError::InvalidConfig {
                        var: key,
                        value: val,
                        reason: "value must be parseable as usize",
                    });
                }
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid usize value for {key}; using default {default}"
                );
                Ok(default)
            }
        },
        Err(_) => Ok(default),
    }
}

fn env_usize_positive(
    key: &'static str,
    default: usize,
    strict: bool,
) -> Result<usize, ServerError> {
    match std::env::var(key) {
        Ok(val) => match val.parse::<usize>() {
            Ok(0) => Err(ServerError::InvalidConfig {
                var: key,
                value: val,
                reason: "value must be >= 1",
            }),
            Ok(v) => Ok(v),
            Err(_) => {
                if strict {
                    return Err(ServerError::InvalidConfig {
                        var: key,
                        value: val,
                        reason: "value must be parseable as usize",
                    });
                }
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid usize value for {key}; using default {default}"
                );
                Ok(default)
            }
        },
        Err(_) => Ok(default),
    }
}

fn env_u64_positive(key: &'static str, default: u64, strict: bool) -> Result<u64, ServerError> {
    match std::env::var(key) {
        Ok(val) => match val.parse::<u64>() {
            Ok(0) => Err(ServerError::InvalidConfig {
                var: key,
                value: val,
                reason: "value must be >= 1",
            }),
            Ok(v) => Ok(v),
            Err(_) => {
                if strict {
                    return Err(ServerError::InvalidConfig {
                        var: key,
                        value: val,
                        reason: "value must be parseable as u64",
                    });
                }
                tracing::warn!(
                    key,
                    raw = %val,
                    default,
                    "invalid u64 value for {key}; using default {default}"
                );
                Ok(default)
            }
        },
        Err(_) => Ok(default),
    }
}

fn env_bool_result(key: &'static str, default: bool, strict: bool) -> Result<bool, ServerError> {
    match std::env::var(key).as_deref() {
        Ok("true") | Ok("1") | Ok("yes") => Ok(true),
        Ok("false") | Ok("0") | Ok("no") => Ok(false),
        Ok(val) => {
            if strict {
                return Err(ServerError::InvalidConfig {
                    var: key,
                    value: val.to_owned(),
                    reason: "value must be a boolean: true, false, 1, 0, yes, or no",
                });
            }
            tracing::warn!(
                key,
                raw = val,
                default,
                "invalid bool value for {key}; using default {default}"
            );
            Ok(default)
        }
        Err(_) => Ok(default),
    }
}

fn env_delimiter(
    key: &'static str,
    default: Delimiter,
    strict: bool,
) -> Result<Delimiter, ServerError> {
    match std::env::var(key).as_deref() {
        Ok("comma") => Ok(Delimiter::Comma),
        Ok("tab") => Ok(Delimiter::Tab),
        Ok("pipe") => Ok(Delimiter::Pipe),
        Ok(val) => {
            if strict {
                return Err(ServerError::InvalidConfig {
                    var: key,
                    value: val.to_owned(),
                    reason: "value must be one of: comma, tab, pipe",
                });
            }
            tracing::warn!(
                key,
                raw = val,
                "invalid delimiter value for {key}; accepted: comma, tab, pipe; \
                 using default"
            );
            Ok(default)
        }
        Err(_) => Ok(default),
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
        temp_env::with_vars([(key, Some(val)), ("TOON_CONFIG_STRICT", None::<&str>)], f);
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
                ("TOON_CONFIG_STRICT", None::<&str>),
            ],
            || {
                let config = Config::load().expect("defaults must load successfully");
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
                assert!(!config.strict_config);
            },
        );
    }

    #[test]
    fn threshold_override() {
        with_env("TOON_COMPRESSION_THRESHOLD", "0.7", || {
            let config = Config::load().expect("0.7 is a valid threshold");
            assert!((config.max_output_ratio - 0.7).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn invalid_f64_falls_back_to_default() {
        temp_env::with_vars(
            [
                ("TOON_CONFIG_STRICT", None::<&str>),
                ("TOON_COMPRESSION_THRESHOLD", Some("not_a_number")),
            ],
            || {
                let config = Config::load().expect("unparseable falls back to default");
                assert!((config.max_output_ratio - 0.85).abs() < f64::EPSILON);
            },
        );
    }

    #[test]
    fn invalid_usize_falls_back_to_default() {
        temp_env::with_vars(
            [
                ("TOON_CONFIG_STRICT", None::<&str>),
                ("TOON_MIN_BYTES", Some("abc")),
            ],
            || {
                let config = Config::load().expect("unparseable usize falls back to default");
                assert_eq!(config.min_bytes, 256);
            },
        );
    }

    #[test]
    fn strict_config_rejects_unparseable_f64() {
        temp_env::with_vars(
            [
                ("TOON_CONFIG_STRICT", Some("true")),
                ("TOON_COMPRESSION_THRESHOLD", Some("not_a_number")),
            ],
            || match Config::load() {
                Err(ServerError::InvalidConfig { var, .. }) => {
                    assert_eq!(var, "TOON_COMPRESSION_THRESHOLD");
                }
                other => panic!("expected InvalidConfig for strict f64, got {other:?}"),
            },
        );
    }

    #[test]
    fn strict_config_rejects_unparseable_bool() {
        temp_env::with_vars(
            [
                ("TOON_CONFIG_STRICT", Some("true")),
                ("TOON_LOG_ENABLED", Some("maybe")),
            ],
            || match Config::load() {
                Err(ServerError::InvalidConfig { var, .. }) => {
                    assert_eq!(var, "TOON_LOG_ENABLED");
                }
                other => panic!("expected InvalidConfig for strict bool, got {other:?}"),
            },
        );
    }

    #[test]
    fn bool_truthy_values() {
        for val in ["true", "1", "yes"] {
            with_env("TOON_LOG_ENABLED", val, || {
                assert!(
                    Config::load().expect("valid bool").logging_enabled,
                    "expected true for {val}"
                );
            });
        }
    }

    #[test]
    fn bool_falsy_values() {
        for val in ["false", "0", "no"] {
            with_env("TOON_LOG_ENABLED", val, || {
                assert!(
                    !Config::load().expect("valid bool").logging_enabled,
                    "expected false for {val}"
                );
            });
        }
    }

    #[test]
    fn delimiter_pipe() {
        with_env("TOON_DELIMITER", "pipe", || {
            assert_eq!(
                Config::load().expect("valid delim").delimiter,
                Delimiter::Pipe
            );
        });
    }

    #[test]
    fn delimiter_tab() {
        with_env("TOON_DELIMITER", "tab", || {
            assert_eq!(
                Config::load().expect("valid delim").delimiter,
                Delimiter::Tab
            );
        });
    }

    #[test]
    fn empty_client_hint_becomes_none() {
        with_env("TOON_CLIENT_HINT", "", || {
            assert!(Config::load().expect("ok").client_hint.is_none());
        });
    }

    #[test]
    fn non_empty_client_hint_is_some() {
        with_env("TOON_CLIENT_HINT", "opencode", || {
            assert_eq!(
                Config::load().expect("ok").client_hint,
                Some("opencode".into())
            );
        });
    }

    #[test]
    fn max_input_bytes_override() {
        with_env("TOON_MAX_INPUT_BYTES", "1048576", || {
            assert_eq!(Config::load().expect("ok").max_input_bytes, 1_048_576);
        });
    }

    #[test]
    fn max_concurrent_calls_override() {
        with_env("TOON_MAX_CONCURRENT_CALLS", "16", || {
            assert_eq!(Config::load().expect("ok").max_concurrent_calls, 16);
        });
    }

    #[test]
    fn csv_numeric_coercion_default() {
        // With no env var set, csv_numeric_coercion defaults to true to
        // preserve historical behaviour and maximise compression.
        temp_env::with_var("TOON_CSV_NUMERIC_COERCION", None::<&str>, || {
            assert!(Config::load().expect("ok").csv_numeric_coercion);
        });
    }

    #[test]
    fn csv_numeric_coercion_override() {
        with_env("TOON_CSV_NUMERIC_COERCION", "false", || {
            assert!(!Config::load().expect("ok").csv_numeric_coercion);
        });
    }

    // --- Validation rejection tests ---

    #[test]
    fn rejects_zero_buffer_size() {
        with_env("TOON_LOG_BUFFER_SIZE", "0", || match Config::load() {
            Err(ServerError::InvalidConfig { var, .. }) => {
                assert_eq!(var, "TOON_LOG_BUFFER_SIZE");
            }
            other => panic!("expected InvalidConfig for buffer_size=0, got {other:?}"),
        });
    }

    #[test]
    fn rejects_zero_flush_interval() {
        with_env("TOON_LOG_FLUSH_INTERVAL_SECS", "0", || {
            match Config::load() {
                Err(ServerError::InvalidConfig { var, .. }) => {
                    assert_eq!(var, "TOON_LOG_FLUSH_INTERVAL_SECS");
                }
                other => panic!("expected InvalidConfig for flush_interval=0, got {other:?}"),
            }
        });
    }

    #[test]
    fn rejects_zero_max_concurrent_calls() {
        with_env("TOON_MAX_CONCURRENT_CALLS", "0", || match Config::load() {
            Err(ServerError::InvalidConfig { var, .. }) => {
                assert_eq!(var, "TOON_MAX_CONCURRENT_CALLS");
            }
            other => panic!("expected InvalidConfig for max_concurrent_calls=0, got {other:?}"),
        });
    }

    #[test]
    fn rejects_negative_threshold() {
        with_env(
            "TOON_COMPRESSION_THRESHOLD",
            "-0.1",
            || match Config::load() {
                Err(ServerError::InvalidConfig { var, .. }) => {
                    assert_eq!(var, "TOON_COMPRESSION_THRESHOLD");
                }
                other => panic!("expected InvalidConfig for threshold=-0.1, got {other:?}"),
            },
        );
    }

    #[test]
    fn rejects_threshold_above_one() {
        with_env("TOON_COMPRESSION_THRESHOLD", "1.5", || {
            match Config::load() {
                Err(ServerError::InvalidConfig { var, .. }) => {
                    assert_eq!(var, "TOON_COMPRESSION_THRESHOLD");
                }
                other => panic!("expected InvalidConfig for threshold=1.5, got {other:?}"),
            }
        });
    }

    #[test]
    fn accepts_threshold_one_inclusive() {
        with_env("TOON_COMPRESSION_THRESHOLD", "1.0", || {
            let config = Config::load().expect("1.0 must be accepted");
            assert!((config.max_output_ratio - 1.0).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn accepts_threshold_zero_inclusive() {
        with_env("TOON_COMPRESSION_THRESHOLD", "0.0", || {
            let config = Config::load().expect("0.0 must be accepted");
            assert!(config.max_output_ratio.abs() < f64::EPSILON);
        });
    }

    #[test]
    fn accepts_zero_min_bytes() {
        with_env("TOON_MIN_BYTES", "0", || {
            let config = Config::load().expect("min_bytes=0 is documented as allowed");
            assert_eq!(config.min_bytes, 0);
        });
    }
}
