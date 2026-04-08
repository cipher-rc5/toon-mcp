# toon-mcp — Production-Grade Improvements Checklist

Each item maps to one or more findings in `critical_analysis.md`. Items are grouped by phase. Complete phases in order — Phase 1 items are blocking; phases 2-4 build on them.

---

## Phase 1 — Correctness and Safety (Blocking)

These must be resolved before any user-facing or production deployment.

### P1.1 — Remove committed runtime artifact
- [ ] Delete `data/1774347477_evaluation.jsonl` from the repository
- [ ] Add `data/*.jsonl` to `.gitignore` to prevent recurrence
- [ ] Run `git rm --cached data/1774347477_evaluation.jsonl` and commit
- [ ] Audit git history; if the file contains proprietary data, consider a `git filter-repo` rewrite
- **Ref:** C5

### P1.2 — Enforce input size limit on all tool handlers
- [ ] Define a constant `MAX_INPUT_BYTES: usize` (e.g., `10 * 1024 * 1024` for 10 MB)
- [ ] Add byte-length check at the top of each handler before any allocation: if `input.len() > MAX_INPUT_BYTES` return an `McpError` immediately
- [ ] Expose `MAX_INPUT_BYTES` as a configurable `TOON_MAX_INPUT_BYTES` env var in `Config`
- [ ] Add env var to `.env.example` and configuration docs
- [ ] Write a handler test that confirms oversized input returns the correct error
- **Ref:** C1

### P1.3 — Fix threshold semantics documentation and rename the field
- [ ] Rename `CompressConfig::threshold` to `max_output_ratio` (or `size_ratio_limit`) to make the semantics unambiguous
- [ ] Update all field doc comments to: "TOON output must be at most this fraction of input bytes (e.g., 0.85 means output ≤ 85% of input)"
- [ ] Update `docs/architecture.md`, `docs/configuration.md`, `docs/algorithms.md`, and `README.md` to use consistent terminology throughout
- [ ] Update `TOON_COMPRESSION_THRESHOLD` env var name or add a clear header note at the top of the configuration doc explaining the sign convention
- [ ] Add a `#[test]` in `compressor.rs` that explicitly verifies `threshold=0.85` with known byte counts to serve as regression documentation
- **Ref:** C2

### P1.4 — Implement graceful shutdown in main.rs
- [ ] After `service.waiting().await?`, call `Arc::into_inner` or extract the sink before drop
- [ ] If the sink is a `ParquetSink`, call `Box::new(sink).shutdown().await` with a timeout (e.g., `tokio::time::timeout(Duration::from_secs(5), sink.shutdown())`)
- [ ] Log a warning if shutdown times out, then proceed with process exit
- [ ] Write a test that verifies events are flushed on clean shutdown (existing `parquet_sink_flushes_to_jsonl` partially covers this; extend it to test the main shutdown path)
- **Ref:** H1

### P1.5 — Add logging to `Config::load()` for invalid and defaulted values
- [ ] Add `tracing::warn!` emission in each `env_*` helper when a value is present but unparseable
- [ ] Log all resolved config values at `tracing::debug!` level on startup to match what `docs/configuration.md` claims
- [ ] Add a `tracing::warn!` when `TOON_COMPRESSION_THRESHOLD` is outside `[0.0, 1.0]`
- [ ] Add a `tracing::warn!` when `TOON_MIN_BYTES = 0`
- [ ] Write `Config::load()` tests using `temp-env` crate (add to dev-dependencies) covering: valid values, invalid parse fallback, missing var fallback, and boolean/delimiter parsing
- **Ref:** H2, M6, L1

### P1.6 — Monitor `ParquetSink` writer task health
- [ ] Capture the `JoinHandle` returned by `tokio::spawn(task)` in `main.rs`
- [ ] Spawn a supervisor task that `.await`s the handle and logs `tracing::error!` if the writer task exits unexpectedly
- [ ] Consider adding a `task_running: AtomicBool` flag to `ParquetSink` so `record()` can return an error rather than silently queuing into a dropped channel
- [ ] Add a test that verifies `record()` returns `LogError::ChannelSend` after the writer task has been dropped
- **Ref:** C4

