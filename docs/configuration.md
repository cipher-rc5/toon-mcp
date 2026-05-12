# Configuration

toon-mcp is configured exclusively through environment variables. There are no config files, command-line flags, or runtime mutation. All values are read once at startup in `Config::load()` and distributed as an immutable `Arc<Config>`.

---

## Loading Precedence

```mermaid
flowchart LR
    EnvFile[".env file\n(dotenvy)"] --> Merge["merge into process env"]
    ShellEnv["Shell environment\n(existing vars)"] --> Merge
    Merge --> Load["Config::load()\nreads TOON_* vars"]
    Load --> Arc["Arc&lt;Config&gt;\n(immutable for process lifetime)"]
```

Shell environment variables take precedence over `.env` values — `dotenvy` only sets a variable if it is not already present in the environment.

---

## Full Variable Reference

### Compression Behavior

| Variable | Type | Default | Description |
|---|---|---|---|
| `TOON_COMPRESSION_THRESHOLD` | `f64` (0.0–1.0) | `0.85` | Maximum fraction of the original byte count that the TOON output may occupy. An input of 10,000 bytes with `threshold=0.85` must produce output ≤ 8,500 bytes to be considered compressed. |
| `TOON_MIN_BYTES` | `usize` | `256` | Inputs shorter than this byte count are passed through without any processing. Avoids overhead on small strings that will never compress meaningfully. |
| `TOON_MAX_INPUT_BYTES` | `usize` | `10485760` | Hard upper bound on input size in bytes (default 10 MiB). Inputs larger than this are rejected immediately without parsing, preventing unbounded memory use. |
| `TOON_KEY_FOLDING` | `bool` | `true` | Enable TOON key-folding mode. When enabled, deeply nested single-key objects are collapsed to dot-notation paths. Disable if your consumer of TOON output does not support key folding. |
| `TOON_DELIMITER` | `string` | `comma` | The delimiter character used between values in TOON tabular output. Accepted values: `comma`, `tab`, `pipe`. |
| `TOON_CSV_NUMERIC_COERCION` | `bool` | `true` | When enabled, CSV/TSV cells that parse as numbers become JSON numbers in the normalised intermediate. Set to `false` for inputs containing identifiers, postal codes, or leading-zero values that must round-trip as strings. |

**`TOON_MAX_INPUT_BYTES`** caps the largest payload the server will accept. Tune it down when the host process is memory-constrained or the upstream client is known to never send payloads above a smaller bound — the rejection happens before any allocation, so a tighter cap is the cheapest form of back-pressure. Tune it up only when you have observed legitimate inputs being rejected with `input_too_large`. Watch the runbook (`docs/runbook.md`) for the matching error code and triage steps.

**`TOON_CSV_NUMERIC_COERCION`** controls a behavioural tradeoff, not a performance one. Leave at `true` to maximise compression density for genuinely numeric CSV columns. Flip to `false` when you see downstream consumers complaining about lost leading zeros, mis-typed phone numbers, or identifiers being silently rewritten as integers — those are the diagnostic symptoms. See `docs/runbook.md` for matching incident patterns.

### Concurrency and Timeouts

| Variable | Type | Default | Description |
|---|---|---|---|
| `TOON_PIPELINE_TIMEOUT_MS` | `u64` (milliseconds) | `30000` | Per-call pipeline timeout in milliseconds. A call exceeding this duration returns a typed timeout error rather than blocking indefinitely. Also bounds how long a call will wait for a concurrency permit before giving up. |
| `TOON_MAX_CONCURRENT_CALLS` | `usize` | `8` | Maximum number of concurrent blocking pipeline calls. Controls how many `spawn_blocking` dispatches can be in-flight at once. When the limit is reached, new calls wait up to `TOON_PIPELINE_TIMEOUT_MS` for a permit before returning a busy error. |

**`TOON_PIPELINE_TIMEOUT_MS`** is the per-call upper bound on blocking work plus permit-wait. Tune it down if you would rather surface a fast `timeout` error to the client than have the agent stall while a pathological input is processed — small payloads should always finish in well under a second. Tune it up only when very large inputs (near `TOON_MAX_INPUT_BYTES`) are legitimately producing timeout errors on a slow machine. Symptom to watch for: a sudden spike in `timeout` errors in the JSONL log under load — that is usually permit-wait, not parse time, so consider raising `TOON_MAX_CONCURRENT_CALLS` first. See `docs/runbook.md` for the triage flow.

**`TOON_MAX_CONCURRENT_CALLS`** caps how many CPU-bound pipeline tasks can run simultaneously on the blocking pool. Tune it down on shared/low-core hosts where unbounded concurrency would starve other workloads. Tune it up when the JSONL log shows a sustained pattern of `busy` rejections while host CPU still has headroom — the default `8` is conservative for modern multi-core machines. See `docs/runbook.md` for the relationship between busy errors, timeouts, and CPU saturation.

