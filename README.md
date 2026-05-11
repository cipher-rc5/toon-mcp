# toon-mcp

Model Context Protocol (MCP) server that detects, classifies, and compresses structured data (JSON, JSONL, CSV, TSV) into the TOON format to reduce token consumption in LLM context windows.

---

## What It Does

LLM tool calls often return large structured payloads — JSON API responses, CSV exports, JSONL logs — that consume significant context tokens without adding proportional information density. toon-mcp sits between your MCP client and those data sources, transparently compressing eligible payloads before they reach the model.

The server exposes three MCP tools:

| Tool | Purpose |
|---|---|
| `compress_content` | Compress a structured string into TOON format, or pass it through unchanged if compression would not help |
| `compression_stats` | Estimate compression savings without encoding — returns statistics only |
| `detect_format` | Identify whether a string is JSON, JSONL, CSV, TSV, or unknown |

---

## Workspace Layout

```
toon-mcp/
├── crates/
│   ├── toon-mcp-core/       # Pure detection, parsing, classification, compression logic
│   ├── toon-mcp-logging/    # Async LogSink trait and implementations (JSONL, memory, noop)
│   ├── toon-mcp-server/     # MCP server binary: tool handlers, config, main entrypoint
│   └── toon-mcp-bench/      # Criterion benchmarks (depends on toon-mcp-core only)
├── data/
│   └── logs/                # Runtime JSONL log partitions (hive layout: day=YYYY-MM-DD/)
├── docs/                    # Extended documentation
└── Cargo.toml               # Workspace manifest with shared dependency versions
```

---

## Installation

### Prerequisites

- Rust toolchain pinned to **1.93.0** (managed by `rust-toolchain.toml` — `rustup` picks this up automatically)
- No Docker, no external services required

### Build

```bash
# debug
cargo build

# release (recommended for MCP clients)
cargo build --release
```

The server binary is written to `./target/release/toon-mcp-server`.

### Environment Variables

Copy the example env file and adjust as needed:

```bash
cp .env.example .env
```

All variables are optional. The server runs with sensible defaults without a `.env` file.

| Variable | Default | Description |
|---|---|---|
| `TOON_COMPRESSION_THRESHOLD` | `0.85` | Maximum output-to-input byte ratio accepted as compressed (0.0–1.0). `0.85` means output must be ≤ 85% of input size (at least 15% savings). |
| `TOON_MIN_BYTES` | `256` | Inputs smaller than this are passed through without attempting compression. |
| `TOON_MAX_INPUT_BYTES` | `10485760` | Inputs larger than this (default 10 MiB) are rejected immediately without parsing, preventing unbounded memory use. |
| `TOON_PIPELINE_TIMEOUT_MS` | `30000` | Per-call compression timeout in milliseconds. Calls exceeding this return an error. |
| `TOON_MAX_CONCURRENT_CALLS` | `8` | Maximum number of concurrent blocking pipeline calls. New calls are rejected with a "server busy" error when the limit is reached. |
| `TOON_KEY_FOLDING` | `true` | Enable TOON key-folding mode for deeply nested objects. |
| `TOON_DELIMITER` | `comma` | TOON output delimiter: `comma`, `tab`, or `pipe`. |
| `TOON_TABULAR_MIN_ROWS` | `3` | Minimum row count for an array to be classified as tabular. |
| `TOON_FOLD_MIN_DEPTH` | `3` | Minimum single-key chain depth to trigger fold-chain classification. |
| `TOON_PRIMITIVE_ARRAY_MIN` | `5` | Minimum element count for a primitive array classification. |
| `TOON_CSV_NUMERIC_COERCION` | `true` | When `false`, CSV/TSV cells that parse as numbers stay as strings. Set to `false` for inputs containing identifiers, postal codes, or leading-zero values that should not be coerced. |
| `TOON_LOG_ENABLED` | `true` | Enable structured event logging. |
| `TOON_LOG_DIR` | `data/logs` | Directory for JSONL log partitions. Must be an absolute path when used from Claude Desktop. When used from opencode (project-relative), `data/logs` resolves correctly from the repository root. |
| `TOON_LOG_BUFFER_SIZE` | `1000` | Number of events buffered before a forced flush. |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | `300` | Periodic flush interval in seconds. |
| `TOON_LOG_LEVEL` | `info` | Tracing log level (`trace`, `debug`, `info`, `warn`, `error`). |
| `TOON_CLIENT_HINT` | _(none)_ | Arbitrary label attached to every log event — useful for identifying which client is calling the server. |

---

## MCP Client Integration

### opencode

The repository ships a ready-to-use `opencode.json`. Build the release binary and the MCP server registers automatically:

```bash
cargo build --release
opencode  # picks up opencode.json from the current directory
```