---

## Phase 2 — Architecture and Design Correctness

These fix structural design flaws that will complicate maintenance and feature additions.

### P2.1 — Eliminate the double-parse on successful compression
- [ ] Add `input_format: InputFormat` and `shape_class: ShapeClass` fields to `CompressDecision::Compressed` variant
- [ ] Populate these fields inside `Compressor::decide` — the format and shape are already computed there
- [ ] Remove the re-detection and re-classification code from `handle_compress_content` and `handle_compression_stats`
- [ ] Update tests to assert on the new fields in `CompressDecision::Compressed`
- **Ref:** C3

### P2.2 — Move `column_count` detection out of the server handler into core
- [ ] Add `column_count: Option<usize>` to `InputFormat` or expose a helper in `toon-mcp-core` that returns column count for CSV/TSV without re-parsing the full document
- [ ] Remove the `csv` dependency from `toon-mcp-server/Cargo.toml`
- [ ] Update `handle_detect_format` to use the new core function
- **Ref:** M1

### P2.3 — Extract `CompressConfig::from(&Config)` impl to eliminate handler duplication
- [ ] Add `impl From<&Config> for CompressConfig` in either `toon-mcp-server/src/config.rs` or a new `crates/toon-mcp-server/src/convert.rs`
- [ ] Replace the 8-line `let compress_config = CompressConfig { ... }` block in both handlers with `CompressConfig::from(config.as_ref())`
- **Ref:** L3

### P2.4 — Replace `PassThroughReason::as_str` `String` allocation with `&'static str`
- [ ] Change the return type of `as_str` to `Cow<'static, str>` or split into two methods: `as_str(&self) -> &'static str` for non-parameterised variants and a `Display` impl that formats the parameterised ones
- [ ] Or: simply return `&'static str` for all variants and format the embedded data only in `Display`
- [ ] Update all call sites
- **Ref:** S1

### P2.5 — Rework `LogSink::shutdown` ergonomics
- [ ] Change `shutdown(self: Box<Self>)` to `shutdown(mut self: Box<Self>)` at minimum, or consider `async fn shutdown(&mut self)` if the boxed pattern causes ergonomic issues at call sites
- [ ] Document why `Box<Self>` is required (to allow the trait to be object-safe with consuming semantics)
- **Ref:** S2

---

## Phase 3 — Dependency and Build Hygiene

### P3.1 — Pin all workspace dependencies to exact versions
- [ ] Update every entry in `[workspace.dependencies]` to use `=x.y.z` exact pinning
- [ ] Run `cargo update` first to get current resolved versions from `Cargo.lock`
- [ ] Copy exact versions from `Cargo.lock` into the manifest
- [ ] Document that `cargo update` must be a deliberate, reviewed action going forward
- **Ref:** M7

### P3.2 — Remove unused `duckdb` from workspace dependencies
- [ ] Remove `duckdb` from `[workspace.dependencies]` in root `Cargo.toml`
- [ ] Verify no crate actually depends on it (`cargo tree | grep duckdb`)
- [ ] If future DuckDB integration is planned, document it as a commented-out entry with a tracking issue reference
- **Ref:** M9

### P3.3 — Replace deprecated `DateTime::from_timestamp`
- [ ] In `parquet_sink.rs:234`, replace `DateTime::<Utc>::from_timestamp(secs, nanos)` with `DateTime::from_timestamp(secs, nanos)` from the chrono 0.4.27+ API, or use `Utc.timestamp_opt(secs, nanos).single()` with appropriate fallback
- [ ] Verify the replacement compiles without deprecation warnings
- [ ] Add a clippy `#![deny(deprecated)]` attribute to the logging crate
- **Ref:** H4

