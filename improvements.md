# toon-mcp — Production-Grade Improvements Checklist

Items are ordered by severity and dependency. Complete blockers before high,
high before medium. Each item references the corresponding finding in
`critical_analysis.md`.

---

## Critical Blockers

- [x] **C1** — Wrap `FormatDetector::detect` call in `handle_detect_format` with
  `tokio::task::spawn_blocking` to prevent blocking the async executor.
  Add a per-call timeout via `tokio::time::timeout` matching the pattern
  already used in `handle_compress_content`.
  _File: `crates/toon-mcp-server/src/handler.rs:91`_

- [x] **C2 / C4** — Fix `ParquetSink::flush()` to use a `oneshot` channel for
  write acknowledgement, mirroring the `shutdown()` implementation.
  Update `main.rs` shutdown to call `Box::new(sink).shutdown().await` instead
  of the current `flush() + sleep(200ms)` pattern.
  _Files: `crates/toon-mcp-logging/src/parquet_sink.rs:103`,
  `crates/toon-mcp-server/src/main.rs:66`_

- [x] **C3** — Add a `tokio::signal` handler for SIGTERM and SIGINT that triggers
  the graceful shutdown sequence (flush + acknowledged drain) before the
  process exits. Use `tokio::select!` in `main` to race the MCP service
  future against the signal future.
  _File: `crates/toon-mcp-server/src/main.rs`_

- [x] **C5a** — Rename `ParquetSink` to `JsonlSink` and `ParquetSinkConfig` to
  `JsonlSinkConfig`. Update all references in source, docs, and tests. The
  current name is factually wrong and misleads operators and contributors.
  _Files: `crates/toon-mcp-logging/src/parquet_sink.rs`, `src/lib.rs`,
  `crates/toon-mcp-server/src/main.rs`, `README.md`, `docs/logging.md`_

- [x] **C5b** — Remove the `duckdb` external reference from `AGENTS.md` and the
  forward-reference to `duckdb_sink.rs` from `docs/architecture.md` until
  the DuckDB sink is actually implemented. Replace with accurate description
  of the current JSONL implementation.
  _Files: `AGENTS.md:103`, `docs/architecture.md:180`_

---

## High Severity

- [x] **H1** — Replace the silent `unwrap_or_else(|_| "{}".into())` fallback in
  `flush_pending` with an explicit `match` that emits a `tracing::error!`
  with the event ID before skipping, so serialization failures are visible
  in operator logs.
  _File: `crates/toon-mcp-logging/src/parquet_sink.rs:202`_

- [x] **H2** — Change `spawn_blocking` `JoinError` mapping from
  `McpError::invalid_params` to `McpError::internal_error` (or the
  equivalent rmcp error code for server-internal failures). Client code that
  branches on MCP error codes will correctly classify this as a server fault,
  not a bad request.
  _File: `crates/toon-mcp-server/src/handler.rs:209, 365`_

- [x] **H3** — Extend `CompressDecision::PassedThrough` to carry the detected
  `InputFormat` alongside the `PassThroughReason`, eliminating the redundant
  `FormatDetector::detect` call in `handle_compress_content` and
  `handle_compression_stats` for pass-through branches.
  _Files: `crates/toon-mcp-core/src/compressor.rs`,
  `crates/toon-mcp-server/src/handler.rs:242, 370`_

- [x] **H4** — Remove the standalone `toon-format` declaration from
  `crates/toon-mcp-server/Cargo.toml:31` and replace it with
  `toon-format.workspace = true` so there is a single source of truth for
  the version.
  _File: `crates/toon-mcp-server/Cargo.toml:31`_

- [x] **H5** — Replace the `unsafe { std::env::set_var }` pattern in config tests
  with the `temp-env` crate (or equivalent) which provides a safe,
  thread-isolated wrapper for temporary env var overrides. Remove the manual
  `ENV_LOCK` mutex.
  _File: `crates/toon-mcp-server/src/config.rs:233`_

- [x] **H6** — Write an MCP transport integration test. Spawn the server binary
  (or construct `ToonMcpServer` directly with an in-memory transport) and
  send at minimum one `tools/call` for each of the three tools via the rmcp
  client. Assert the tool name routing, JSON parameter deserialization, and
  response structure are correct.
  _File: `crates/toon-mcp-server/src/server.rs`_

---

## Medium Severity

- [x] **M1** — Add a concurrency gate to the handler layer. Use a
  `tokio::sync::Semaphore` with a configurable permit count
  (`TOON_MAX_CONCURRENT_CALLS`, default 8) to limit simultaneous
  `spawn_blocking` dispatches. Return `McpError::internal_error` with a
  "server busy" message when the semaphore cannot be acquired within a
  short timeout.

- [x] **M2** — At `Config::load()` startup, detect when `log_dir` is a relative
  path and emit `tracing::warn!` advising the operator to use an absolute
  path. Optionally canonicalize to an absolute path using
  `std::fs::canonicalize` if the directory already exists, and fail startup
  with `ServerError::LoggingInit` if it does not and logging is enabled.
  _File: `crates/toon-mcp-server/src/config.rs:86`_

