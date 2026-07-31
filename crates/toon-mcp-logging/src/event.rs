// file: crates/toon-mcp-logging/src/event.rs
// description: LogEvent struct representing one tool invocation record

use serde::{Deserialize, Serialize};

/// A structured record of a single MCP tool invocation.
///
/// Events are serialised as JSONL and written to hive-partitioned files
/// under the configured log directory. DuckDB can query them directly:
///
/// ```sql
/// SELECT * FROM read_json('data/logs/**/*.jsonl');
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    /// 16-character lowercase hexadecimal identifier (xxh3-64 of an
    /// atomic counter mixed with a nanosecond timestamp). Not a UUID;
    /// collisions become non-negligible around 2^32 generated IDs within
    /// a single process run.
    pub event_id: String,

    /// Unix timestamp in microseconds when the tool call was received.
    pub ts_us: i64,

    /// Name of the MCP tool that was invoked.
    /// One of: `"compress_content"`, `"compression_stats"`, `"detect_format"`.
    pub tool_name: String,

    /// Detected input format (`"json"`, `"jsonl"`, `"csv"`, `"tsv"`, `"unknown"`).
    pub input_format: String,

    /// Classifier shape class for the input value tree.
    pub shape_class: String,

    /// Byte length of the input string.
    pub input_bytes: u64,

    /// Byte length of the output string (equals `input_bytes` when not compressed).
    pub output_bytes: u64,

    /// Whether TOON encoding was applied and its output returned.
    pub compressed: bool,

    /// Fraction of bytes saved (0.0 when `compressed` is false).
    pub savings_pct: f64,

    /// The compression threshold that was active during this invocation.
    pub threshold_used: f64,

    /// Wall-clock duration in microseconds covering detect + classify + encode.
    pub duration_us: u64,

    /// How the tool invocation ended: `"ok"` for a successful response,
    /// `"rejected"` for an input rejected before processing (e.g. over the
    /// size limit), `"timeout"` for a pipeline timeout, `"busy"` when no
    /// concurrency permit was available, or `"failed"` for an internal
    /// failure such as a crashed blocking task.
    ///
    /// Deserialisation defaults to `"ok"` so rows written before this field
    /// existed remain readable.
    #[serde(default = "default_outcome")]
    pub outcome: String,

    /// Human-readable pass-through reason when `compressed` is false.
    pub pass_reason: Option<String>,

    /// Client identifier set via `TOON_CLIENT_HINT` env var.
    /// Allows log queries to split metrics by host.
    pub client_hint: Option<String>,
}

/// Default for [`LogEvent::outcome`] when deserialising pre-outcome rows.
fn default_outcome() -> String {
    "ok".into()
}
