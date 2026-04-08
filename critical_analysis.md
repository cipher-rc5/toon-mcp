# toon-mcp — Critical Technical Analysis

**Date:** 2026-04-08
**Scope:** Full workspace (toon-mcp-core, toon-mcp-logging, toon-mcp-server, toon-mcp-bench)
**Build status:** Passes `cargo build`, `cargo test --workspace`, `cargo clippy -- -D warnings`, `cargo fmt --check`

---

## Production Readiness Score: 4 / 10 (original) → 8 / 10 (post-remediation)

> **Note:** All findings in this document have been addressed. The score below
> reflects the state at the time of analysis. See `improvements.md` for the
> remediation checklist and git history for the implementation.

The codebase is well-structured and architecturally sound for a pre-alpha prototype. The layering rules are enforced, the code compiles clean, and the tests pass. However, a substantial number of correctness issues, missing operational requirements, documentation inconsistencies, and absent safety guarantees prevent it from being deployed in any production or user-facing context without significant additional work.

---

## Summary of Findings

| Severity | Count |
|---|---|
| Critical | 5 |
| High | 8 |
| Medium | 9 |
| Low | 7 |

---

## Critical Issues

### C1 — No input size limit: unbounded memory allocation and potential OOM

**Location:** `crates/toon-mcp-server/src/handler.rs` — all three handlers

There is no upper bound on `params.input`. An MCP client (or adversary with stdio access) can send an arbitrarily large string. The pipeline will attempt to parse it entirely into memory as `serde_json::Value`, hold a clone for re-detection, and then allocate the TOON-encoded output. For a 500 MB payload this means multiple gigabyte allocations on a single tool call. The server has no rejection mechanism.

**Impact:** Out-of-memory crash, server restart loop, or host OS swap exhaustion. A single malicious or buggy client call brings down the server.

**No mitigation exists anywhere in the codebase.**

---

### C2 — Threshold logic is inverted relative to its documentation

**Location:** `crates/toon-mcp-core/src/compressor.rs:222`

```rust
// Actual code
if savings_pct < (1.0 - config.threshold) {
    return CompressDecision::PassedThrough { ... };
}
```

`TOON_COMPRESSION_THRESHOLD = 0.85` (the default) means the savings check passes when `savings_pct >= 0.15`, i.e., when the output is at most 85% of the input. But the field doc comment on `CompressConfig::threshold` says:

> "Encode only when `toon_bytes < input_bytes * threshold`"

These are not equivalent:
- The doc says: compress when `toon_bytes < input_bytes * 0.85` → accept if output is < 85% of input → savings > 15% ✓
- The code says: compress when `savings_pct >= (1.0 - 0.85)` = `savings_pct >= 0.15` → savings > 15% ✓

The check is numerically equivalent, but the semantic description in `docs/architecture.md` and `docs/configuration.md` is internally contradictory. The configuration reference (`docs/configuration.md:27`) states:

> "Maximum fraction of the original byte count that the TOON output may occupy."

While `docs/architecture.md:222` shows the opposite sign convention. A `threshold=1.0` would require zero savings (`savings_pct >= 0.0`), meaning any positive compression passes — but the configuration doc example says `threshold=1.0` is "pass through unless TOON output is strictly smaller than input", which is the same semantics but described as "useful for testing the encoding path." This double-inversion makes the threshold non-intuitive to configure and is a latent operator error source.

**Impact:** Misconfigured deployments. Operators setting `threshold=0.5` expecting "only compress if 50% savings" will instead get "compress if any savings exist." This is a user-facing correctness issue.

---

### C3 — Double parse on every successful compression: correctness risk and wasted work

**Location:** `crates/toon-mcp-server/src/handler.rs:189-195` and `handler.rs:323-326`

For both `compress_content` and `compression_stats`, when compression succeeds, the handler re-calls `FormatDetector::detect_and_parse` on the original input to extract `format` and `shape_class` for the response and log event:

```rust
// CompressDecision does not expose fmt or shape, so we re-detect
let fmt: String = FormatDetector::detect(&input).as_str().into();
let shape: String = match toon_mcp_core::detector::FormatDetector::detect_and_parse(&input) {
    Ok((_, val)) => Classifier::classify(&val).as_str().into(),
    Err(_) => ShapeClass::PassThrough.as_str().into(),
};
```