### Classification Thresholds

These control the minimum structural requirements for each shape class. Inputs that fall below the threshold are classified as `PassThrough` and not compressed.

| Variable | Type | Default | Corresponding constant |
|---|---|---|---|
| `TOON_TABULAR_MIN_ROWS` | `usize` | `3` | `TABULAR_MIN_ROWS` |
| `TOON_FOLD_MIN_DEPTH` | `usize` | `3` | `FOLD_MIN_DEPTH` |
| `TOON_PRIMITIVE_ARRAY_MIN` | `usize` | `5` | `PRIMITIVE_ARRAY_MIN` |

### Logging

| Variable | Type | Default | Description |
|---|---|---|---|
| `TOON_LOG_ENABLED` | `bool` | `true` | Enable structured event logging to JSONL files. When `false`, a `NoopSink` is used and no events are written. |
| `TOON_LOG_DIR` | `string` | `data/logs` | Directory where hive-partitioned JSONL log files are written. **Must be an absolute path** when the server is launched by Claude Desktop. |
| `TOON_LOG_BUFFER_SIZE` | `usize` | `1000` | Number of events held in memory before the background writer task forces a flush to disk. |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | `u64` | `300` | Periodic flush interval in seconds. Events older than this will be flushed even if the buffer is not full. |

### Observability

| Variable | Type | Default | Description |
|---|---|---|---|
| `TOON_LOG_LEVEL` | `string` | `info` | Tracing log level for stderr output. Accepted values: `trace`, `debug`, `info`, `warn`, `error`. |
| `TOON_CLIENT_HINT` | `string` | _(none)_ | Arbitrary label attached to every `LogEvent`. Use this to distinguish traffic from different MCP clients when multiple clients share the same server (e.g., `"opencode"`, `"claude-desktop"`, `"ci"`). |

---

## Boolean Parsing

`env_bool` accepts the following values case-insensitively:

| Truthy | Falsy |
|---|---|
| `true`, `1`, `yes` | `false`, `0`, `no` |

Any other value causes `Config::load()` to log a warning and use the default.

---

## Delimiter Parsing

`env_delimiter` accepts (case-insensitive):

| Value | Delimiter character |
|---|---|
| `comma` | `,` |
| `tab` | `\t` |
| `pipe` | `\|` |

---

## Threshold Semantics in Depth

The `TOON_COMPRESSION_THRESHOLD` value is inverted relative to what you might expect from a "savings percentage":

```
threshold = 0.85
→ output must be ≤ 85% of input bytes
→ minimum savings = 15%
```

```mermaid
xychart-beta
    title "Threshold vs. required savings"
    x-axis ["0.50", "0.60", "0.70", "0.80", "0.85", "0.90", "0.95", "1.00"]
    y-axis "Required savings %" 0 --> 55
    bar [50, 40, 30, 20, 15, 10, 5, 0]
```

**Practical guidance:**

- `0.85` (default) — accepts inputs that compress by at least 15%. Suitable for typical JSON API responses with repeated keys.
- `0.70` — requires 30% savings; filters out inputs with low redundancy.
- `0.95` — accepts almost any positive compression; maximizes compressed output volume.
- `1.00` — pass through unless TOON output is strictly smaller than input (useful for testing the encoding path).

---

## Classification Threshold Guidance

### `TOON_TABULAR_MIN_ROWS` (default: 3)

Setting this lower (e.g., `2`) compresses two-row arrays but provides minimal savings — the header overhead in TOON tabular format is only worthwhile with several rows.

Setting this higher (e.g., `10`) reserves tabular encoding for larger collections. For APIs that typically return small result sets, increasing this avoids compressing inputs that would not compress well.

### `TOON_FOLD_MIN_DEPTH` (default: 3)

A depth of `3` means a chain must be at least `a → b → c → leaf` to qualify. Shallower chains (`a → b → leaf`, depth 2) would be classified as `PassThrough` or `Mixed`.

Lower values compress shallower nesting but may produce false positives on normal nested objects. The default of `3` matches common API response patterns like `response.data.items`.

### `TOON_PRIMITIVE_ARRAY_MIN` (default: 5)

Arrays of numbers or strings below this size produce negligible savings. The default of `5` ensures the TOON overhead of encoding mode markers is amortized across enough elements.

---

## Example Configurations

### Aggressive compression (maximum token savings)

```bash
TOON_COMPRESSION_THRESHOLD=0.95
TOON_MIN_BYTES=128
TOON_TABULAR_MIN_ROWS=2
TOON_FOLD_MIN_DEPTH=2
TOON_PRIMITIVE_ARRAY_MIN=3
```

