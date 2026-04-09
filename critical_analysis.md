# toon-mcp — Critical Production Readiness Analysis

**Score: 5.5 / 10**

**Reviewer context:** Full static analysis of all Rust source files, CI
configuration, documentation, and test suite. `cargo check`, `cargo clippy
-- -D warnings`, `cargo test --workspace`, and `cargo build --release` were
all run and passed cleanly. Issues below are architectural, operational, and
safety concerns — not compilation errors.

---

## Executive Summary

The codebase is a disciplined, well-structured Rust workspace at the prototype
stage. Architectural separation of concerns is correct: a pure core library,
a decoupled logging layer, and a thin server binary. The code style is
consistent, doc coverage is solid, and the test suite passes with zero
failures. However, several categories of concern prevent production deployment:
the primary logging backend is a JSONL file writer instead of the planned
DuckDB-backed sink, the `detect_format` handler exposes an unprotected
synchronous code path on the async executor, the shutdown sequence is
unreliable, there are no integration or end-to-end tests of the MCP transport
layer, and observability is minimal. The score reflects a well-engineered
prototype that has not yet crossed the gap to a deployable, operable service.

---

## Category Scores

| Category | Score | Weight |
|---|---|---|
| Architecture and layering | 8 / 10 | High |
| Error handling and correctness | 6 / 10 | High |
| Operational safety (shutdown, signals) | 4 / 10 | High |
| Test coverage and quality | 5 / 10 | High |
| Observability and debuggability | 4 / 10 | Medium |
| Security and input validation | 6 / 10 | Medium |
| Performance and concurrency | 6 / 10 | Medium |
| CI/CD and release pipeline | 5 / 10 | Medium |
| Documentation | 8 / 10 | Low |
| Dependency management | 8 / 10 | Low |

---

## Critical Issues (Blockers)

### C1 — `detect_format` runs blocking I/O on the tokio executor

**File:** `crates/toon-mcp-server/src/handler.rs:91-93`

`handle_compress_content` and `handle_compression_stats` correctly wrap
`Compressor::decide` in `tokio::task::spawn_blocking`. `handle_detect_format`
does not. `FormatDetector::detect` calls `serde_json::from_str` (full document
parse), CSV reader allocation, and multiple string scans directly on the
async executor thread. For large inputs this will starve the tokio runtime
of its worker threads. The AGENTS.md rule "Never call blocking I/O on the
tokio executor" is violated.

```rust
// handler.rs:92 — runs synchronously on the executor, no spawn_blocking
let fmt = FormatDetector::detect(input);
```

**Impact:** Under concurrent load, a single large detect call can block all
other inflight requests. Production: high severity.

---

### C2 — Shutdown sequence does not guarantee log flush

**File:** `crates/toon-mcp-server/src/main.rs:66-79`

The shutdown path calls `sink.flush()` (which sends `SinkCmd::Flush` over the
mpsc channel) then `drop(sink)` then `tokio::time::sleep(200ms)`. This is
fragile for three reasons:

1. `flush()` is non-consuming — it sends a Flush command but does not wait for
   the writer task to acknowledge completion. The actual I/O may not have
   finished when the program continues.
2. The 200 ms sleep is an arbitrary race condition. If the writer task is slow
   (e.g., filesystem pressure), events are silently lost.
3. `Box::new(sink).shutdown()` exists on the `LogSink` trait and returns an
   ack channel confirming the flush completed, but it is never called. The
   correct shutdown is to call `shutdown()` on the boxed sink, which blocks
   until the writer task drains and acknowledges.

**Impact:** Log events from the last flush interval before exit are silently
dropped every time the server shuts down.

---

### C3 — No SIGTERM / SIGINT handler; stdio closure is the only shutdown path

**File:** `crates/toon-mcp-server/src/main.rs`

The server only terminates cleanly when the MCP client closes the stdin pipe.
There is no `tokio::signal::ctrl_c()` or `tokio::signal::unix::signal(SIGTERM)`
handler. When deployed as a system service (systemd, launchd, Docker) a
SIGTERM is sent before SIGKILL. Without handling it, the process is killed
without flushing the log sink, and the writer task is aborted mid-write,
potentially corrupting the JSONL partition file.

**Impact:** Production deployments via any process supervisor will lose log
events and may write partial JSONL lines on every restart.

---

### C4 — `ParquetSink::flush()` does not confirm write completion

**File:** `crates/toon-mcp-logging/src/parquet_sink.rs:103-108`

`flush()` sends `SinkCmd::Flush` over the channel and returns immediately. It
does not wait for the writer task to complete the disk write. This means callers
(including the shutdown path in `main.rs`) get a successful `Ok(())` before any
data has actually reached disk.