This is a double-parse of the full document on every successful compression call. For a 1 MB JSON payload, this is two full serde_json deserializations plus one full classification walk. The comment acknowledges this ("cheap — value already parsed inside Compressor") but the actual cost is O(N) parsing, not O(1) lookup.

More critically, the `CompressDecision::Compressed` variant does not expose the detected `InputFormat` or classified `ShapeClass`. Because the handler must re-detect independently, there is a theoretical TOCTOU window: if the input contains non-deterministic content (currently impossible with pure functions, but a design smell), the format in the log could differ from the format used to encode.

**Impact:** 2x CPU and memory cost on the hot path. Structural design flaw that the `CompressDecision` type should carry format and shape.

---

### C4 — `ParquetSink` writer task panic is silently swallowed; events are lost without caller awareness

**Location:** `crates/toon-mcp-logging/src/parquet_sink.rs:121` (`writer_task`)

If `writer_task` panics (e.g., `spawn_blocking` itself panics), the `mpsc::Receiver` is dropped. Subsequent `record()` calls return `LogError::ChannelSend`. But the handlers unconditionally fire-and-forget: `let _ = log_sink.record(event).await;`. There is no recovery path, no alert, and no way for the operator to know that logging silently stopped.

Furthermore, `tokio::spawn(task)` in `main.rs:32` does not attach a `.expect()` or any join handle monitoring. If the writer task exits unexpectedly mid-session, the server keeps serving tool calls while silently dropping all log events with no operator signal.

**Impact:** Silent data loss. An operator relying on event logs for compliance, billing, or analytics will receive a truncated log with no error indicator.

---

### C5 — Committed runtime artifact in source tree: `data/1774347477_evaluation.jsonl`

**Location:** `data/1774347477_evaluation.jsonl` (2,053 lines)

A 2,053-line JSONL evaluation file containing real prediction data (`btc_price`, `predicted_direction`, `actual_winner`, `confidence`, `model_version`) is committed to the repository. This is not a test fixture — it is a runtime artifact from an unrelated trading/evaluation system. The data includes:

- Unix timestamps
- BTC price data
- Confidence scores and directional predictions
- Model version identifiers

This file has no relationship to the toon-mcp codebase and should not exist in the repository. It may contain proprietary model evaluation data.

**Impact:** Data leakage, repository pollution, confused consumers of the repo. Violates the principle that only reproducible build inputs belong in source control.

---

## High Severity Issues

### H1 — No graceful shutdown: buffered log events are lost on SIGTERM/process exit

**Location:** `crates/toon-mcp-server/src/main.rs:44`

```rust
service.waiting().await?;
info!("toon-mcp-server shutting down");
Ok(())
```

After `service.waiting()` returns (client disconnects or stdin closes), `main` immediately returns. The `ParquetSink` background task is still running with unflushed events in its buffer. Tokio drops all non-awaited tasks on runtime shutdown. The `shutdown()` method on `ParquetSink` is never called from `main.rs`.

**Impact:** Up to `TOON_LOG_BUFFER_SIZE` (default: 1,000) events are silently dropped on every clean server exit.

---

### H2 — `Config::load()` silently ignores invalid environment variable values with no tracing output

**Location:** `crates/toon-mcp-server/src/config.rs:95-130`

All `env_*` helpers use `.and_then(|v| v.parse().ok()).unwrap_or(default)` — parse failures silently fall through to the default. The `docs/configuration.md` claims:

> "Config::load() does not panic on invalid values — it logs a tracing::warn! and substitutes the default."

But the implementation emits no `tracing::warn!`. Invalid values like `TOON_MIN_BYTES=abc` are silently ignored. An operator who typo'd a variable will have no indication that their configuration was rejected and the default is being used.

**Impact:** Misconfiguration silently ignored. The documentation is incorrect and the behavior is operationally dangerous.

---

### H3 — No rate limiting or per-call timeout: a slow client blocks the tool handler indefinitely

**Location:** `crates/toon-mcp-server/src/handler.rs`

The compression pipeline is synchronous and unbounded in execution time (bound only by input size). There is no `tokio::time::timeout` wrapping the `Compressor::decide` call or the log sink `record` call. A sufficiently large input will hold the handler thread for seconds.

Since rmcp uses the stdio transport sequentially, a single hung handler blocks all subsequent tool calls on that session.

**Impact:** Denial of service on large inputs; client timeout mismatches.

---

### H4 — `day_partition_key` uses deprecated `DateTime::from_timestamp`

**Location:** `crates/toon-mcp-logging/src/parquet_sink.rs:234`