- [x] **M3** — Implement automatic restart of the writer task in the supervisor.
  Alternatively, treat writer task exit as a fatal startup failure that
  should bring down the server cleanly (logging-enabled deployments cannot
  silently lose observability). At minimum, emit a structured health metric
  that a monitoring system can observe.
  _File: `crates/toon-mcp-server/src/main.rs:38`_

- [x] **M4** — Add a readiness signal. Options include:
  - Write a `ready` file to a configured path on successful MCP `initialize`
    handshake completion, readable by a process supervisor.
  - Emit a structured `tracing::info!` event with a stable message string
    that a log scraper can match (`toon-mcp ready` as an alerting anchor).
  The existing `info!("toon-mcp-server ready")` at line 58 is a start but
  is not structured or machine-parseable.

- [x] **M5** — Remove the `async_trait` dependency from `toon-mcp-logging`. Rewrite
  the `LogSink` trait using native `async fn` in trait syntax (stable since
  Rust 1.75, toolchain is 1.87.0). This eliminates one heap allocation per
  `record()` call and removes a dependency.
  _File: `crates/toon-mcp-logging/src/sink.rs`_

- [x] **M6** — Emit the `event_id` into the active `tracing` span at the start of
  each tool handler. This links `tracing` log output to `LogEvent` records,
  making it possible to correlate a tracing error with its corresponding
  audit entry during incident investigation.
  _File: `crates/toon-mcp-server/src/handler.rs`_

- [x] **M7** — Pin the GitHub Actions runner to a specific Ubuntu version
  (e.g., `ubuntu-24.04`) to prevent silent CI breakage on runner image
  updates.
  _File: `.github/workflows/ci.yml:17`_

- [x] **M8** — Add code coverage measurement to CI using `cargo-llvm-cov` or
  `cargo-tarpaulin`. Set a minimum coverage threshold (suggested: 70% line
  coverage) that fails the CI run if not met. Publish the coverage report as
  a CI artifact.
  _File: `.github/workflows/ci.yml`_

---

## Low Severity

- [x] **L1** — In `handle_compress_content` and `handle_compression_stats`, move
  `input` directly into the `spawn_blocking` closure instead of cloning it.
  Since `input` is already owned at that point, no copy is needed.
  _File: `crates/toon-mcp-server/src/handler.rs:194, 350`_

- [x] **L2** — Replace the 7-element tuple in `handle_compress_content` with a
  named intermediate struct (e.g., `CompressOutcome`) to make the match arm
  self-documenting and resilient to field additions.
  _File: `crates/toon-mcp-server/src/handler.rs:216`_

- [x] **L3** — Refactor `flush_pending` in the writer task to hold the JSONL file
  handles open between flushes. Re-open the handle only when the day partition
  changes (UTC day boundary). This reduces `open(2)` / `close(2)` syscall
  overhead under sustained load.
  _File: `crates/toon-mcp-logging/src/parquet_sink.rs:207`_

- [x] **L4** — (Blocked on C5a) After renaming `ParquetSink` to `JsonlSink`,
  document in `docs/logging.md` and the `README.md` that DuckDB querying
  works because DuckDB can read JSONL natively, not because the sink writes
  Parquet.

- [x] **L5** — Add `detect_and_parse` round-trip tests for `Jsonl`, `Csv`, and
  `Tsv` formats in `detector.rs`. Verify the parsed `Value` structure matches
  expectations (array of objects, correct field types).
  _File: `crates/toon-mcp-core/src/detector.rs`_

- [x] **L6** — Add parser edge-case tests for:
  - Empty string input
  - Whitespace-only input
  - Single-row CSV (header only, no data rows)
  - JSONL with only one non-empty line
  - Inputs containing non-ASCII / Unicode field names and values
  _Files: `crates/toon-mcp-core/src/parser/*.rs`, `detector.rs`_

- [x] **L7** — Add `#![deny(missing_docs)]` to `lib.rs` in `toon-mcp-core` and
  `toon-mcp-logging` so that missing doc comments on public items are caught
  at compile time locally, not only in CI.
  _Files: `crates/toon-mcp-core/src/lib.rs`,
  `crates/toon-mcp-logging/src/lib.rs`_

- [x] **L8** — Document the `opencode.json` schema in `README.md` under the
  opencode integration section. Explain which fields are meaningful and how
  to add or override env vars for local development.
  _File: `README.md`_

- [x] **L9** — Add a `CHANGELOG.md` tracking version history. Even at `0.1.0` a
  changelog establishes the practice and gives operators a reference for
  what changed between releases.

- [x] **L10** — Add a `SECURITY.md` file describing the responsible disclosure
  process. The repository is public; without a security policy, researchers
  do not know how to report vulnerabilities.
  _File: `.github/SECURITY.md`_

- [x] **L11** — Add `.github/ISSUE_TEMPLATE/` templates for bug reports and feature
  requests to improve contributor signal quality.

---

## Verification Gate

After completing each tier, run the following before moving to the next:

```bash
cargo fmt && cargo clippy --workspace -- -D warnings && cargo test --workspace && cargo build --release --package toon-mcp-server
```

For the transport integration test (H6), also run:

```bash
cargo test --package toon-mcp-server -- --test-threads=1
```

For code coverage (M8), once configured:

```bash
cargo llvm-cov --workspace --lcov --output-path lcov.info
```