```rust
async fn flush(&self) -> Result<(), LogError> {
    self.sender
        .send(SinkCmd::Flush)  // fire-and-forget — no ack
        .await
        .map_err(|e| LogError::ChannelSend(e.to_string()))
}
```

Compare with `shutdown()`, which correctly uses a `oneshot::Sender` for
acknowledgement. `flush()` should do the same.

**Impact:** `flush()` is semantically broken as an acknowledged operation.
The main.rs flush timeout will always succeed immediately, not after data
is durable.

---

### C5 — DuckDB sink was planned but never implemented; AGENTS.md still references it

**File:** `AGENTS.md:103`, `docs/architecture.md:180`, `docs/initial_plan.md`

The `AGENTS.md` External Reference Corpus lists `duckdb` under "Any work on
`duckdb_sink.rs`", and `architecture.md` describes the `DuckDbSink` as a
future replacement for the JSONL writer. The planned structured storage with
queryable Parquet output was scoped to the initial plan but was never
implemented. The production logging layer is the `ParquetSink`, which despite
its name writes JSONL files, not Parquet. The sink name is misleading and the
AGENTS.md references a non-existent file, which will cause confusion for future
contributors.

**Impact:** The name "ParquetSink" is wrong (it writes JSONL). Documentation
references a dead code path. Future contributors will waste time looking for
`duckdb_sink.rs`.

---

## High Severity Issues

### H1 — Silent event corruption on serialization failure in flush_pending

**File:** `crates/toon-mcp-logging/src/parquet_sink.rs:202`

When `serde_json::to_string(event)` fails during a flush, the event is silently
replaced with `"{}"`. This means a corrupted `LogEvent` writes a valid but
empty JSON object to disk, making the failure invisible in both the logs and
any downstream DuckDB query.

```rust
.map(|e| serde_json::to_string(e).unwrap_or_else(|_| "{}".into()))
```

`LogEvent` only contains primitive types and `Option<String>` fields; in
practice serialization cannot fail. But the silent replacement is a correctness
hazard: when it does fail (e.g., a future field with a non-serializable type),
data is lost without any observable signal.

**Impact:** Silent data corruption in the audit log; undetectable in queries.

---

### H2 — `spawn_blocking` errors are mapped to `invalid_params` instead of an internal error

**File:** `crates/toon-mcp-server/src/handler.rs:209, 365`

`JoinError` from a panicking `spawn_blocking` task is reported back to the MCP
client as `invalid_params`. This is the wrong MCP error code — `invalid_params`
implies the client sent bad input, not that the server had an internal failure.
An internal server error should use a different error code (e.g.
`McpError::internal_error`). Clients that branch on error codes will
misdiagnose server panics as user errors.

---

### H3 — `compression_stats` re-runs `FormatDetector::detect` after already running the full pipeline

**File:** `crates/toon-mcp-server/src/handler.rs:370`

After calling `Compressor::decide` (which internally calls
`FormatDetector::detect_and_parse`), `handle_compression_stats` calls
`FormatDetector::detect` again to populate the `fmt` field for pass-through
cases. This is a redundant O(N) full-document scan. The same issue exists in
`handle_compress_content` at line 242 for `PassedThrough` branches. The format
information should be carried in `PassedThrough` variants of `CompressDecision`,
as it is already present in `Compressed`.

---

### H4 — `toon-format` is declared twice in `toon-mcp-server/Cargo.toml`

**File:** `crates/toon-mcp-server/Cargo.toml:31` and `Cargo.toml:22`

`toon-format` is declared both in the workspace `[workspace.dependencies]`
table and directly in the server's `[dependencies]` with an explicit version
string, bypassing the workspace key. Any future version bump to `toon-format`
must be made in two places or the server will silently resolve a different
version than the rest of the workspace.

---

### H5 — `unsafe` env var mutation in config tests is data-race prone

**File:** `crates/toon-mcp-server/src/config.rs:237-243`

Tests use `unsafe { std::env::set_var(...) }` protected by a `Mutex`. This
relies on all tests that touch env vars being in the same test binary and
all using `ENV_LOCK`. This is fragile: adding a test in a different module that
calls `std::env::var` without acquiring the lock introduces a silent data race.
In Rust 2024 edition, `std::env::set_var` is deprecated and `unsafe` is
required. The correct approach is to use a test helper crate (e.g.,
`serial_test`, `temp-env`) or restructure `Config::load()` to accept an
environment abstraction injectable for tests.

---

### H6 — No integration test of the MCP transport layer

**File:** `crates/toon-mcp-server/src/server.rs`

The `server.rs` tool routing layer (`#[tool_router]`, `#[tool_handler]`) has
zero test coverage, as acknowledged in `docs/testing.md`. The only way to
verify that tool names are dispatched to the correct handler, that JSON
parameter deserialization works end-to-end, and that the rmcp framing is
correct is via a stdio round-trip test. No such test exists. A bug in the
macro expansion or the `Parameters<T>` wrapper would be invisible until the
server is deployed.