The `opencode.json` schema is:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "instructions": ["AGENTS.md"],
  "mcp": {
    "toon": {
      "type": "local",
      "command": ["./target/release/toon-mcp-server"],
      "enabled": true,
      "environment": {
        "TOON_CLIENT_HINT": "opencode"
      }
    }
  }
}
```

- `type: "local"` — the binary runs as a subprocess in the project directory,
  so `TOON_LOG_DIR` can be a relative path (e.g. `data/logs`).
- `command` — path to the release binary, relative to the project root.
- `environment` — any `TOON_*` variable can be overridden here. Only variables
  that differ from their defaults need to be listed.
- `enabled` — set to `false` to temporarily disable the server without removing
  the configuration.

### Claude Desktop

Add an entry to your Claude Desktop MCP config (typically `~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "toon": {
      "command": "/absolute/path/to/toon-mcp-server",
      "env": {
        "TOON_COMPRESSION_THRESHOLD": "0.85",
        "TOON_MIN_BYTES": "256",
        "TOON_KEY_FOLDING": "true",
        "TOON_DELIMITER": "comma",
        "TOON_LOG_ENABLED": "true",
        "TOON_LOG_DIR": "/absolute/path/to/toon-mcp/data/logs",
        "TOON_CLIENT_HINT": "claude-desktop"
      }
    }
  }
}
```

**Important:** Claude Desktop does not inherit a shell environment. All paths in `env` values must be absolute.

---

## Tool Reference

### `compress_content`

Compress a structured payload into TOON format. If the input does not meet the compression criteria (wrong format, too small, insufficient savings, unfavorable shape), the original input is returned unchanged.

**Input**
```json
{ "input": "<raw string>" }
```

**Output**
```json
{
  "output": "<TOON-encoded string or original input>",
  "compressed": true,
  "format": "json",
  "shape_class": "tabular",
  "input_bytes": 4096,
  "output_bytes": 891,
  "savings_pct": 0.782,
  "duration_us": 1240,
  "pass_reason": null
}
```

`pass_reason` is non-null when `compressed` is `false`, explaining why the input was not compressed:

| `pass_reason` | Meaning |
|---|---|
| `unknown_format` | Input is not valid JSON, JSONL, CSV, or TSV |
| `below_min_bytes` | Input is smaller than `TOON_MIN_BYTES` |
| `insufficient_savings` | Encoding succeeded but savings were below threshold |
| `shape_not_beneficial` | Classifier determined the shape would not compress well |
| `parse_failed` | Parsing failed after format was detected |

### `compression_stats`

Preview compression statistics without producing encoded output.

**Input**
```json
{ "input": "<raw string>" }
```

**Output**
```json
{
  "would_compress": true,
  "format": "jsonl",
  "shape_class": "tabular",
  "input_bytes": 8192,
  "estimated_output_bytes": 1740,
  "estimated_savings_pct": 0.787,
  "threshold": 0.85,
  "pass_reason": null
}
```

### `detect_format`

Identify the format of an input string.

**Input**
```json
{ "input": "<raw string>" }
```

**Output**
```json
{
  "format": "csv",
  "input_bytes": 512,
  "line_count": 25,
  "column_count": 7
}
```

`line_count` is populated for JSONL inputs. `column_count` is populated for CSV and TSV inputs. Both are null for JSON and unknown formats.

---

## Querying Logs

When `TOON_LOG_ENABLED=true`, every tool invocation writes a structured event to a hive-partitioned JSONL log directory:

```
data/logs/
  day=2026-04-06/
    events.jsonl
```

These files are queryable directly with DuckDB:

```sql
-- install DuckDB: https://duckdb.org/docs/installation
duckdb

SELECT
    tool_name,
    format,
    shape_class,
    input_bytes,
    output_bytes,
    round(savings_pct * 100, 1) AS savings_pct,
    compressed,
    pass_reason
FROM read_json('data/logs/**/*.jsonl')
ORDER BY ts_us DESC
LIMIT 20;
```

Example aggregate queries:

```sql
-- compression rate by format
SELECT
    input_format,
    count(*) AS calls,
    avg(savings_pct) AS avg_savings,
    sum(input_bytes - output_bytes) AS bytes_saved_total
FROM read_json('data/logs/**/*.jsonl')
WHERE compressed = true
GROUP BY input_format;

-- pass-through breakdown
SELECT pass_reason, count(*) AS n
FROM read_json('data/logs/**/*.jsonl')
WHERE compressed = false
GROUP BY pass_reason
ORDER BY n DESC;
```

---

## Running Tests

```bash
# all workspace tests
cargo test --workspace

# single crate
cargo test -p toon-mcp-core

# benchmarks (does not run tests)
cargo bench --package toon-mcp-bench
```

---

## Development

```bash
# format
cargo fmt

# lint (all warnings are errors)
cargo clippy -- -D warnings

# docs
cargo doc --no-deps --open
```

Pre-commit gate (must pass before every commit):

```bash
cargo fmt && cargo clippy -- -D warnings && cargo test --workspace
```

---

## Extended Documentation

| Document | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Crate dependency graph, data flow, layer rules, design rationale |
| [docs/algorithms.md](docs/algorithms.md) | Format detection, shape classification, and compression pipeline algorithms |
| [docs/configuration.md](docs/configuration.md) | Full configuration reference with tuning guidance |
| [docs/logging.md](docs/logging.md) | LogSink trait, event schema, log storage layout, and query examples |

---

## License

[MIT](LICENSE)