### P3.4 — Add LICENSE file
- [ ] Decide on a license (MIT, Apache-2.0, or dual MIT/Apache-2.0 are conventional for Rust tools)
- [ ] Create `LICENSE` (or `LICENSE-MIT` / `LICENSE-APACHE`) at the repository root
- [ ] Add `license = "MIT"` (or chosen SPDX identifier) to `[workspace.package]`
- [ ] Update the README to remove the "if present" qualifier
- **Ref:** H7

### P3.5 — Add crate metadata to all `Cargo.toml` files
- [ ] Add to `[workspace.package]`: `version`, `authors`, `description`, `repository`, `keywords`, `categories`, `license`
- [ ] Use `version.workspace = true`, `license.workspace = true`, etc. in each crate's `Cargo.toml`
- [ ] Ensure `toon-mcp-bench` is marked `publish = false`
- **Ref:** H6

### P3.6 — Remove empty `src/lib.rs` from `toon-mcp-bench`
- [ ] Delete `crates/toon-mcp-bench/src/lib.rs`
- [ ] Remove the implicit `[lib]` that Cargo generates from it
- [ ] Verify `cargo bench --package toon-mcp-bench` still works
- **Ref:** L6

---

## Phase 4 — Testing and Observability

### P4.1 — Write `MemorySink` integration tests for all three handlers
- [ ] For `handle_detect_format`: verify `tool_name`, `input_format`, `input_bytes`, `compressed=false`, `savings_pct=0.0`, `duration_us > 0` are correctly populated in emitted `LogEvent`
- [ ] For `handle_compress_content` (compressed path): verify `compressed=true`, `savings_pct > 0.0`, correct `format` and `shape_class`
- [ ] For `handle_compress_content` (pass-through path): verify `compressed=false`, `pass_reason` is non-null
- [ ] For `handle_compression_stats`: verify `would_compress` reflects the actual pipeline decision
- **Ref:** M5

### P4.2 — Write `Config::load()` unit tests
- [ ] Add `temp-env` or `serial_test` to `toon-mcp-server/dev-dependencies`
- [ ] Test: default values used when no vars set
- [ ] Test: `TOON_COMPRESSION_THRESHOLD=0.7` overrides correctly
- [ ] Test: `TOON_MIN_BYTES=abc` falls back to default (and emits a warn after P1.5)
- [ ] Test: `TOON_LOG_ENABLED=yes` parses as `true`
- [ ] Test: `TOON_DELIMITER=pipe` maps to `Delimiter::Pipe`
- [ ] Test: `TOON_CLIENT_HINT=` (empty string) sets `client_hint` to `None`
- **Ref:** M6

### P4.3 — Write `server.rs` routing smoke tests
- [ ] Create a test that constructs a `ToonMcpServer` with a `MemorySink` and calls `detect_format`, `compress_content`, and `compression_stats` directly through the handler interface
- [ ] Alternatively, use rmcp's test transport if available, or write a JSON-RPC frame round-trip test
- **Ref:** (docs/testing.md — Known Gap 3)

### P4.4 — Write parser edge-case tests
- [ ] Empty string input to each parser
- [ ] Whitespace-only input
- [ ] Unicode field names and values in CSV/JSON
- [ ] JSONL with trailing blank line
- [ ] Very large CSV (>10 MB) hitting the byte gate in `Compressor::decide`
- [ ] JSON/JSONL with deeply nested structures (stack overflow risk for recursive classifiers)
- **Ref:** (docs/testing.md — Known Gap 5)

### P4.5 — Commit Criterion benchmark baselines
- [ ] Fix `.gitignore` to not exclude `bench/baselines/`
- [ ] Create `bench/baselines/` directory
- [ ] Run `cargo bench --package toon-mcp-bench -- --save-baseline main`
- [ ] Commit the resulting `bench/baselines/main/` directory
- [ ] Document the baseline capture and comparison workflow in `docs/testing.md`
- **Ref:** H8