**Impact:** The most critical integration point in the entire server — the
MCP wire protocol — is completely untested.

---

## Medium Severity Issues

### M1 — No rate limiting at the MCP handler level

The server accepts arbitrarily many concurrent tool calls. A malicious or
misbehaving client could submit many large-input calls simultaneously, consuming
all `spawn_blocking` threads and memory. There is a per-call byte limit
(`TOON_MAX_INPUT_BYTES`) but no concurrency limit or queue depth cap at the
handler level.

---

### M2 — Log directory defaults to a relative path

**File:** `crates/toon-mcp-server/src/config.rs:86`

`TOON_LOG_DIR` defaults to `"data/logs"` (relative). The README warns that
Claude Desktop requires absolute paths. A user who forgets to set an absolute
path in Claude Desktop will silently write logs relative to the process working
directory (which may be `/` or a temp dir in a managed environment), and the
log files will be unrecoverable. The server should detect relative paths at
startup and emit a `tracing::warn!` with a clear message when running under
environments known to have unpredictable working directories.

---

### M3 — Writer task supervisor does not restart the writer; it only logs

**File:** `crates/toon-mcp-server/src/main.rs:38-45`

The supervisor task spawned after `ParquetSink::new` logs an error if the
writer task exits unexpectedly, but takes no action. After an unexpected writer
exit, all subsequent `sink.record()` calls will fail with
`LogError::ChannelSend` (channel closed), and those errors are silently dropped
by `let _ = log_sink.record(event).await`. The system continues running but
logging is permanently dead, with no operator-visible indication.

---

### M4 — No health check or readiness signal

There is no mechanism for a process supervisor or monitoring system to
determine whether the server is healthy and accepting requests. MCP servers
communicate over stdio, but there is no liveness endpoint or startup signal
beyond the MCP `initialize` handshake. A deployment that checks health by
polling a port would have no answer.

---

### M5 — `async_trait` macro is a compatibility shim, not idiomatic Rust 1.75+

**File:** `crates/toon-mcp-logging/src/sink.rs:29`

Rust 1.75 stabilized `async fn` in traits natively. The toolchain is pinned to
1.87.0. Using `async_trait` (which erases the future via `Box<dyn Future>` and
adds a heap allocation per call) is unnecessary and introduces overhead on
every `record()`, `flush()`, and `shutdown()` call. This should be replaced
with native async trait syntax.

---

### M6 — No structured log correlation between tracing events and LogEvents

`tracing` spans and `LogEvent` records are completely separate. A
`tracing::error!` and the `LogEvent` for the same request share no correlation
ID. Debugging a failing request requires correlating stderr tracing output
(which has no stable request ID) against `LogEvent.event_id` (UUIDv4, which is
not emitted to tracing). A `tracing::info!` event at the start of each tool
call that emits the `event_id` would make incident investigation tractable.

---

### M7 — CI runs on `ubuntu-latest` with no pinned version

**File:** `.github/workflows/ci.yml:17`

`runs-on: ubuntu-latest` resolves to whatever GitHub's current LTS is. A
runner image update could change the libc version, linker, or system libraries.
Combined with a pinned Rust toolchain this is low risk but impure. A CI
failure caused by a runner image change is indistinguishable from a code
regression without pinned runner images.

---

### M8 — No code coverage enforcement in CI

The CI pipeline runs `cargo test` but has no coverage gate. It is unknown what
percentage of lines are covered. The known testing gaps (transport layer,
parser edge cases, unicode inputs, empty inputs) could silently regress.

---

## Low Severity Issues

### L1 — `clone()` before `spawn_blocking` allocates a full input copy per call

**File:** `crates/toon-mcp-server/src/handler.rs:194, 350`

`input_clone = input.clone()` copies the entire input string to move it into
the `spawn_blocking` closure. For a 10 MiB input this is a 10 MiB allocation
per call. Since `input` is already owned (moved from `params.input`), the
original could be moved directly into the closure, eliminating the copy.

---

### L2 — `handler.rs` result destructuring tuple is verbose and error-prone

**File:** `crates/toon-mcp-server/src/handler.rs:216-253`

The 7-element tuple returned from the `match &decision` block is difficult to
maintain. Adding a field to `CompressDecision::Compressed` requires updating
the tuple signature at the match site, the type annotation, and the downstream
usages. A named intermediate struct would be clearer and less fragile.

---

### L3 — JSONL file opened with `append` mode on every flush, not held open

**File:** `crates/toon-mcp-logging/src/parquet_sink.rs:212-216`

Each flush opens the JSONL file, appends, and closes it. For high-throughput
scenarios (many events per flush interval) this is a syscall-per-flush
overhead. The writer task should hold the file handle open between flushes and
only re-open on day boundary rolls, which would reduce syscall overhead
significantly.

