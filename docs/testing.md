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

| Layer                      | Location                                        | Style            | Count |
| -------------------------- | ----------------------------------------------- | ---------------- | ----- |
| Parsers (JSON, JSONL, CSV) | `crates/toon-mcp-core/src/parser/*.rs`          | `#[test]`        | 20+ (incl. proptests) |
| Format detector            | `crates/toon-mcp-core/src/detector.rs`          | `#[test]`        | 19    |
| Shape classifier           | `crates/toon-mcp-core/src/classifier.rs`        | `#[test]`        | 18+ (incl. proptests) |
| Compressor pipeline        | `crates/toon-mcp-core/src/compressor.rs`        | `#[test]`        | 8     |
| JSONL sink                 | `crates/toon-mcp-logging/src/jsonl_sink.rs`     | `#[tokio::test]` | 12    |
| Tool handlers              | `crates/toon-mcp-server/src/handler.rs`         | `#[tokio::test]` | 20+ (incl. permit-release, schema property tests, diagnostics aggregation) |
| Config loading             | `crates/toon-mcp-server/src/config.rs`          | `#[test]`        | 10    |
| MCP transport              | `crates/toon-mcp-server/tests/mcp_transport.rs` | `#[tokio::test]` | 10    |

Workspace `cargo test --workspace` reports ~180 unit/integration tests plus 16 doc-tests; the per-row counts above are approximate and may lag the codebase between releases.

Doc-tests in `toon-mcp-core` add a further 14 runnable examples.

The handler tests use both `NoopSink` and `MemorySink`. The `MemorySink`
tests assert on emitted `LogEvent` fields (tool name, input size, compressed
flag, savings ratio, format detected, pass reason).

The MCP transport integration tests (`tests/mcp_transport.rs`) use an
in-memory `tokio::io::duplex` transport and the rmcp client API to verify
that tool names are routed to the correct handler, JSON parameters are
deserialised correctly, and responses are well-formed.

### Benchmarks (Criterion)

Four benchmark binaries live in `crates/toon-mcp-bench/benches/`, each backed
by fixtures in `crates/toon-mcp-bench/fixtures/`. Baselines are committed to
`bench/baselines/main/`. The `jsonl_sink` async-throughput benchmark lives
separately from the sync core benches and is the only one that starts a tokio
runtime (`current_thread`, for deterministic measurement).

| Bench file          | Group            | Scope                         |
| ------------------- | ---------------- | ----------------------------- |
| `detection.rs`      | `detect_format`  | Sync core                     |
| `classification.rs` | `classify_shape` | Sync core                     |
| `compression.rs`    | `pipeline`       | Sync core                     |
| `jsonl_sink.rs`     | `jsonl_sink`     | Async logging sink throughput |

---

## Known Gaps

### 1. Malformed / truncated inputs

Truncated JSON mid-array or mid-object is not explicitly covered. The
underlying `serde_json` error path is exercised but not the specific
truncation case.

A `cargo-fuzz` harness exists in `fuzz/` that targets the four
untrusted-input entry points (`FormatDetector::detect_and_parse`,
`JsonParser::parse`, `JsonlParser::parse`, and `CsvParser::parse`). Fuzzing
is not part of CI and requires the nightly toolchain plus `cargo-fuzz`. See
[CONTRIBUTING.md](../CONTRIBUTING.md#fuzz-testing) for setup and run
instructions.

---

## Pre-Commit Gate

Per `CONTRIBUTING.md`, the following command **must pass** before every commit:

```bash
cargo fmt && cargo clippy -- -D warnings && cargo test --workspace
```

All three steps are mandatory. Do not use `--no-verify` to bypass them.