### Conservative compression (only high-confidence wins)

```bash
TOON_COMPRESSION_THRESHOLD=0.70
TOON_MIN_BYTES=1024
TOON_TABULAR_MIN_ROWS=10
TOON_FOLD_MIN_DEPTH=4
TOON_PRIMITIVE_ARRAY_MIN=10
```

### Logging disabled (zero I/O overhead)

```bash
TOON_LOG_ENABLED=false
```

### High-throughput logging

```bash
TOON_LOG_BUFFER_SIZE=5000
TOON_LOG_FLUSH_INTERVAL_SECS=60
```

### Claude Desktop absolute paths

```json
{
  "mcpServers": {
    "toon": {
      "command": "/Users/you/projects/toon-mcp/target/release/toon-mcp-server",
      "env": {
        "TOON_LOG_DIR": "/Users/you/projects/toon-mcp/data/logs",
        "TOON_CLIENT_HINT": "claude-desktop"
      }
    }
  }
}
```

**Claude Desktop does not inherit your shell environment.** All paths must be absolute. Relative paths like `data/logs` will resolve relative to Claude Desktop's working directory, which is typically your home directory — not the project directory.

---

## Config Struct

**Source:** `crates/toon-mcp-server/src/config.rs`

```rust
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum output-to-input byte ratio accepted as "compressed".
    pub max_output_ratio: f64,
    /// Minimum input byte count for classification to run.
    pub min_bytes: usize,
    /// Maximum input byte count. Larger inputs are rejected without parsing.
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
    pub csv_numeric_coercion: bool,
    /// Whether structured logging is enabled.
    pub logging_enabled: bool,
    /// JSONL sink configuration (only meaningful when `logging_enabled`).
    pub logging: JsonlSinkConfig,
    /// tracing filter string (e.g. `"info"`, `"debug"`).
    pub log_level: String,
    /// Optional client identifier tag written to every log row.
    pub client_hint: Option<String>,
    /// Per-call pipeline timeout in milliseconds.
    pub pipeline_timeout_ms: u64,
    /// Maximum number of concurrent blocking pipeline calls.
    pub max_concurrent_calls: usize,
}
```

`CompressConfig` is derived from `Config` fields in each handler call:

```rust
let compress_config = CompressConfig {
    threshold: config.max_output_ratio,
    min_bytes: config.min_bytes,
    key_folding: config.key_folding,
    delimiter: config.delimiter,
    tabular_min_rows: config.tabular_min_rows,
    fold_min_depth: config.fold_min_depth,
    primitive_array_min: config.primitive_array_min,
};
```

---

## Startup Validation

The accepted range or value-set for each variable is the **contract**:

| Variable | Accepted values |
|---|---|
| `TOON_COMPRESSION_THRESHOLD` | finite `f64` in `[0.0, 1.0]` |
| `TOON_MIN_BYTES` | non-negative `usize` |
| `TOON_MAX_INPUT_BYTES` | positive `usize` |
| `TOON_KEY_FOLDING` | one of `true`, `1`, `yes`, `false`, `0`, `no` (case-insensitive) |
| `TOON_DELIMITER` | one of `comma`, `tab`, `pipe` |
| `TOON_TABULAR_MIN_ROWS` | non-negative `usize` |
| `TOON_FOLD_MIN_DEPTH` | non-negative `usize` |
| `TOON_PRIMITIVE_ARRAY_MIN` | non-negative `usize` |
| `TOON_CSV_NUMERIC_COERCION` | one of `true`, `1`, `yes`, `false`, `0`, `no` |
| `TOON_PIPELINE_TIMEOUT_MS` | positive `u64` |
| `TOON_MAX_CONCURRENT_CALLS` | positive `usize` |
| `TOON_LOG_ENABLED` | one of `true`, `1`, `yes`, `false`, `0`, `no` |
| `TOON_LOG_DIR` | string (absolute path required for Claude Desktop) |
| `TOON_LOG_BUFFER_SIZE` | positive `usize` |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | positive `u64` |
| `TOON_LOG_LEVEL` | one of `trace`, `debug`, `info`, `warn`, `error` |
| `TOON_CLIENT_HINT` | non-empty string, or unset |

Values outside the documented range are rejected at startup with a typed
error. Watch stderr during startup to verify all values loaded correctly:

```
[INFO] toon_mcp_server::config: loaded config max_output_ratio=0.85 min_bytes=256 ...
```

---

## See Also

- `docs/runbook.md` — incident triage for `input_too_large`, `timeout`, and `busy` errors, plus the JSONL log fields used to diagnose them.
- `README.md` — quick-start environment variable reference.
