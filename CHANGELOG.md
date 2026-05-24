# Changelog

All notable changes to toon-mcp are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The `[Unreleased]` section accumulates changes that have not yet been tagged.
Each release header records the date (UTC) the tag was published.

---

## [Unreleased] — 0.2.0 (in progress)

### Added

- Fourth MCP tool `toon_diagnostics` that returns runtime health counters:
  logging drops/failures, writer-task failures, queue saturation gauges,
  pipeline timeout count, request-duration aggregates (count / total / max /
  average), and available concurrency permits. The tool exposes the same
  data that previously required reading the JSONL log files or attaching
  to the tracing stream.
- SIGTERM and SIGINT handlers: the server now shuts down cleanly when
  signalled by a process supervisor, flushing all buffered log events.
- Concurrency gate `TOON_MAX_CONCURRENT_CALLS` (default `8`): caps
  simultaneous blocking pipeline dispatches; callers receive a structured
  `server busy` error instead of blocking indefinitely when exceeded.
- Structured readiness log on startup: `status="ready"`, `component`, and
  `version` fields are emitted as a stable anchor for monitoring.
- Writer-task supervisor that logs a structured `error!` with
  `component="jsonl_sink_writer"` when the writer exits unexpectedly.
- Relative `TOON_LOG_DIR` paths emit a `tracing::warn!` at startup.
- `JsonlSink` now keeps file handles open between flushes, reducing
  `open(2)` / `close(2)` syscall overhead under sustained load.
- `event_id` is attached to tracing spans for cross-referencing log
  entries with `LogEvent` records.
- `CompressDecision::PassedThrough` now carries an `Option<InputFormat>`,
  eliminating the redundant second `FormatDetector::detect` call in pass-
  through response paths.
- 5 MCP transport integration tests verifying tool routing, parameter
  deserialization, and wire-protocol correctness.
- 15 additional unit tests for `detect_and_parse` round-trips (JSONL, CSV,
  TSV), parser edge cases (empty input, unicode, single-row CSV, infinity
  fields), and `flush` acknowledgement in `JsonlSink`.
- `#![deny(missing_docs)]` on `toon-mcp-core` and `toon-mcp-logging`.
- CI runner pinned to `ubuntu-24.04`.
- Per-PR fuzz-smoke step (`fuzz-smoke-pr`, 15 s per target) so parser
  regressions surface in the PR that introduces them; the extended weekly
  job (`fuzz-smoke-extended`, 30 s per target) still runs on schedule and
  workflow dispatch.
- Release pipeline produces a CycloneDX SBOM, SHA256 checksum file, and
  Sigstore keyless signatures (`.sigstore.json`) for every asset, plus
  GitHub artifact attestations.
- `CHANGELOG.md`, `SECURITY.md`, and GitHub issue templates.
- Line-coverage gate measured by `cargo-llvm-cov`. The floor was raised
  from 70 % to 75 % once non-trivial coverage was confirmed in CI.

### Changed

- **Toolchain bumped to Rust 1.95.0.** `rust-toolchain.toml`, `Cargo.toml`
  `rust-version`, and `.clippy.toml` (`msrv`) all pin 1.95.0. Downstream
  consumers must use Rust 1.95.0 or newer.
- **Renamed `ParquetSink` → `JsonlSink`** (and `ParquetSinkConfig` →
  `JsonlSinkConfig`). The sink writes JSONL, not Parquet; the previous
  name was misleading. Update any custom code that references the old
  types.
- `JsonlSink::flush_pending` now drains the buffered events into the
  spawn_blocking closure rather than cloning each `LogEvent`, reducing
  peak memory under high write pressure.
- Dependency bumps (all pinned exactly with `=`):
  `toon-format` `=0.4.6` → `=0.5.0`,
  `serde_json` `=1.0.149` → `=1.0.150`,
  `rmcp` `=1.6.0` → `=1.7.0`.

### Fixed

- `detect_format` handler now runs `FormatDetector::detect` on a
  `spawn_blocking` thread, preventing it from stalling the tokio executor
  under large inputs.
- `JsonlSink::flush()` now sends an acknowledged `Flush` command and
  blocks until the writer task confirms the data is on disk. Previously
  it was fire-and-forget and returned before any I/O completed.
- Server shutdown now calls the acknowledged `flush()` and drops the
  `Arc` to trigger clean writer-task exit. The previous `sleep(200ms)`
  race condition is eliminated.
- `spawn_blocking` `JoinError` is now reported to MCP clients as
  `internal_error` instead of `invalid_params`.
- Serialization failures in `flush_pending` emit a `tracing::error!`
  with the event ID instead of silently replacing the event with `{}`.
- Removed duplicate `toon-format` declaration from
  `toon-mcp-server/Cargo.toml`.
- Replaced `unsafe { std::env::set_var }` in config tests with
  the `temp-env` crate.
- Removed stale `AGENTS.md` references from `docs/LLM_RULES.md`,
  `docs/testing.md`, `docs/adr/0001-async-benches.md`, and
  `opencode.json`. The authoritative contributor rules live in
  `CONTRIBUTING.md`.

---

## [0.1.0] — 2026-04-08

Initial release.

### Added

- Three MCP tools: `detect_format`, `compress_content`, `compression_stats`.
- Support for JSON, JSONL, CSV, and TSV input formats.
- Shape classification: Tabular, FoldChain, PrimitiveArray, Mixed, PassThrough.
- TOON encoding via `toon-format` with configurable delimiter and key folding.
- JSONL logging to hive-partitioned files, queryable with DuckDB.
- `MemorySink` for integration tests; `NoopSink` for benchmarks.
- Criterion benchmark suite with committed baselines.
- GitHub Actions CI: fmt, clippy, test, doc, release build.

---

## Release Process

1. Move the `[Unreleased]` items under a new dated version header.
2. Update `version` in `Cargo.toml` (`[workspace.package]`) and refresh
   `Cargo.lock` with `cargo check --workspace --locked`.
3. Tag the release: `git tag -s vX.Y.Z -m "Release X.Y.Z"` and push the
   tag. The `Release` GitHub Actions workflow takes over from there
   (verify → cross-compile → SBOM → checksum → cosign sign → publish).
4. Verify the published release contains binaries for the four supported
   targets, the SBOM (`*-sbom.cdx.json`), `checksums.sha256`, and a
   matching `*.sigstore.json` bundle per asset.