### P4.6 — Add per-call timeout to handler pipeline
- [ ] Wrap `Compressor::decide` (or the spawn_blocking call if moved there) with `tokio::time::timeout`
- [ ] Make the timeout configurable via `TOON_PIPELINE_TIMEOUT_MS` env var (default: `30_000`)
- [ ] Return an `McpError` with a clear message on timeout
- [ ] Write a test that verifies timeout behavior with a mock that sleeps
- **Ref:** H3

---

## Phase 5 — CI, Documentation, and Operational Readiness

### P5.1 — Add CI pipeline
- [ ] Create `.github/workflows/ci.yml` with jobs:
  - `cargo fmt --check`
  - `cargo clippy -- -D warnings`
  - `cargo test --workspace`
  - `cargo doc --no-deps --workspace`
  - `cargo build --release`
- [ ] Run CI on push to `main` and on all pull requests
- [ ] Add a CI status badge to `README.md`
- **Ref:** L5

### P5.2 — Fix `.env.example` to match current implementation
- [ ] Remove `TOON_LOG_DB_PATH` and `TOON_LOG_PARQUET_DIR` (DuckDB-era artifacts)
- [ ] Add `TOON_LOG_DIR=data/logs` with the correct comment
- [ ] Add `TOON_MAX_INPUT_BYTES` after P1.2 is implemented
- [ ] Align all comments with the actual `Config::load()` behavior
- **Ref:** M2

### P5.3 — Fix `docs/testing.md` inaccuracies
- [ ] Replace the reference to `duckdb_sink.rs` with `parquet_sink.rs`
- [ ] Update the test count table to reflect actual numbers (47 unit + 12 doctests = 59 total)
- [ ] Remove or update the "DuckDB sink" row to reflect `ParquetSink` / JSONL
- **Ref:** M3

### P5.4 — Fix `docs/algorithms.md` pseudocode for `probe_delimited`
- [ ] Update the pseudocode listing in Stage 1 to match the actual `detector.rs:157-177` implementation (`has_headers: true`, reads headers then one data record)
- [ ] Add a note explaining why `has_headers: true` is used (to check that the first record has consistent column count relative to the header)
- **Ref:** M4

### P5.5 — Resolve `ServerError` dead code
- [ ] Either use `ServerError` as the return type in `main.rs` (changing `Box<dyn std::error::Error>` to `ServerError`), or
- [ ] Remove `ServerError` entirely and use `Box<dyn std::error::Error>` explicitly with a comment
- [ ] Remove `#[allow(dead_code)]`
- [ ] Note: using `ServerError` in `main.rs` would satisfy the AGENTS.md rule against `Box<dyn Error>` in public API return types
- **Ref:** H5

### P5.6 — Add operational runbook to documentation
- [ ] Create `docs/operations.md` covering:
  - How to verify the server is running (health check via MCP `list_tools`)
  - How to query logs with DuckDB (expand on the README examples)
  - What to do when `ChannelSend` errors appear in stderr
  - Log rotation strategy for long-running deployments (JSONL files grow unbounded)
  - How to reload configuration (requires server restart — document this)
  - Troubleshooting: binary not found, absolute path requirement for Claude Desktop

### P5.7 — Add log rotation or partitioned file size limit
- [ ] The JSONL sink appends indefinitely to `day=YYYY-MM-DD/events.jsonl` with no size limit
- [ ] For long-running deployments, a single day's file could grow to gigabytes
- [ ] Add a configurable `TOON_LOG_MAX_FILE_BYTES` that rotates to `events.001.jsonl`, `events.002.jsonl`, etc.
- [ ] Alternatively, document the lack of rotation and provide a `logrotate` configuration example

---

## Summary: Minimum Viable Production Checklist

The following items from Phase 1 are the minimum required before any production or shared deployment:

- [ ] P1.1 — Remove committed evaluation data
- [ ] P1.2 — Input size limit
- [ ] P1.4 — Graceful shutdown (prevents log data loss)
- [ ] P1.5 — Config warning emission (operational safety)
- [ ] P1.6 — Writer task monitoring
- [ ] P3.4 — Add LICENSE file

Without these six items, the server is not suitable for use beyond local development.
