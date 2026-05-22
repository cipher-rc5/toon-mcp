# Changelog

All notable changes to toon-mcp are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased] — 0.2.0 (in progress)

### Changed

- Bumped the pinned Rust toolchain from 1.93.0 to 1.95.0 (`rust-toolchain.toml`, `Cargo.toml` `rust-version`, and all documentation references). Downstream consumers must use Rust 1.95.0 or newer.
- Renamed `ParquetSink` to `JsonlSink` and `ParquetSinkConfig` to
  `JsonlSinkConfig`. The sink writes JSONL, not Parquet; the previous name
  was misleading. Update any custom code that references these types.

### Fixed

- Removed stale `AGENTS.md` references from `docs/LLM_RULES.md`, `docs/testing.md`, `docs/adr/0001-async-benches.md`, and `opencode.json`. The authoritative contributor rules now live in `CONTRIBUTING.md`.
- `detect_format` handler now runs `FormatDetector::detect` on a
  `spawn_blocking` thread, preventing it from stalling the tokio executor
  under large inputs.
- `JsonlSink::flush()` now sends an acknowledged `Flush` command and blocks
  until the writer task confirms the data is on disk. Previously it was
  fire-and-forget and returned before any I/O completed.
- Server shutdown now calls the acknowledged `flush()` and drops the `Arc`
  to trigger clean writer task exit. The previous `sleep(200ms)` race
  condition is eliminated.
- `spawn_blocking` `JoinError` is now reported to MCP clients as
  `internal_error` instead of `invalid_params`.
- Serialization failures in `flush_pending` now emit a `tracing::error!`
  with the event ID instead of silently replacing the event with `{}`.
- Removed duplicate `toon-format` declaration from `toon-mcp-server/Cargo.toml`.
- Replaced `unsafe { std::env::set_var }` in config tests with `temp-env`.

### Added

- SIGTERM and SIGINT handlers: the server now shuts down cleanly when
  signalled by a process supervisor, flushing all buffered log events.
- Concurrency gate: `TOON_MAX_CONCURRENT_CALLS` (default: 8) limits
  simultaneous blocking pipeline dispatches. Callers receive a
  `server busy` error instead of blocking indefinitely when exceeded.
- `CompressDecision::PassedThrough` now carries an `Option<InputFormat>`,
  eliminating the redundant second `FormatDetector::detect` call in pass-
  through response paths.
- `event_id` is now emitted into tracing spans for cross-referencing log
  entries with `LogEvent` records.
- Structured readiness log: `status="ready"`, `component`, and `version`
  fields are emitted at startup, providing a stable anchor for monitoring.
- Writer task supervisor now logs a structured `error!` with
  `component="jsonl_sink_writer"` when the writer exits unexpectedly.
- Relative `TOON_LOG_DIR` paths emit a `tracing::warn!` at startup.
- File handles in `JsonlSink` are now held open between flushes, reducing
  `open(2)` / `close(2)` syscall overhead under sustained load.
- 5 MCP transport integration tests verifying tool routing, parameter
  deserialization, and wire protocol correctness.
- 15 additional unit tests for: `detect_and_parse` round-trips (JSONL, CSV,
  TSV), parser edge cases (empty input, unicode, single-row CSV, infinity
  fields), and `flush` acknowledgement in `JsonlSink`.
- `#![deny(missing_docs)]` added to `toon-mcp-core` and `toon-mcp-logging`.
- `TOON_MAX_CONCURRENT_CALLS` configuration variable.
- CI runner pinned to `ubuntu-24.04`.
- Coverage measurement via `cargo-llvm-cov` with a 70% line coverage gate.
- `CHANGELOG.md`, `SECURITY.md`, and GitHub issue templates.

---

## [0.1.0] - 2026-04-08

Initial release.

- Three MCP tools: `detect_format`, `compress_content`, `compression_stats`.
- Support for JSON, JSONL, CSV, and TSV input formats.
- Shape classification: Tabular, FoldChain, PrimitiveArray, Mixed, PassThrough.
- TOON encoding via `toon-format` with configurable delimiter and key folding.
- JSONL logging to hive-partitioned files, queryable with DuckDB.
- `MemorySink` for integration tests; `NoopSink` for benchmarks.
- Criterion benchmark suite with committed baselines.
- GitHub Actions CI: fmt, clippy, test, doc, release build.
