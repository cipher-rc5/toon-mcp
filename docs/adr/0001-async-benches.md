<!-- file: docs/adr/0001-async-benches.md -->
<!-- description: ADR allowing async benchmarks in toon-mcp-bench under stated constraints -->

# ADR 0001 — Async benchmarks in `toon-mcp-bench`

**Status:** accepted
**Date:** 2026-05-11
**Supersedes:** prior rule in CONTRIBUTING.md and docs/architecture.md (pre-2026-05-11) requiring all
benchmarks to be synchronous and `toon-mcp-bench` to depend only on
`toon-mcp-core`.

## Context

`toon-mcp-bench` originally measured only synchronous CPU work in
`toon-mcp-core` (detection, classification, compression). The
`toon-mcp-logging` `JsonlSink` is async by design — channel-fed writer
task, periodic flushes, day-rollover behaviour — and these
characteristics matter to operators. Measuring them requires a Tokio
runtime, which the prior workspace policy forbade.

Two options were considered:

1. **Move async benches to a separate sibling crate.** Cleanest in
   isolation but creates a second criterion-using crate with its own
   baseline tracking, doubling maintenance.
2. **Allow async benches in `toon-mcp-bench` under constraints.** Single
   bench crate, single baseline tracker. Adds a per-binary policy.

We chose option 2.

## Decision

`toon-mcp-bench` may contain async benchmarks under all of the following
constraints:

- **Dedicated bench binary.** Sync and async benches must live in
  separate `[[bench]]` binaries — never mixed in the same file.
- **Reason for async.** The benchmark must measure async-specific
  behaviour (channel throughput, flush latency, day-rollover overhead).
  Wrapping a sync API in `block_on(...)` is not justified by this ADR.
- **Runtime configuration.** Async benches should prefer
  `tokio::runtime::Builder::new_current_thread().enable_all().build()`
  over the multi-threaded default, to keep measurements deterministic.
- **Workspace dependency scope.** `toon-mcp-bench` may dev-depend on
  `toon-mcp-logging` for async benches. It still must not depend on
  `toon-mcp-server` (which would inject the rmcp protocol surface into
  benchmark measurements).

## Consequences

- Future contributors adding a new async bench should reference this
  ADR in their PR description.
- The bench-crate dependency graph row in
  `docs/architecture.md` lists `toon-mcp-logging` for async benches only.
- If a contributor needs `toon-mcp-server` in benches (e.g. to measure
  rmcp dispatch latency), open a new ADR — that decision is not in scope
  here.

## See also

- `docs/architecture.md` layer-rules section
- `crates/toon-mcp-bench/benches/jsonl_sink.rs` — the first async bench
  this ADR formalises
