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

# Run benchmarks (does not run tests)
cargo bench --package toon-mcp-bench
```

---

## What Is Already Tested

All tests are inline `#[cfg(test)]` modules — there are no separate `tests/`
directories. `toon-mcp-bench` contributes 0 unit tests (benchmark binaries use
`harness = false` and only run via `cargo bench`, not `cargo test`).

| Layer | Location | Style | Count |
|-------|----------|-------|-------|
| Parsers (JSON, JSONL, CSV) | `crates/toon-mcp-core/src/parser/*.rs` | `#[test]` | 11 |
| Format detector | `crates/toon-mcp-core/src/detector.rs` | `#[test]` | 9 |
| Shape classifier | `crates/toon-mcp-core/src/classifier.rs` | `#[test]` | 12 |
| Compressor pipeline | `crates/toon-mcp-core/src/compressor.rs` | `#[test]` | 8 |
| JSONL sink | `crates/toon-mcp-logging/src/parquet_sink.rs` | `#[tokio::test]` | 3 |
| Tool handlers | `crates/toon-mcp-server/src/handler.rs` | `#[tokio::test]` | 16 |
| Config loading | `crates/toon-mcp-server/src/config.rs` | `#[test]` | 9 |

Doc-tests in `toon-mcp-core` add a further 14 runnable examples.

The handler tests use both `NoopSink` and `MemorySink`. The `MemorySink`
tests assert on emitted `LogEvent` fields (tool name, input size, compressed
flag, savings ratio, format detected, pass reason).

### Benchmarks (18 Criterion functions)

Three benchmark binaries live in `crates/toon-mcp-bench/benches/`, each backed
by fixtures in `crates/toon-mcp-bench/fixtures/`. Baselines are committed to
`bench/baselines/main/`.

| Bench file | Group | Functions |
|------------|-------|-----------|
| `detection.rs` | `detect_format` | 6 |
| `classification.rs` | `classify_shape` | 6 |
| `compression.rs` | `pipeline` | 6 |

---

## Known Gaps

### 1. `server.rs` tool routing

The rmcp `tool_router!` / `tool_handler!` macro layer is untested. This would
require either a mock transport or a lightweight stdio round-trip test to
verify that tool names are routed to the correct handler functions.

### 2. Parser edge cases

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