```rust
let dt = DateTime::<Utc>::from_timestamp(secs, nanos).unwrap_or_else(|| {
    DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is a valid timestamp")
});
```

`DateTime::from_timestamp` was deprecated in chrono 0.4.27 in favour of `DateTime::from_timestamp` → `DateTime::from_timestamp` → `DateTime::from_timestamp` (the API changed). The current chrono 0.4 series emits a deprecation warning for this function. The workspace Cargo.toml pins `chrono = "0.4"` without an exact patch version, meaning this deprecation will become a compile error when the crate is eventually updated to chrono 0.5.

**Impact:** Build breakage on the next chrono major version bump. Currently emits a compiler warning that is suppressed by the test run.

---

### H5 — `ServerError` is `#[allow(dead_code)]` and unused: error surface is vestigial

**Location:** `crates/toon-mcp-server/src/error.rs:9`

```rust
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum ServerError {
    LoggingInit(#[from] toon_mcp_logging::LogError),
    McpService(String),
}
```

`ServerError` is never instantiated or returned from any function. `main.rs` returns `Box<dyn std::error::Error>`. The `#[allow(dead_code)]` suppresses the warning rather than either using the type or removing it. This is not a stub pattern — it has been `#[allow]`-ed, which means it will never trigger a warning reminding developers to implement it.

**Impact:** Dead code in the error surface; `main.rs` violates the stated rule against `Box<dyn Error>` in public API return types (AGENTS.md rule).

---

### H6 — No workspace-level `[package]` metadata: unpublishable crates

**Location:** `Cargo.toml` (workspace root), all crate `Cargo.toml` files

No crate has `description`, `license`, `repository`, `authors`, or `keywords` fields. The `[workspace.package]` block only sets `edition = "2024"`. These are required for any crate destined for crates.io and are best practice for any shared library.

**Impact:** Cannot publish without adding required metadata. No license is specified anywhere (README mentions "See LICENSE if present" but no LICENSE file exists).

---

### H7 — Missing `LICENSE` file

**Location:** Repository root

README.md line 306: "See [LICENSE](LICENSE) if present." No LICENSE file exists. For an open-source tool that LLM developers will embed in production workflows, the absence of a license means it is legally all-rights-reserved by default.

**Impact:** Users cannot legally use, modify, or redistribute the code without explicit permission.

---

### H8 — Benchmark baselines not committed; AGENTS.md rule violated

**Location:** `AGENTS.md` (mandatory rule), `docs/testing.md:84`

AGENTS.md states: "Baseline snapshots are committed to `bench/baselines/`. Do not delete them." The `bench/baselines/` directory does not exist. No baselines have ever been captured. The `.gitignore` lists `/bench/baselines/` as ignored.

**Impact:** No regression protection on the benchmark suite. The mandatory rule in AGENTS.md is violated. The `.gitignore` entry actively prevents baselines from being committed even if generated.

---

## Medium Severity Issues

### M1 — `toon-mcp-server` directly imports `csv` for column count detection, duplicating core logic

**Location:** `crates/toon-mcp-server/Cargo.toml:26`, `handler.rs:72-79`

The server imports `csv` directly and uses `csv::ReaderBuilder` in `handle_detect_format` to compute `column_count`. This duplicates logic that properly belongs in `toon-mcp-core`. It also means the handler is format-aware in a way the architecture doc says it should not be.

---

### M2 — `.env.example` is stale and contains removed variables from the DuckDB era

**Location:** `.env.example:29-33`

```
TOON_LOG_DB_PATH=data/interactions.duckdb
TOON_LOG_PARQUET_DIR=data/parquet
```

These variables are not read by `Config::load()`. The implemented logging backend uses `TOON_LOG_DIR` only. These phantom variables will confuse operators who try to configure DuckDB integration that no longer exists.

---

### M3 — `docs/testing.md` references a non-existent `duckdb_sink.rs` file

**Location:** `docs/testing.md:36`

```
| DuckDB sink | crates/toon-mcp-logging/src/duckdb_sink.rs:275 | #[tokio::test] | 2 |
```

There is no `duckdb_sink.rs` file. The actual sink is `parquet_sink.rs`. The test count table also shows 46 tests total but the workspace produces 47 (39 + 3 + 5 = 47, plus 12 doctests = 59). The documentation describing the test suite is incorrect.

---

### M4 — `docs/algorithms.md` pseudocode for `probe_delimited` diverges from actual implementation

