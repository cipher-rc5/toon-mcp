# toon-mcp — Agent Rules

> This file is the authoritative source of truth for all LLM agents and human
> contributors working in this repository. All rules are mandatory. There are
> no suggested guidelines. When in doubt, refuse to proceed and ask for
> clarification rather than guessing.

---

## External Reference Corpus

Load references below using your fetch tool only when the immediate task
requires them. Do not preload all references on every turn.

| Reference | When to load |
|-----------|--------------|
| https://docs.rs/rmcp/latest/rmcp/ | Any work on server.rs, handler.rs, or MCP protocol |
| https://docs.rs/toon-format/latest/toon_format/ | Any work on detector.rs, classifier.rs, or compressor.rs |
| https://docs.rs/csv/latest/csv/ | Any work on parser/csv.rs |
| https://docs.rs/tokio/latest/tokio/ | Any async work, spawn_blocking, mpsc, oneshot |
| https://docs.rs/criterion/latest/criterion/ | Any work in toon-mcp-bench |
| https://doc.rust-lang.org/std/ | Standard library questions |
| https://www.conventionalcommits.org/en/v1.0.0/#specification | Before writing any commit message |

---

## Runtime and Toolchain

- Toolchain is pinned in rust-toolchain.toml. NEVER change the channel
  without explicit instruction.
- Formatter: rustfmt exclusively. Run `cargo fmt` before every commit.
- Linter: clippy exclusively. Run `cargo clippy -- -D warnings`.
  All warnings are errors.
- No build.rs files unless a C dependency requires bindgen.
- No Docker in local development.

### Strict Version Pinning

- All workspace.dependencies MUST specify an exact semver version string.
- Do NOT use wildcard (*) or open ranges (>=).
- Do NOT upgrade any dependency unless explicitly instructed.
- Commit Cargo.lock — this is a binary workspace.

---

## Code Style

- snake_case: all variables, functions, modules, file names.
- PascalCase: types, enums, traits, structs.
- SCREAMING_SNAKE_CASE: constants and statics.
- No emojis anywhere: source, comments, docs, commit messages.
- All public items MUST have doc comments (///). Run `cargo doc --no-deps`.
- No .unwrap() outside of tests and main(). Use ? for propagation.
- No .expect() with a generic string. The message MUST be a postcondition.
- No clone() to silence the borrow checker without an explanatory comment.
- No unsafe blocks without a safety comment explaining the invariant upheld.

### File Header

Every Rust source file MUST begin with this header:

    // file: crates/<crate>/src/<module>.rs
    // description: <one line summary>
    // reference: <upstream doc URL if applicable, else omit line>

---

## Rust Safety

- Use thiserror for all library crate errors.
- Never use Box<dyn Error> in public API return types.
- Never hold a std::sync::MutexGuard across an .await point.
- Never call blocking I/O on the tokio executor. Use spawn_blocking.
- File handles in JsonlSink MUST live exclusively on the background writer
  task. They MUST NOT be wrapped in Arc<Mutex<Handle>>.

---

## Architecture

Layer call direction (strict — no upward calls permitted):

    toon-mcp-server  ->  toon-mcp-core
    toon-mcp-server  ->  toon-mcp-logging
    toon-mcp-core    ->  (no workspace deps beyond toon-format, serde_json, csv)
    toon-mcp-logging ->  (no toon-mcp-core dep)
    toon-mcp-bench   ->  toon-mcp-core (sync benches);
                         additionally toon-mcp-logging for async benches only

- toon-mcp-core MUST remain pure: no I/O, no async, no rmcp dependency.
- toon-mcp-logging MUST NOT depend on toon-mcp-core.
- toon-mcp-bench MUST NOT be imported by toon-mcp-server.
- All parsers MUST normalise output to serde_json::Value before returning.
  The classifier and compressor MUST NOT be aware of the original format.
- All tool handlers live in toon-mcp-server/src/handler.rs.
- Config is loaded once in main.rs and passed by value or Arc.
- The LogSink trait is the only interface between handlers and logging.
- Claude Desktop requires absolute paths in all env config values that
  reference the file system. Document this clearly in README.md.

---

## Benchmarking

- All benchmark fixtures live in crates/toon-mcp-bench/fixtures/.
- Benchmark harness: Criterion exclusively.
- Sync core benchmarks (detection, classification, compression) MUST NOT
  start a tokio runtime. They measure synchronous core functions only.
- Async-specific benchmarks (e.g. logging sink throughput) MAY start a
  tokio runtime if all of the following hold: they live in their own
  dedicated bench binary, they use `current_thread` runtime where possible
  to keep measurements deterministic, and their purpose is to measure
  async-specific behaviour (channel throughput, flush latency) that cannot
  be measured synchronously.
- Baseline snapshots are committed to bench/baselines/. Do not delete them.
- Run benchmarks: `cargo bench --package toon-mcp-bench`

---

## Git and Commits

Format: `<type>[optional scope]: <description>`

Permitted types: feat, fix, docs, style, refactor, perf, test, build, ci,
chore, revert.

- Description: imperative mood, present tense, no trailing period.
- Body wraps at 72 characters.
- `cargo fmt && cargo clippy -- -D warnings && cargo test --workspace`
  MUST pass before every commit.
- Do NOT commit with --no-verify.

---

## What This Codebase Is Not

| Forbidden | Reason |
|-----------|--------|
| rayon | Tokio runtime conflict; spawn_blocking is sufficient |
| async-std or smol | Tokio is the sole async runtime |
| libsql | Wrong tool; dedicated writer task eliminates Send+Sync problem |
| Arc<Mutex<File>> in JsonlSink | Replaced by writer task + mpsc channel |
| Box<dyn Error> in public APIs | Untyped error surface |
| .unwrap() outside tests/main | Panics on production paths |
| anyhow in library crates | Leaks opaque errors to callers |
| Wildcard dependency versions | Reproducibility |
| Emojis anywhere | Consistency |
| Direct LLM API calls from server | Server is a tool provider only |
| System prompt injection per turn | Instructions registered once in ServerInfo |
| XML, YAML, TOML parsers (v1) | Out of scope; planned for future plugin iteration |
| toon-mcp-bench importing toon-mcp-server | Bench depends on toon-mcp-core (sync benches) or toon-mcp-logging (async benches only); never on toon-mcp-server |
