# toon-mcp — Testing Guide

---

## Running Existing Tests

```bash
# Run all unit tests across the workspace
cargo test --workspace

# Run tests for a specific crate
cargo test --package toon-mcp-core
cargo test --package toon-mcp-logging
cargo test --package toon-mcp-server

# Run benchmarks
cargo bench --package toon-mcp-bench
```

---

## What Is Already Tested (46 unit tests)

All tests are inline `#[cfg(test)]` modules — there are no separate `tests/`
directories. `toon-mcp-bench` contributes 0 unit tests (benchmark binaries use
`harness = false` and only run via `cargo bench`, not `cargo test`). Doc-tests
across all crates also report 0 because no doc comments contain runnable code
examples.

| Layer | Location | Style | Count |
|-------|----------|-------|-------|
| Parsers (JSON, JSONL, CSV) | `crates/toon-mcp-core/src/parser/*.rs` | `#[test]` | 11 |
| Format detector | `crates/toon-mcp-core/src/detector.rs:144` | `#[test]` | 9 |
| Shape classifier | `crates/toon-mcp-core/src/classifier.rs:173` | `#[test]` | 12 |
| Compressor pipeline | `crates/toon-mcp-core/src/compressor.rs:209` | `#[test]` | 7 |
| DuckDB sink | `crates/toon-mcp-logging/src/duckdb_sink.rs:275` | `#[tokio::test]` | 2 |
| Tool handlers | `crates/toon-mcp-server/src/handler.rs:361` | `#[tokio::test]` | 5 |

The handler tests use `NoopSink` with a real `Compressor`. The DuckDB sink
tests use an in-memory DuckDB instance (no filesystem required).

### Benchmarks (18 Criterion functions)

Three benchmark binaries live in `crates/toon-mcp-bench/benches/`, each backed
by fixtures in `crates/toon-mcp-bench/fixtures/`.

| Bench file | Group | Functions |
|------------|-------|-----------|
| `detection.rs` | `detect_format` | 6 |
| `classification.rs` | `classify_shape` | 6 |
| `compression.rs` | `pipeline` | 6 |

---

## Known Gaps

### 1. Integration tests via `MemorySink`

`crates/toon-mcp-logging/src/memory_sink.rs` was explicitly built for
integration testing. It exposes an `Arc<Mutex<Vec<LogEvent>>>` that can be
inspected after a handler call. No integration tests exist yet that use it.

Suggested approach: write `#[tokio::test]` functions that construct a handler
with `MemorySink`, fire a tool call, then assert on the emitted `LogEvent`
fields (tool name, input size, savings ratio, format detected, etc.).

### 2. `config.rs` env-var parsing

`Config::load()` reads all `TOON_*` environment variables and is completely
untested. Tests can be written using `std::env::set_var` scoped carefully, or
with the `temp-env` crate to avoid inter-test pollution. Consider pairing with
`serial_test` if tests mutate global env state.

### 3. `server.rs` tool routing

The rmcp `tool_router!` / `tool_handler!` macro layer is untested. This would
require either a mock transport or a lightweight stdio round-trip test to
verify that tool names are routed to the correct handler functions.

### 4. Benchmark baselines

`AGENTS.md` specifies that Criterion baseline snapshots should be committed to
`bench/baselines/`. That directory does not exist yet. Running
`cargo bench --package toon-mcp-bench` and saving the output establishes a
regression guard for future performance work.

### 5. Parser edge cases

The existing parser tests cover the happy path. Missing coverage includes:

- Malformed or truncated inputs
- Empty strings and whitespace-only inputs
- Unicode content and non-ASCII field names
- Very large payloads (stress the byte-gate in `Compressor`)
- JSONL files with a blank trailing line

---

## Pre-Commit Gate

Per `AGENTS.md`, the following command **must pass** before every commit:

```bash
cargo fmt && cargo clippy -- -D warnings && cargo test --workspace
```

All three steps are mandatory. Do not use `--no-verify` to bypass them.