---

### L4 — `ParquetSinkConfig` name is misleading; the sink writes JSONL

**File:** `crates/toon-mcp-logging/src/parquet_sink.rs:35`

The struct is named `ParquetSinkConfig` and the file `parquet_sink.rs`, but
the implementation writes JSONL, not Parquet. This was clearly intended as a
transitional name (pending the DuckDB migration), but the name will mislead
any contributor or operator who reads the config struct name and expects actual
Parquet output.

---

### L5 — Missing TSV detection test for `FormatDetector::detect_and_parse`

**File:** `crates/toon-mcp-core/src/detector.rs`

`detect_and_parse` has a test for JSON and a test for `Unknown` errors, but
no tests for `Jsonl`, `Csv`, or `Tsv` paths through `detect_and_parse`. The
`detect` tests cover format recognition but not the combined detect-and-parse
code path for these formats.

---

### L6 — `bench/baselines/` is committed but `.gitignore` excludes Criterion output

**File:** `.gitignore:13`

`.gitignore` excludes `criterion/` (Criterion's HTML report output) but the
`bench/baselines/` path is explicitly tracked. This is intentional per the
comment but the Criterion baseline format is internal to Criterion and not
designed to be stable across versions. A Criterion upgrade could invalidate
committed baselines silently.

---

### L7 — No `#[deny(missing_docs)]` crate-level attribute

The AGENTS.md rules require all public items to have doc comments. This rule
is enforced in CI only via `cargo doc --no-deps -- -D warnings` for missing
docs on public items, but a crate-level `#![deny(missing_docs)]` attribute
would catch violations at compile time locally before CI runs.

---

### L8 — `opencode.json` is not documented in README

**File:** `opencode.json`, `README.md:89-95`

The README mentions opencode integration but does not document the
`opencode.json` schema or how to modify it. A contributor who wants to change
server arguments or add env vars will not know where to look.

---

## Summary Table

| ID | Severity | Location | Description |
|---|---|---|---|
| C1 | Critical | `handler.rs:92` | `detect_format` blocks executor thread, no `spawn_blocking` |
| C2 | Critical | `main.rs:66-79` | Shutdown does not await flush acknowledgement |
| C3 | Critical | `main.rs` | No SIGTERM/SIGINT handler; process supervisor kills without flush |
| C4 | Critical | `parquet_sink.rs:103` | `flush()` is fire-and-forget; returns before data hits disk |
| C5 | Critical | `AGENTS.md`, `parquet_sink.rs` | DuckDB sink planned but never built; `ParquetSink` name is wrong |
| H1 | High | `parquet_sink.rs:202` | Silent `{}` replacement on serialization failure corrupts audit log |
| H2 | High | `handler.rs:209,365` | `JoinError` from server panic reported as `invalid_params` to client |
| H3 | High | `handler.rs:242,370` | Redundant second `detect()` call on every pass-through response |
| H4 | High | `server/Cargo.toml:31` | `toon-format` duplicated outside workspace table |
| H5 | High | `config.rs:237` | `unsafe` env mutation in tests; fragile, Rust 2024 deprecated |
| H6 | High | `server.rs` | MCP transport/routing layer has zero test coverage |
| M1 | Medium | `handler.rs` | No concurrency cap; executor starvation under load |
| M2 | Medium | `config.rs:86` | Relative `TOON_LOG_DIR` default silently misdirects logs |
| M3 | Medium | `main.rs:38` | Writer task supervisor logs failure but does not restart |
| M4 | Medium | `main.rs` | No health check or readiness signal |
| M5 | Medium | `sink.rs` | `async_trait` crate unnecessary on Rust 1.75+; heap allocation overhead |
| M6 | Medium | `handler.rs` | No request correlation between tracing spans and `LogEvent.event_id` |
| M7 | Medium | `ci.yml:17` | Unpinned `ubuntu-latest` runner |
| M8 | Medium | `ci.yml` | No code coverage gate or measurement in CI |
| L1 | Low | `handler.rs:194,350` | Unnecessary full input clone before `spawn_blocking` |
| L2 | Low | `handler.rs:216` | 7-element tuple destructuring is fragile |
| L3 | Low | `parquet_sink.rs:212` | File re-opened on every flush instead of held open |
| L4 | Low | `parquet_sink.rs` | `ParquetSink`/`ParquetSinkConfig` names contradict JSONL behaviour |
| L5 | Low | `detector.rs` | Missing `detect_and_parse` tests for JSONL, CSV, TSV paths |
| L6 | Low | `bench/baselines/` | Committed Criterion baselines not guaranteed stable across versions |
| L7 | Low | all crates | No `#![deny(missing_docs)]` crate attribute |
| L8 | Low | `README.md` | `opencode.json` schema undocumented |