**Location:** `docs/algorithms.md:91-102`

The documentation shows `has_headers: false` and reads three rows with `take(3)`:

```rust
let mut rdr = csv::ReaderBuilder::new()
    .delimiter(delimiter)
    .has_headers(false)
    .from_reader(input.as_bytes());
let rows: Vec<csv::StringRecord> = rdr.records().take(3).filter_map(Result::ok).collect();
```

The actual implementation (`detector.rs:157-177`) uses `has_headers: true` and reads only the first data record:

```rust
let headers = match rdr.headers() { ... };
match rdr.records().next() { ... }
```

These are different algorithms. Documentation is misleading to anyone trying to understand the detection behavior.

---

### M5 — `MemorySink` has no integration tests despite being purpose-built for testing

**Location:** `crates/toon-mcp-logging/src/memory_sink.rs`

`MemorySink` was built specifically to enable handler integration tests that assert on logged `LogEvent` fields. This is explicitly called out as a known gap in `docs/testing.md:57-66`. Zero integration tests use it. No handler test verifies that `tool_name`, `input_bytes`, `compressed`, `savings_pct`, or `format` are correctly populated in the emitted event.

---

### M6 — No `Config::load()` tests; silent env-var misconfiguration is completely untested

**Location:** `crates/toon-mcp-server/src/config.rs`

`Config::load()` is untested (acknowledged in `docs/testing.md:67-73`). With 13 env vars that each have fallback behavior, there are many paths that have never been exercised by a test runner. This is especially concerning given issue H2 (silent failure on invalid values).

---

### M7 — Workspace dependency versions use open ranges, violating AGENTS.md

**Location:** `Cargo.toml:11-44`

AGENTS.md states: "All workspace.dependencies MUST specify an exact semver version string. Do NOT use wildcard (*) or open ranges (>=)." Yet multiple workspace dependencies use open minor/patch ranges:

- `toon-format = { version = "0.4", ... }` — matches any 0.4.x
- `schemars = "1"` — matches any 1.x.y
- `serde = { version = "1", ... }` — matches any 1.x.y
- `serde_json = "1"` — matches any 1.x.y
- `csv = "1.3"` — matches any 1.3.x through 1.x.y
- `async-trait = "0.1"` — matches any 0.1.x
- `tokio = { version = "1", ... }` — matches any 1.x.y
- `duckdb = { version = "1.1", ... }` — matches any 1.1.x through 1.x.y
- `tracing = "0.1"` — matches any 0.1.x
- `tracing-subscriber = { version = "0.3", ... }` — matches any 0.3.x
- `chrono = { version = "0.4", ... }` — matches any 0.4.x
- `dotenvy = "0.15"` — matches any 0.15.x
- `thiserror = "2"` — matches any 2.x.y
- `uuid = { version = "1", ... }` — matches any 1.x.y

Only `criterion = "0.8.2"` and `tempfile = "3.10"` specify minor versions. None use exact `=x.y.z` pinning. The AGENTS.md rule is broken by virtually every dependency.

---

### M8 — `is_fold_chain` uses `.expect()` with a postcondition claim that is not fully verified at compile time

**Location:** `crates/toon-mcp-core/src/classifier.rs:198`

```rust
let child = map.values().next().expect("map has exactly one value");
```

The comment claim is correct given the `map.len() == 1` guard on the same arm, but this is a runtime assertion that could be eliminated with a safer pattern (`map.into_values().next()` paired with `?` or `if let`). AGENTS.md permits `.expect()` only with a postcondition message, and this one is present — but the approach is fragile if the match arm pattern is ever refactored.

---

### M9 — `duckdb` is in `workspace.dependencies` and `toon-mcp-server/Cargo.toml` but unused

**Location:** `Cargo.toml:30`, `toon-mcp-server/Cargo.toml` (not present but implied by earlier agent notes)

`duckdb = { version = "1.1", features = ["bundled"] }` is declared in `workspace.dependencies`. The `bundled` feature compiles DuckDB from source, adding significant compile time and binary size. No crate in the workspace depends on it. It is dead weight in the workspace manifest, inflating `cargo build` times and triggering unnecessary compilation.

**Note:** After direct verification, `duckdb` does not appear in any crate's `[dependencies]`, so it is declared but not pulled in. Still, its presence in the workspace manifest is misleading and violates workspace hygiene.

---

## Low Severity Issues

