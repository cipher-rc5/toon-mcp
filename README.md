# toon-mcp

[![CI](https://github.com/cipher-rc5/toon-mcp/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/cipher-rc5/toon-mcp/actions/workflows/ci.yml)
[![Release](https://github.com/cipher-rc5/toon-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/cipher-rc5/toon-mcp/actions/workflows/release.yml)
[![Rust](https://img.shields.io/badge/rust-1.95.0-orange?logo=rust)](rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-BUSL--1.1-blue)](LICENSE)

> A [Model Context Protocol (MCP)](https://spec.modelcontextprotocol.io/) server that detects, classifies, and compresses structured payloads (JSON, JSONL, CSV, TSV) into the [TOON](https://github.com/toon-format/toon) format to reduce LLM context-window token consumption.

LLM tool calls often return large structured payloads — JSON API responses, CSV exports, JSONL logs — that consume context tokens without proportional informational value. `toon-mcp` sits between an MCP client and those data sources, transparently re-encoding eligible payloads before they reach the model.

---

## Table of Contents

- [Highlights](#highlights)
- [Quick start](#quick-start)
- [Installation](#installation)
- [MCP client integration](#mcp-client-integration)
- [MCP tools](#mcp-tools)
- [Configuration](#configuration)
- [Observability](#observability)
- [Compatibility & support](#compatibility--support)
- [Development](#development)
- [Releases & verification](#releases--verification)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

---

## Highlights

- **Four MCP tools** over stdio: `detect_format`, `compression_stats`, `compress_content`, `toon_diagnostics`.
- **Lossy-free by construction.** When TOON encoding does not produce meaningful savings the original input is returned verbatim along with a structured `pass_reason`.
- **Bounded by design.** Hard per-input size cap, per-call timeout, and a concurrency permit gate make resource use predictable. Logging is fire-and-forget and never fails a tool response.
- **Production-grade observability.** Every call writes a JSONL row to a hive-partitioned directory that is queryable in place with DuckDB; runtime counters are exposed via `toon_diagnostics`.
- **Reproducible, signed releases.** Linux (glibc 2.17 baseline) and macOS binaries for `x86_64` and `aarch64`, with SHA256 checksums, a CycloneDX SBOM, and Sigstore keyless signatures.
- **Strong test surface.** ~180 unit/integration tests, doctests, property tests, boundary stress tests, and an in-tree `cargo-fuzz` harness for every untrusted parser entry point.

---

## Quick start

```bash
# 1. build the release binary
cargo build --release

# 2. try it locally with opencode (config shipped in opencode.json)
opencode

# 3. or register with Claude Code (CLI)
claude mcp add toon -s user \
  -e TOON_CLIENT_HINT=claude-code \
  -e TOON_LOG_DIR=/absolute/path/to/toon-mcp/data/logs \
  -- /absolute/path/to/toon-mcp/target/release/toon-mcp-server
```

After registering, the four tools (`compress_content`, `compression_stats`, `detect_format`, `toon_diagnostics`) appear in the client's tool list and can be invoked over stdio JSON-RPC.

---

## Installation

### Prerequisites

- Rust toolchain `1.95.0` (managed by [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` picks it up automatically).
- No Docker, no external services.

### Build from source

```bash
cargo build              # debug build
cargo build --release    # recommended for MCP clients
```

The server binary is written to `./target/release/toon-mcp-server`.

### Pre-built release binaries

Download a binary for your platform from the [GitHub Releases page](https://github.com/cipher-rc5/toon-mcp/releases). Every release ships:

| Asset                                                            | Notes                                                                 |
| ---------------------------------------------------------------- | --------------------------------------------------------------------- |
| `toon-mcp-server-x86_64-unknown-linux-gnu`                       | Linux x86_64 (glibc 2.17 baseline)                                    |
| `toon-mcp-server-aarch64-unknown-linux-gnu`                      | Linux aarch64 (glibc 2.17 baseline)                                   |
| `toon-mcp-server-x86_64-apple-darwin`                            | macOS Intel                                                           |
| `toon-mcp-server-aarch64-apple-darwin`                           | macOS Apple Silicon                                                   |
| `toon-mcp-server-sbom.cdx.json`                                  | CycloneDX 1.4 software bill of materials                              |
| `checksums.sha256`                                               | SHA256 over every asset                                               |
| `*.sigstore.json`                                                | One Sigstore keyless signature bundle per asset                       |

See [Releases & verification](#releases--verification) below for the recommended `cosign` flow.

### Environment variables

Copy and edit the example env file:

```bash
cp .env.example .env
```

All variables are optional; the server runs with sensible defaults without a `.env` file. See [Configuration](#configuration) for the full reference.

---

## MCP client integration

`toon-mcp-server` speaks MCP over **stdio only**. Any client that can launch a local subprocess and connect to its stdin/stdout is supported.

### opencode

The repository ships a ready-to-use [`opencode.json`](opencode.json). Build the release binary and the MCP server registers automatically:

```bash
cargo build --release
opencode  # picks up opencode.json from the current directory
```

Schema reference:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "toon": {
      "type": "local",
      "command": ["./target/release/toon-mcp-server"],
      "enabled": true,
      "environment": { "TOON_CLIENT_HINT": "opencode" }
    }
  }
}
```

- `type: "local"` runs the binary as a subprocess in the project directory, so `TOON_LOG_DIR` may be a relative path (e.g. `data/logs`).
- Any `TOON_*` variable can be overridden under `environment`. Variables that match defaults can be omitted.
- Set `enabled: false` to temporarily disable the server without removing the configuration.

### Claude Code (CLI)

Register with the Claude Code CLI. Build the release binary first, then:

```bash
cargo build --release

# user scope — available across all your projects
claude mcp add toon \
  -s user \
  -e TOON_CLIENT_HINT=claude-code \
  -e TOON_LOG_DIR=/absolute/path/to/toon-mcp/data/logs \
  -- /absolute/path/to/toon-mcp/target/release/toon-mcp-server

# verify
claude mcp list
claude mcp get toon
```

Scope options:

| Scope         | Stored in                  | Sharing                                            |
| ------------- | -------------------------- | -------------------------------------------------- |
| `-s user`     | `~/.claude.json`           | Available in every project for the OS user        |
| `-s project`  | `.mcp.json` at the repo root | Can be committed and shared with teammates       |
| `-s local`    | Local config (default)      | Only the current project + user                   |

Notes:

- Always use absolute paths. Claude Code does not resolve `~`, `./`, or `$VARS` inside the `command` field.
- Add further overrides with repeated `-e KEY=VALUE` flags. Unspecified variables fall back to defaults.
- After registering, run `/mcp` inside a session to confirm `toon` is connected and the four tools are listed.

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

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

> **Claude Desktop does not inherit a shell environment.** Every path in `env` must be absolute.

After editing, fully quit and relaunch Claude Desktop. The toon tools appear in the tool picker (🔌) once the server logs `toon-mcp-server ready` on stderr.

### Claude (web)

The web client at <https://claude.ai> connects to MCP servers via **Custom Connectors**, which require an HTTP-based transport. `toon-mcp-server` currently exposes only the rmcp `transport-io` feature (stdio), so claude.ai cannot launch it directly. Options:

1. **Use Claude Desktop or Claude Code instead** — both speak stdio natively.
2. **Bridge stdio → HTTP** using a third-party adapter (`mcp-remote`, `supergateway`) behind your own TLS termination and authentication. This is unsupported by this project; you accept responsibility for the bridge.

A first-party HTTP transport is not currently on the roadmap. Open an issue if you need one.

---

## MCP tools

All four tools are stateless from the client's perspective: each call carries its own `input` and is logged independently.

### `compress_content`

Compress a structured payload into TOON format. If the input does not meet the compression criteria (unknown format, too small, parse failure, unfavourable shape, or insufficient savings), the original input is returned unchanged.

**Input**

```json
{ "input": "<raw string>" }
```

**Output (all fields)**

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
  "pass_reason": null,
  "detection_confidence": "certain",
  "detection_ambiguous": false,
  "detection_candidates": ["json"],
  "numeric_coercion_used": null,
  "lossy_coercion_possible": null
}
```

`pass_reason` is non-null when `compressed` is `false`:

| `pass_reason`           | Meaning                                                                                                                                                |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `unknown_format`        | Input is not valid JSON, JSONL, CSV, or TSV.                                                                                                           |
| `below_min_bytes`       | Input is smaller than `TOON_MIN_BYTES`.                                                                                                                |
| `insufficient_savings`  | Encoding succeeded but savings were below `TOON_COMPRESSION_THRESHOLD`.                                                                                |
| `shape_not_beneficial`  | Classifier judged the shape unlikely to compress well.                                                                                                 |
| `parse_failed`          | The format was detected but parsing failed (truncation, invalid escape, etc.).                                                                         |
| `encode_failed`         | The input parsed successfully but TOON encoding failed; the original input is returned unchanged.                                                      |
| `input_exceeds_limit`   | Library-only outcome reachable through `Compressor::decide`. The MCP server intercepts oversized inputs earlier and returns an `invalid_params` error. |

`detection_confidence` is `"certain"` for inputs validated by a full `serde_json` parse or for `unknown`, and `"heuristic"` for JSONL / CSV / TSV (which are probed without parsing the entire document). `detection_candidates` lists every format that matched, in detection-precedence order. `numeric_coercion_used` and `lossy_coercion_possible` are populated only for CSV / TSV inputs; see [Numeric coercion](#numeric-coercion-csv--tsv).

### `compression_stats`

Run the full pipeline (detect → parse → classify → encode) but return statistics only. The encoded string is discarded.

**Input**

```json
{ "input": "<raw string>" }
```

**Output (all fields)**

```json
{
  "would_compress": true,
  "format": "jsonl",
  "shape_class": "tabular",
  "input_bytes": 8192,
  "estimated_output_bytes": 1740,
  "estimated_savings_pct": 0.787,
  "threshold": 0.85,
  "pass_reason": null,
  "detection_confidence": "heuristic",
  "detection_ambiguous": false,
  "detection_candidates": ["jsonl"],
  "numeric_coercion_used": null,
  "lossy_coercion_possible": null
}
```

### `detect_format`

Identify the format of an input string without invoking the compression pipeline.

**Input**

```json
{ "input": "<raw string>" }
```

**Output (all fields)**

```json
{
  "format": "csv",
  "input_bytes": 512,
  "line_count": null,
  "column_count": 7,
  "detection_confidence": "heuristic",
  "detection_ambiguous": false,
  "detection_candidates": ["csv"],
  "numeric_coercion_used": true,
  "lossy_coercion_possible": false
}
```

- `line_count` is populated only for JSONL inputs.
- `column_count` is populated only for CSV and TSV inputs.
- Both are `null` for JSON and `unknown`.

### `toon_diagnostics`

Return runtime health counters for the running server. Useful for live troubleshooting and as the response payload for ad-hoc health probes from a client.

**Input**

```json
{}
```

**Output (abbreviated)**

```json
{
  "logging_enabled": true,
  "logging": {
    "record_dropped_count": 0,
    "record_failed_count": 0,
    "serialization_failed_count": 0,
    "writer_failed_count": 0,
    "last_error": null,
    "queue_capacity": 1000,
    "queue_queued": 0,
    "queue_available": 1000
  },
  "handler": {
    "log_record_failed_count": 0,
    "log_record_dropped_count": 0,
    "pipeline_timeout_count": 0,
    "request_succeeded_count": 42,
    "request_duration_us_total": 51230,
    "request_duration_us_max": 8120,
    "request_duration_us_avg": 1219.76
  },
  "semaphore_available_permits": 8,
  "max_concurrent_calls": 8
}
```

Use the `handler.*` fields to spot tail latency and pipeline timeouts; use the `logging.*` fields to spot writer-task backpressure or failures.

### Numeric coercion (CSV / TSV)

When `TOON_CSV_NUMERIC_COERCION=true` (the default), numeric-looking CSV/TSV cells are parsed as JSON numbers in the intermediate value tree. This improves compression but can lose information for identifiers, postal codes, and leading-zero values.

Each tool result includes:

- `numeric_coercion_used` — `true` when at least one cell was coerced.
- `lossy_coercion_possible` — `true` when at least one coerced cell has syntax (leading zeros, explicit `+` sign, exponent, decimal spelling of a whole number) that JSON numbers cannot round-trip exactly.

Set `TOON_CSV_NUMERIC_COERCION=false` per-call (via env override) or globally if your data contains identifiers that must remain strings.

---

## Configuration

All configuration is via `TOON_*` environment variables, read once at startup. Full reference: [docs/configuration.md](docs/configuration.md).

### Compression behaviour

| Variable                     | Default     | Purpose                                                                                                                                      |
| ---------------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `TOON_COMPRESSION_THRESHOLD` | `0.85`      | Maximum output-to-input byte ratio accepted as compressed. `0.85` means output must be ≤ 85 % of input (≥ 15 % savings).                     |
| `TOON_MIN_BYTES`             | `256`       | Inputs below this byte count are passed through immediately.                                                                                 |
| `TOON_MAX_INPUT_BYTES`       | `10485760`  | Hard upper bound (10 MiB by default). Larger inputs are rejected before parsing.                                                             |
| `TOON_KEY_FOLDING`           | `true`      | Enable TOON key folding for deeply nested objects.                                                                                           |
| `TOON_DELIMITER`             | `comma`     | TOON array delimiter: `comma`, `tab`, or `pipe`.                                                                                             |
| `TOON_CSV_NUMERIC_COERCION`  | `true`      | Coerce numeric-looking CSV/TSV cells into JSON numbers. Set `false` for identifiers / leading-zero values.                                   |

### Classification thresholds

| Variable                  | Default | Purpose                                                              |
| ------------------------- | ------- | -------------------------------------------------------------------- |
| `TOON_TABULAR_MIN_ROWS`   | `3`     | Minimum row count for an array to be classified as tabular.          |
| `TOON_FOLD_MIN_DEPTH`     | `3`     | Minimum single-key chain depth to trigger fold-chain classification. |
| `TOON_PRIMITIVE_ARRAY_MIN`| `5`     | Minimum element count for a primitive-array classification.          |

### Concurrency, timeouts, and admission control

| Variable                    | Default | Purpose                                                                                                                                                                                   |
| --------------------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TOON_PIPELINE_TIMEOUT_MS`  | `30000` | Per-call pipeline timeout in ms. Bounds permit wait + handler-visible blocking pipeline result. Timed-out blocking work may continue until the worker finishes.                           |
| `TOON_MAX_CONCURRENT_CALLS` | `8`     | Maximum concurrent blocking pipeline calls. Additional calls wait up to `TOON_PIPELINE_TIMEOUT_MS` for a permit, then return `server busy`.                                              |

### Logging

| Variable                       | Default     | Purpose                                                                                                                                                                                              |
| ------------------------------ | ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TOON_LOG_ENABLED`             | `true`      | Enable structured event logging.                                                                                                                                                                     |
| `TOON_LOG_DIR`                 | `data/logs` | Directory for JSONL log partitions. **Use an absolute path** when launched under Claude Desktop or a process supervisor with an unpredictable working directory.                                     |
| `TOON_LOG_BUFFER_SIZE`         | `1000`      | Events buffered before a forced flush. Doubles as the writer-task channel capacity for backpressure accounting.                                                                                       |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | `300`       | Periodic flush interval.                                                                                                                                                                             |
| `TOON_LOG_LEVEL`               | `info`      | Tracing log level (`trace`, `debug`, `info`, `warn`, `error`).                                                                                                                                       |
| `TOON_CLIENT_HINT`             | _(none)_    | Arbitrary label attached to every log event — useful for splitting metrics by calling client.                                                                                                        |
| `TOON_CONFIG_STRICT`           | `false`     | If `true`, unparseable env values cause startup to fail. The default is to log a warning and fall back to the documented default.                                                                    |

### Recommended starting points by host

| Host                                   | Suggested overrides                                                              |
| -------------------------------------- | -------------------------------------------------------------------------------- |
| 1–2 vCPU laptop / VM                   | `TOON_MAX_INPUT_BYTES=1048576`, `TOON_MAX_CONCURRENT_CALLS=2`, `TOON_PIPELINE_TIMEOUT_MS=10000` |
| Normal developer workstation           | All defaults                                                                     |
| Dedicated multi-core host              | Raise `TOON_MAX_CONCURRENT_CALLS` only after observing sustained `server busy` errors with CPU/RSS headroom |

See [docs/production.md](docs/production.md), [docs/deployment.md](docs/deployment.md), and [docs/runbook.md](docs/runbook.md) for deployment, sizing, retention, and troubleshooting guidance.

---

## Observability

### `toon_diagnostics` (live)

Call the `toon_diagnostics` MCP tool from any client. The response captures the same counters the JSONL logs aggregate, in real time.

### JSONL event log (historical)

When `TOON_LOG_ENABLED=true` every tool invocation appends a `LogEvent` row to a hive-partitioned JSONL directory:

```
data/logs/
  day=2026-04-06/
    events.jsonl
```

The on-disk schema is documented in detail in [docs/logging.md](docs/logging.md). Logs are queryable in place with [DuckDB](https://duckdb.org/) (or any tool that reads JSONL):

```sql
-- recent activity
SELECT
    tool_name,
    input_format,
    shape_class,
    input_bytes,
    output_bytes,
    round(savings_pct * 100, 1) AS savings_pct,
    compressed,
    pass_reason
FROM read_json('data/logs/**/*.jsonl')
ORDER BY ts_us DESC
LIMIT 20;

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

### Tracing

Structured tracing events are emitted on stderr. The startup readiness line is a stable anchor for monitoring:

```
INFO toon_mcp_server: toon-mcp-server ready status="ready" component="toon-mcp-server" version="0.1.0"
```

---

## Compatibility & support

- **Rust toolchain**: exactly the version pinned in [`rust-toolchain.toml`](rust-toolchain.toml) (`1.95.0`).
- **Release binaries**: Linux (glibc 2.17 baseline) and macOS on `x86_64` and `aarch64`.
- **MCP transport**: local stdio clients that can launch a binary and pass environment variables.
- **Active branch**: `master`. CI also runs on `main` while both branch names exist.
- **Stability policy**: pre-`1.0.0`. Tool schemas, public Rust APIs, and environment variable semantics may change in minor releases; breaking changes are recorded in [`CHANGELOG.md`](CHANGELOG.md). The semver bump rules are in [`CONTRIBUTING.md`](CONTRIBUTING.md#semver-policy).

---

## Development

### Tests

```bash
# all workspace tests
cargo test --workspace

# single crate
cargo test -p toon-mcp-core
cargo test -p toon-mcp-logging
cargo test -p toon-mcp-server

# benchmarks (does not run tests)
cargo bench --package toon-mcp-bench
```

### Linting and formatting

```bash
cargo fmt                                                      # apply formatting
cargo fmt --check                                              # CI-style check
cargo clippy --workspace --all-targets -- -D warnings          # canonical lint gate
cargo doc --workspace --no-deps --open                         # local docs
```

### Pre-commit gate

The minimum gate that must pass before pushing a commit:

```bash
cargo fmt --check && \
  cargo clippy --workspace --all-targets -- -D warnings && \
  cargo test --workspace
```

CI runs this same triple plus:

- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- `cargo audit --deny warnings`
- `cargo deny check advisories bans licenses sources`
- `cargo-llvm-cov` line coverage gate (currently **75 %**)
- `cargo-semver-checks` against library crates when their sources change
- `cargo-fuzz` smoke (15 s per target) on every PR; 30 s per target on schedule / dispatch

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full contributor workflow, branch model, and semver policy.

---

## Releases & verification

Releases are tagged `vX.Y.Z` and built by [`.github/workflows/release.yml`](.github/workflows/release.yml). Each release includes:

1. Pre-flight gates: `cargo check --locked`, fmt, clippy `-D warnings`, full test suite, `cargo audit`.
2. Cross-compiled binaries for `x86_64`/`aarch64` × Linux/macOS, with a glibc 2.17 baseline for Linux via `cargo-zigbuild`.
3. CycloneDX SBOM (`toon-mcp-server-sbom.cdx.json`) generated by `cargo-cyclonedx`.
4. `checksums.sha256` with self-verification (`sha256sum -c`) inside the release job.
5. Sigstore keyless signatures (`*.sigstore.json`) over every published asset, plus GitHub artifact attestations.

To verify a downloaded artifact before installing, see [docs/deployment.md → Verifying a release artifact](docs/deployment.md#verifying-a-release-artifact). The short form:

```bash
sha256sum --ignore-missing -c checksums.sha256
cosign verify-blob \
  --bundle <asset>.sigstore.json \
  --certificate-identity-regexp "https://github.com/cipher-rc5/toon-mcp/.+" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  <asset>
```

---

## Documentation

| Document                                       | Contents                                                                                |
| ---------------------------------------------- | --------------------------------------------------------------------------------------- |
| [docs/overview.md](docs/overview.md)           | What the server does and why it exists                                                  |
| [docs/architecture.md](docs/architecture.md)   | Crate dependency graph, data flow, layer rules, design rationale                        |
| [docs/algorithms.md](docs/algorithms.md)       | Format detection, shape classification, and TOON encoding algorithms                    |
| [docs/configuration.md](docs/configuration.md) | Full configuration reference with tuning guidance                                       |
| [docs/logging.md](docs/logging.md)             | `LogSink` trait, `LogEvent` schema, on-disk layout, and example queries                 |
| [docs/runbook.md](docs/runbook.md)             | Operator runbook: diagnostics and remediation for production incidents                  |
| [docs/production.md](docs/production.md)       | Production readiness, sizing, durability, known non-goals                               |
| [docs/deployment.md](docs/deployment.md)       | Release-binary deployment examples (systemd, launchd, Claude Desktop) + verification    |
| [docs/testing.md](docs/testing.md)             | Existing test suite, gaps, and how to add new tests                                     |
| [docs/adr/](docs/adr/)                         | Architecture Decision Records                                                           |
| [CHANGELOG.md](CHANGELOG.md)                   | Release history and the in-progress `[Unreleased]` section                              |
| [SECURITY.md](.github/SECURITY.md)             | Vulnerability reporting policy and supported versions                                   |

---

## Contributing

Pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening one — it covers the branch model, semver policy, pre-commit gate, and dependency-pinning rules.

A short version:

- Branch off `master` (the active branch). CI also runs on `main` while both names exist.
- Run the [pre-commit gate](#pre-commit-gate) locally before pushing.
- Update [`CHANGELOG.md`](CHANGELOG.md) under `[Unreleased]` as part of the PR.
- Pin every new workspace dependency exactly with `=` in `[workspace.dependencies]`.

---

## Security

Report vulnerabilities through GitHub's private vulnerability reporting at <https://github.com/cipher-rc5/toon-mcp/security/advisories/new>. Do **not** open a public issue. Full policy and scope: [SECURITY.md](.github/SECURITY.md).

---

## License

Distributed under the [Business Source License 1.1](LICENSE).
