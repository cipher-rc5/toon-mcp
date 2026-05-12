# toon-mcp — Developer Overview

**toon-mcp** is a locally-hosted [Model Context Protocol (MCP)](https://spec.modelcontextprotocol.io/) server written in Rust. Its primary job is to act as a **token-reduction middleware** between LLM clients (such as Claude Desktop or opencode) and the context window. Rather than pasting raw structured data directly into a prompt, clients call toon-mcp's tools, which return a more compact representation that the model can read natively.

---

## What It Does

toon-mcp exposes three tools to any MCP-compatible client:

| Tool                | Purpose                                                                                               |
| ------------------- | ----------------------------------------------------------------------------------------------------- |
| `detect_format`     | Identify whether input is JSON, JSONL, CSV, or TSV and return shape metadata                          |
| `compression_stats` | Preview estimated savings without encoding — useful for deciding whether to compress                  |
| `compress_content`  | Run the full compression pipeline and return the result (or the original if savings are insufficient) |

All three tools are called over stdio JSON-RPC. The server is a single compiled binary, usable from any client that speaks MCP.

---

## Compression Pipeline

Input passes through five sequential gates. If it fails any gate, the original is returned unchanged — there is no lossy fallback.

1. **Minimum size check** — very small payloads are skipped immediately; the overhead is not worth it.
2. **Format detection and parsing** — the input is probed and parsed into a normalised internal representation. Supported formats: JSON, JSONL, CSV, TSV. Unknown formats pass through.
3. **Shape classification** — the parsed structure is analysed to determine whether its shape is one that compresses efficiently. Highly regular tabular data, deeply nested chains, and flat scalar arrays all have known-good compression profiles. Irregular or mixed shapes are attempted but subject to the threshold gate.
4. **TOON encoding** — the normalised structure is encoded using the TOON (Token-Oriented Object Notation) format, which is specifically designed to reduce token count in LLM context windows while remaining human-readable. Encoding is performed by the `toon-format` library.
5. **Savings threshold gate** — if the encoded output is not meaningfully smaller than the original (default: must save at least 15%), the original is returned. Compression is never forced.

The critical design invariant: **the classifier and encoder never see the original format** — only the normalised representation. This keeps each stage independently testable and makes it straightforward to add new input formats in the future.

---

## Why It Works

Token reduction comes from a combination of two structural observations:

- **Repeated keys dominate JSON token cost.** In an array of objects with the same schema, every row repeats every key name. TOON eliminates this repetition at the encoding level.
- **Structural nesting adds token overhead.** Deeply nested single-key objects carry significant syntactic overhead (braces, quotes, colons) relative to their data content. TOON collapses these chains.

The server only emits compressed output when the savings are real and measurable. When compression is skipped, the tool returns the original string along with a machine-readable reason (`pass_reason`), so clients can log or react accordingly.

---

## Architecture

The workspace has four crates with a strict, enforced dependency direction:

```
toon-mcp-server  (binary, MCP wiring, tool handlers)
     |
     +-- toon-mcp-core    (pure logic: detect, classify, compress — no I/O, no async)
     +-- toon-mcp-logging (structured event logging behind a trait interface)

toon-mcp-bench   (Criterion benchmarks; sync benches depend on toon-mcp-core,
                  async benches additionally on toon-mcp-logging)
```

Key architectural decisions:

- **`toon-mcp-core` has no I/O and no async.** All compression logic is synchronous and pure, making it trivially benchmarkable and testable without a runtime.
- **Logging is behind a trait.** Handlers receive a `LogSink` and never import the logging implementation directly. Tests use an in-memory sink; production uses a file-backed sink that writes hive-partitioned JSONL queryable by DuckDB.
- **The DuckDB connection (if used) lives exclusively on a background task.** There is no `Arc<Mutex<Connection>>` — an mpsc channel carries log events to a single writer, eliminating lock contention entirely.
- **All tool handler logic lives in `handler.rs`.** The server module is a thin rmcp dispatch wrapper.

---

## Configuration

All behaviour is controlled via environment variables prefixed `TOON_*`. Relevant knobs include compression threshold, minimum input size, shape classification thresholds, delimiter style, and logging settings. There are no hardcoded magic numbers in the core pipeline.

---

## Observability

Every tool invocation produces a `LogEvent` with 13 fields: event ID, timestamp, tool name, detected format, shape class, input/output byte counts, savings percentage, threshold used, pass reason, duration in microseconds, and an optional client hint tag. These are written as JSONL to partitioned daily files that can be queried with DuckDB at any time without taking a lock on the running server.

---

## Running Benchmarks

```sh
cargo bench --package toon-mcp-bench
```

Benchmarks cover format detection, shape classification, and the full compression pipeline across six fixture types. They run without a tokio runtime and report throughput in bytes/sec.

---

_Built with Rust 1.93.0. Tokio async runtime. rmcp for MCP protocol. toon-format for TOON encoding._