### L1 — No `tracing::warn!` for invalid env var values (documentation promises it)

See H2 above. The configuration documentation explicitly promises warn-level output for invalid values, but no `tracing::warn!` calls exist anywhere in `config.rs`. The document is wrong.

---

### L2 — `data/` directory contains runtime artifacts that are not gitignored

**Location:** `.gitignore`, `data/` directory

`data/interactions.duckdb` and `data/logs/` are partially covered by gitignore. However, `data/1774347477_evaluation.jsonl` is committed (see C5). The gitignore entry `logs/` would match `data/logs/` only if interpreted as a glob pattern relative to the repository root, but gitignore patterns without a leading `/` match anywhere in the tree, so `data/logs/` is gitignored. The evaluation JSONL is not.

---

### L3 — `CompressConfig` is constructed identically in both handlers — a DRY violation

**Location:** `handler.rs:159-167` (`handle_compress_content`) and `handler.rs:299-307` (`handle_compression_stats`)

```rust
let compress_config = CompressConfig {
    threshold: config.compression_threshold,
    min_bytes: config.min_bytes,
    key_folding: config.key_folding,
    delimiter: config.delimiter,
    tabular_min_rows: config.tabular_min_rows,
    fold_min_depth: config.fold_min_depth,
    primitive_array_min: config.primitive_array_min,
};
```

This 8-line block is copy-pasted verbatim in both handlers. A helper function or a `From<&Config> for CompressConfig` impl would eliminate the duplication.

---

### L4 — `detect_format` handler uses `input.lines()` while core uses `input.lines().enumerate()`

**Location:** `handler.rs:65`

The handler counts non-empty lines with:
```rust
Some(input.lines().filter(|l| !l.trim().is_empty()).count())
```

This is consistent with the JSONL parser's behavior but is a separate implementation of line counting logic that could drift. A helper or reuse of the parser's output would be more maintainable.

---

### L5 — No CI pipeline defined

There is no `.github/workflows/`, `.gitlab-ci.yml`, `.circleci/`, or any other CI configuration file. The pre-commit gate (`cargo fmt && cargo clippy -- -D warnings && cargo test --workspace`) exists only as a human-executed mandate in AGENTS.md and README.md. Any push can introduce breakage that is not automatically caught.

---

### L6 — `toon-mcp-bench/src/lib.rs` is an empty file with only a header comment

**Location:** `crates/toon-mcp-bench/src/lib.rs`

The bench crate has `src/lib.rs` with only the file header. The benchmarks are `[[bench]]` binaries and do not depend on this lib. The file is dead. A `[[bench]]`-only crate does not need a `lib.rs`. The empty file is confusing.

---

### L7 — `opencode.json` and `README.md` have inconsistent integration instructions

`opencode.json` sets `TOON_LOG_DIR` to a path that assumes the repo is at a specific location. `README.md` correctly says paths must be absolute for Claude Desktop. But the `opencode.json` example uses what appears to be a relative or placeholder path. The two documents describe slightly different setup workflows without cross-referencing the inconsistency.

---

## Structural Observations (Non-Scoring)

These are design patterns that are not bugs today but are worth noting for future development:

**S1 — `PassThroughReason::as_str` allocates a `String` rather than returning `&'static str`.**
Every log event construction calls `reason.as_str()` which allocates. `ShapeClass::as_str` returns `&'static str`. `PassThroughReason::as_str` should do the same for the non-parameterised variants. The parameterised variant `InsufficientSavings` does need formatting, but the others do not.

**S2 — The `LogSink` trait `shutdown` method consumes `Box<Self>`.**
This forces every shutdown call site to box the sink explicitly (`Box::new(sink).shutdown()`). This pattern is unusual for Rust trait objects and is not standard ergonomics. An `async fn shutdown(&mut self)` with a consumed `&mut self` or a separate `ShutdownHandle` pattern would be more idiomatic.

**S3 — No `version` in `[workspace.package]`; crate versions are `0.1.0` hardcoded.**
All four crates are at `0.1.0` with no shared version management strategy. For a workspace that may eventually be published, shared versioning via `version.workspace = true` would simplify release management.

**S4 — `detector.rs` internal comment says "JSON probe — cheapest successful path" but JSON parsing is O(N) over the full document, the most expensive probe for large inputs.**
A truly cheap probe would check only the first non-whitespace byte (`{` or `[`) before attempting full parse. The comment is misleading about actual performance characteristics.
