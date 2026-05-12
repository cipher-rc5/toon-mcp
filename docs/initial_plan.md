# toon-mcp — Build Plan

> **Status: historical planning artefact.** This document captures the
> original build plan for toon-mcp and is preserved as a record of intent.
> It has diverged from the implementation in several places (notably the
> Parquet sink was replaced with a JSONL sink in `toon-mcp-logging`, and
> the bench-crate dependency policy now permits async benches that
> additionally depend on `toon-mcp-logging`). For the authoritative
> current state, see:
>
> - [AGENTS.md](../AGENTS.md) — agent / contributor rules and layer policy
> - [docs/architecture.md](architecture.md) — crate dependency graph and data flow
> - [docs/configuration.md](configuration.md) — environment-variable reference
> - [docs/runbook.md](runbook.md) — production operator runbook
>
> Token-Oriented Object Notation compression layer for opencode and Claude
> Desktop, implemented as a locally hosted MCP server in Rust. Dynamically
> detects structured payloads across multiple input formats (JSON, JSONL, CSV,
> TSV) that benefit from TOON encoding, compresses them, and logs every
> interaction to a DuckDB-backed Parquet store for offline analysis. An
> independent benchmarking crate provides reproducible performance measurement
> decoupled from the server runtime.

---

## Table of Contents

1. [Goals and Non-Goals](#1-goals-and-non-goals)
2. [Architecture Overview](#2-architecture-overview)
3. [Workspace Layout](#3-workspace-layout)
4. [Crate Specifications](#4-crate-specifications)
   - 4.1 [toon-mcp-core](#41-toon-mcp-core)
   - 4.2 [toon-mcp-logging](#42-toon-mcp-logging)
   - 4.3 [toon-mcp-server](#43-toon-mcp-server)
   - 4.4 [toon-mcp-bench](#44-toon-mcp-bench)
5. [Data Flow](#5-data-flow)
6. [Format Detection Pipeline](#6-format-detection-pipeline)
7. [Schema Definitions](#7-schema-definitions)
8. [Configuration Reference](#8-configuration-reference)
9. [Client Integration](#9-client-integration)
   - 9.1 [opencode](#91-opencode)
   - 9.2 [Claude Desktop](#92-claude-desktop)
10. [Dependency Manifest](#10-dependency-manifest)
11. [AGENTS.md](#11-agentsmd)
12. [opencode.json](#12-opencodejson)
13. [Build, Test, and Release](#13-build-test-and-release)
14. [Implementation Order](#14-implementation-order)

---

## 1. Goals and Non-Goals

### Goals

- Expose three MCP tools via rmcp stdio transport, compatible with both
  opencode and Claude Desktop:
  `compress_content`, `compression_stats`, and `detect_format`.
- Dynamically detect and classify incoming payloads across multiple structured
  formats: JSON, JSONL (newline-delimited JSON), CSV, and TSV. Each format
  has a dedicated parser and normalises to `serde_json::Value` before
  classification.
- Apply AST-style structural shape classification on the normalised value tree
  and decide whether TOON encoding yields a net token reduction above a
  configurable threshold.
- Log every tool invocation — including format detected, shape class, byte
  deltas, compression decision, and duration — to a DuckDB instance that
  periodically exports hive-partitioned Parquet files.
- Provide an independent `toon-mcp-bench` crate with Criterion-based
  benchmarks covering the full format detection and compression pipeline,
  runnable entirely outside the server process.
- Keep the logging layer behind a trait so it is swappable (DuckDB sink,
  no-op sink, in-memory sink for tests).
- Remain self-contained: single compiled binary, no system DuckDB install
  required (`bundled` feature), no TLS, no remote calls.
- Never require a system prompt injection on each message — the server
  instructions are registered once in `ServerInfo::instructions` at session
  handshake.

### Non-Goals

- Direct LLM API calls. The server is a tool provider only; opencode and
  Claude Desktop own the LLM connection.
- Compression of prose, source code files, or arbitrary binary payloads. The
  classifier passes unrecognised content through unchanged.
- Multi-user or networked deployment. This is a single-user local process.
- rayon parallelism. Classification and encoding are fast synchronous
  operations; `tokio::task::spawn_blocking` handles any unexpectedly large
  payload without polluting the async executor.
- XML, YAML, TOML, or binary format support in v1. These may be added as
  format detector plugins in a future iteration.

---

## 2. Architecture Overview

```
opencode / Claude Desktop (LLM clients)
    |
    | stdio JSON-RPC (MCP protocol via rmcp)
    |
toon-mcp-server  (binary)
    |
    +-- tool: detect_format(input: String) -> DetectResult
    |       |
    |       +-- toon-mcp-core::FormatDetector  (sniff + parse pipeline)
    |
    +-- tool: compress_content(input: String) -> CompressResult
    |       |
    |       +-- toon-mcp-core::FormatDetector  (sniff + normalise to Value)
    |       +-- toon-mcp-core::Classifier       (shape walk)
    |       +-- toon-mcp-core::Compressor       (threshold gate + encode)
    |       +-- toon-mcp-logging::LogSink       (fire-and-forget Sender<LogEvent>)
    |
    +-- tool: compression_stats(input: String) -> StatsResult
            |
            +-- toon-mcp-core::FormatDetector  (dry run)
            +-- toon-mcp-core::Classifier
            +-- toon-mcp-logging::LogSink

toon-mcp-logging (background task, separate tokio task)
    |
    +-- mpsc::Receiver<LogEvent>
    +-- duckdb::Connection  (exclusively owned, never shared)
    +-- Appender -> tool_log table
    +-- periodic COPY TO Parquet (hive-partitioned by day)

toon-mcp-bench (independent binary, never imported by server)
    |
    +-- Criterion benchmark groups
    +-- format_detection benchmarks
    +-- classification benchmarks
    +-- compression pipeline benchmarks
    +-- reads fixture files from bench/fixtures/
```

The `Connection` never crosses a thread boundary. Tool handlers hold only a
`Sender<LogEvent>`, which is `Send + Clone`. No `Mutex<Connection>` is needed.

Both opencode and Claude Desktop connect via the same stdio transport. The
binary is identical; only the host-side config file differs.

---

## 3. Workspace Layout

```
toon-mcp/
  Cargo.toml                          workspace manifest
  rust-toolchain.toml                 pinned toolchain
  rustfmt.toml                        formatter config
  .clippy.toml                        clippy overrides
  AGENTS.md                           LLM agent rules (see section 11)
  opencode.json                       opencode config (see section 12)
  README.md
  .env.example                        documented env vars with defaults

  crates/
    toon-mcp-core/
      Cargo.toml
      src/
        lib.rs
        detector.rs                   InputFormat enum + multi-format sniff pipeline
        parser/
          mod.rs                      Parser trait
          json.rs                     serde_json pass-through parser
          jsonl.rs                    JSONL -> Vec<Value> -> Array(Value) normaliser
          csv.rs                      CSV/TSV -> tabular Value normaliser (csv crate)
        classifier.rs                 ShapeClass enum + serde_json::Value walker
        compressor.rs                 threshold gate + encode decision
        error.rs                      CoreError (thiserror)

    toon-mcp-logging/
      Cargo.toml
      src/
        lib.rs
        event.rs                      LogEvent struct
        sink.rs                       LogSink trait
        duckdb_sink.rs                DuckDB writer task + Appender + Parquet export
        noop_sink.rs                  no-op implementation for tests
        memory_sink.rs                in-memory Vec sink for integration tests
        error.rs                      LogError (thiserror)

    toon-mcp-server/
      Cargo.toml
      src/
        main.rs                       tokio entry point, wires all crates
        server.rs                     rmcp ServerHandler impl + tool_router
        handler.rs                    tool handler functions
        config.rs                     Config struct loaded from env via dotenvy
        error.rs                      ServerError (thiserror)

    toon-mcp-bench/
      Cargo.toml
      benches/
        detection.rs                  format sniff benchmarks
        classification.rs             shape classifier benchmarks
        compression.rs                full pipeline benchmarks
      fixtures/
        small_json.json               ~1 KB JSON object
        large_tabular.json            ~50 KB array of uniform objects
        large_jsonl.jsonl             ~50 KB JSONL
        large_csv.csv                 ~50 KB CSV
        deep_fold.json                deeply nested single-key object
        mixed_array.json              non-uniform array

  data/
    .gitkeep                          DuckDB files and Parquet output land here
```

---

## 4. Crate Specifications

### 4.1 toon-mcp-core

**Purpose:** Pure format detection, classification, and compression logic.
No I/O, no async, no rmcp dependency. Fully unit-testable in isolation.
This crate is also the sole dependency of `toon-mcp-bench`.

---

#### detector.rs — Format Detection

```
InputFormat enum
  Json      -- valid JSON (object or array root)
  Jsonl     -- two or more newline-separated JSON values, each valid
  Csv       -- comma-delimited, two or more columns, optional header row
  Tsv       -- tab-delimited, two or more columns, optional header row
  Unknown   -- none of the above; produces PassThrough downstream
```

Detection is ordered by specificity and cost:

```
1. JSON probe  (cheapest: attempt serde_json::from_str on full input)
   -> success: InputFormat::Json

2. JSONL probe (split on '\n', attempt parse on first two non-empty lines)
   -> both succeed AND line count >= 2: InputFormat::Jsonl

3. CSV probe   (count comma-delimited columns on first two lines;
                >= 2 cols and equal column counts): InputFormat::Csv

4. TSV probe   (same logic with tab delimiter): InputFormat::Tsv

5. Fallthrough -> InputFormat::Unknown
```

Detection MUST NOT read from disk. It operates on the `&str` input only.

`FormatDetector::detect(input: &str) -> InputFormat` is the public entry point.

`FormatDetector::detect_and_parse(input: &str) -> Result<(InputFormat, Value), CoreError>`
calls `detect` then dispatches to the appropriate parser, returning a
normalised `serde_json::Value` ready for the classifier.

---

#### parser/mod.rs — Parser Trait

```rust
pub trait Parser: Send + Sync {
    fn parse(&self, input: &str) -> Result<serde_json::Value, CoreError>;
}
```

Each format has a dedicated implementation. Parsers are stateless unit
structs. The trait is kept simple — no streaming, no async — because all
inputs arrive as an owned `String` from the MCP tool call.

---

#### parser/json.rs

Thin wrapper around `serde_json::from_str`. Returns the `Value` as-is.
No transformation required.

---

#### parser/jsonl.rs

```
Algorithm:
  1. Split input on '\n', filter empty lines.
  2. Parse each line as serde_json::Value.
  3. If any line fails: CoreError::ParseFailed { format: Jsonl, line }.
  4. Collect into Vec<Value>.
  5. Wrap in Value::Array(vec).
```

JSONL files commonly represent streams of uniform objects — exactly the
Tabular shape TOON compresses best. Array-wrapping normalisation preserves
this signal for the classifier.

---

#### parser/csv.rs

Uses the `csv` crate. CSV and TSV share this parser; the delimiter is a
parameter.

```
Algorithm:
  1. Build csv::ReaderBuilder with delimiter = b',' or b'\t'.
  2. Read headers from first record.
  3. For each subsequent record: build serde_json::Map<String, Value>
     where keys are header names and values are the string fields.
     Attempt numeric coercion: if the field parses as f64, emit
     Value::Number; otherwise emit Value::String.
  4. Collect into Vec<Value::Object(map)>.
  5. Wrap in Value::Array(vec).
```

CSV/TSV always normalises to an array of uniform objects — the canonical
Tabular shape. Numeric coercion improves TOON savings because numbers are
emitted without quotes.

---

#### classifier.rs

```
ShapeClass enum
  Tabular         -- array of N uniform objects, all primitive values
  FoldChain       -- object where every level has exactly one key, depth >= 3
  PrimitiveArray  -- flat array of scalar values (no objects)
  Mixed           -- array with non-uniform objects or nested arrays
  PassThrough     -- root is scalar, array too short, or Unknown format
```

The classifier takes `&serde_json::Value` and returns `ShapeClass`. It does
not allocate a new string. It walks the existing parsed tree.

Classification rules (evaluated in order):

```
Tabular:
  value is Array
  AND len >= TABULAR_MIN_ROWS (default 3)
  AND all elements are Object
  AND all Objects share identical key sets
  AND all Object values are primitives (no nested Object or Array)

FoldChain:
  value is Object
  AND has exactly one key
  AND that key's value is Object
  AND chain depth >= FOLD_MIN_DEPTH (default 3)
  (recursive: walk until chain breaks or leaf is reached)

PrimitiveArray:
  value is Array
  AND len >= PRIMITIVE_ARRAY_MIN (default 5)
  AND all elements are primitives

Mixed:
  value is Array
  AND does not meet Tabular or PrimitiveArray criteria
  -> attempt encode; let threshold gate decide

PassThrough:
  InputFormat::Unknown
  OR root is a primitive
  OR Array len < minimum thresholds
  OR Object with multiple keys and no nested chain
```

---

#### compressor.rs

```
CompressDecision enum
  Compressed {
    toon:           String,
    original_bytes: usize,
    toon_bytes:     usize,
    savings_pct:    f64,
  }
  PassedThrough { reason: PassThroughReason }

PassThroughReason enum
  UnknownFormat
  BelowMinBytes
  InsufficientSavings { estimated_pct: f64, threshold: f64 }
  ShapeNotBeneficial
  ParseFailed { format: InputFormat, detail: String }
```

The compressor pipeline:

```
1. Check raw byte length against TOON_MIN_BYTES.
   -> below threshold: PassedThrough(BelowMinBytes)

2. FormatDetector::detect_and_parse(input)
   -> InputFormat::Unknown: PassedThrough(UnknownFormat)
   -> parse error:          PassedThrough(ParseFailed)

3. Classifier::classify(&value)
   -> ShapeClass::PassThrough: PassedThrough(ShapeNotBeneficial)

4. toon_format::encode(&value, &encode_opts)
   where encode_opts reflects TOON_KEY_FOLDING and TOON_DELIMITER config

5. Compute savings_pct = 1.0 - (toon_bytes as f64 / original_bytes as f64)
   -> savings_pct < TOON_COMPRESSION_THRESHOLD:
      PassedThrough(InsufficientSavings)

6. Return Compressed { toon, original_bytes, toon_bytes, savings_pct }
```

---

### 4.2 toon-mcp-logging

**Purpose:** Structured interaction logging behind a trait. The DuckDB sink
runs on a dedicated tokio task and is the only place `duckdb::Connection`
lives.

#### event.rs

```rust
pub struct LogEvent {
    pub event_id:        String,         // UUIDv4
    pub ts_us:           i64,            // unix timestamp microseconds
    pub tool_name:       String,         // "compress_content" | "compression_stats" | "detect_format"
    pub input_format:    String,         // InputFormat::to_str()
    pub shape_class:     String,         // ShapeClass::to_str()
    pub input_bytes:     u64,
    pub output_bytes:    u64,
    pub compressed:      bool,
    pub savings_pct:     f64,            // 0.0 if not compressed
    pub threshold_used:  f64,
    pub duration_us:     u64,            // wall time: detect + classify + encode
    pub pass_reason:     Option<String>, // set when compressed = false
    pub client_hint:     Option<String>, // "opencode" | "claude-desktop" | None
}
```

`client_hint` is populated from the `TOON_CLIENT_HINT` env var, allowing
log queries to split metrics by client.

#### sink.rs

```rust
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    async fn record(&self, event: LogEvent) -> Result<(), LogError>;
    async fn flush(&self) -> Result<(), LogError>;
    async fn shutdown(self: Box<Self>) -> Result<(), LogError>;
}
```

#### duckdb_sink.rs

Construction returns `(DuckDbSink, impl Future<Output = ()>)`. The caller
spawns the future as a background task. The sink itself holds only a
`mpsc::Sender<DuckDbCmd>`.

```
DuckDbCmd enum
  Record(LogEvent)
  Flush
  Shutdown(oneshot::Sender<Result<(), LogError>>)
```

Background task behaviour:

```
1. Connection::open(config.db_path)
2. CREATE TABLE IF NOT EXISTS tool_log (...) on startup
3. conn.appender("tool_log") -> Appender
4. On Record:  appender.append_row(event fields in column order)
5. On Flush OR every TOON_LOG_FLUSH_INTERVAL_SECS OR TOON_LOG_BUFFER_SIZE rows:
   a. appender.flush()
   b. COPY tool_log TO '{parquet_dir}'
      (FORMAT PARQUET, PARTITION_BY (day), OVERWRITE_OR_IGNORE)
6. On Shutdown: flush, export, send ack on oneshot, exit loop
```

---

### 4.3 toon-mcp-server

**Purpose:** rmcp server binary. Wires core, logging, and config together.
Exposes three tools. Identical binary for opencode and Claude Desktop.

#### server.rs — ServerHandler

```rust
#[derive(Clone)]
pub struct ToonMcpServer {
    compressor:  Arc<Compressor>,
    log_sink:    Arc<dyn LogSink>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ToonMcpServer {
    #[tool(description = "Detect the format of a structured input string. \
                          Returns the detected format (json, jsonl, csv, tsv, \
                          or unknown) and basic statistics.")]
    async fn detect_format(&self, params: Parameters<DetectParams>)
        -> Result<Json<DetectResult>, McpError> { ... }

    #[tool(description = "Compress structured content (JSON, JSONL, CSV, TSV) \
                          to TOON format for token efficiency. Returns the \
                          compressed TOON string when savings exceed the \
                          configured threshold, or the original input unchanged. \
                          TOON is human-readable — interpret it directly.")]
    async fn compress_content(&self, params: Parameters<CompressParams>)
        -> Result<Json<CompressResult>, McpError> { ... }

    #[tool(description = "Preview compression statistics without encoding. \
                          Returns format detection, shape classification, and \
                          estimated token savings. Use before compress_content \
                          to decide whether compression is worthwhile.")]
    async fn compression_stats(&self, params: Parameters<StatsParams>)
        -> Result<Json<StatsResult>, McpError> { ... }
}

#[tool_handler]
impl ServerHandler for ToonMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Compresses structured data (JSON, JSONL, CSV, TSV) to TOON \
                 format to reduce token consumption in context windows. \
                 Workflow: call detect_format to identify input type, \
                 compression_stats to preview savings, then compress_content \
                 to encode. Pass-through is automatic when savings are \
                 insufficient. TOON output is human-readable — no decoding \
                 step is required before use.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
```

#### handler.rs — Tool input/output types

```rust
// detect_format
pub struct DetectParams { pub input: String }

pub struct DetectResult {
    pub format:        String,
    pub input_bytes:   usize,
    pub line_count:    Option<usize>,
    pub column_count:  Option<usize>,
}

// compress_content
pub struct CompressParams { pub input: String }

pub struct CompressResult {
    pub output:        String,
    pub compressed:    bool,
    pub format:        String,
    pub shape_class:   String,
    pub input_bytes:   usize,
    pub output_bytes:  usize,
    pub savings_pct:   f64,
    pub duration_us:   u64,
    pub pass_reason:   Option<String>,
}

// compression_stats (dry run — no encoding performed)
pub struct StatsParams { pub input: String }

pub struct StatsResult {
    pub would_compress:         bool,
    pub format:                 String,
    pub shape_class:            String,
    pub input_bytes:            usize,
    pub estimated_output_bytes: usize,
    pub estimated_savings_pct:  f64,
    pub threshold:              f64,
    pub pass_reason:            Option<String>,
}
```

All three handlers record a `LogEvent` via `log_sink.record(event).await`.
The channel is bounded; backpressure is the intended flow-control mechanism.

#### main.rs

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let config = Config::load();
    init_tracing(&config.log_level);

    let sink: Arc<dyn LogSink> = if config.logging.enabled {
        let (sink, task) = DuckDbSink::new(&config.logging).await?;
        tokio::spawn(task);
        Arc::new(sink)
    } else {
        Arc::new(NoopSink)
    };

    let server = ToonMcpServer::new(config, sink);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
```

---

### 4.4 toon-mcp-bench

**Purpose:** Independent Criterion benchmark suite covering the full format
detection and compression pipeline. This crate is NEVER imported by the
server. It depends only on `toon-mcp-core`. It is excluded from default
workspace build targets and run explicitly via `cargo bench`.

The benchmarking crate is pluggable in two senses:

1. **Fixture-pluggable:** Fixtures in `bench/fixtures/` can be replaced or
   extended without changing benchmark code. Each benchmark group reads
   fixtures by path constant defined at the top of the bench file.

2. **Config-pluggable:** Benchmark runs read env vars to parameterise
   threshold, delimiter, and key-folding settings, allowing configuration
   comparison without recompilation.

#### Cargo.toml (toon-mcp-bench)

```toml
[package]
name    = "toon-mcp-bench"
version = "0.1.0"
edition.workspace = true

[[bench]]
name    = "detection"
harness = false

[[bench]]
name    = "classification"
harness = false

[[bench]]
name    = "compression"
harness = false

[dependencies]
toon-mcp-core = { path = "../toon-mcp-core" }

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
```

#### benches/detection.rs

Benchmark groups:

```
detect_json_small       -- ~1 KB JSON object
detect_json_large       -- ~50 KB JSON array
detect_jsonl_large      -- ~50 KB JSONL
detect_csv_large        -- ~50 KB CSV
detect_tsv_large        -- ~50 KB TSV
detect_unknown          -- ~1 KB prose string (fast rejection path)
```

Each group measures `FormatDetector::detect(input)` in isolation.
No parsing, no classification.

#### benches/classification.rs

Benchmark groups:

```
classify_tabular        -- large_tabular.json fixture
classify_fold_chain     -- deep_fold.json fixture
classify_primitive_arr  -- array of 1000 numbers
classify_mixed          -- mixed_array.json fixture
classify_pass_through   -- scalar root (fastest path)
```

Each group measures `Classifier::classify(&value)` on a pre-parsed
`serde_json::Value`. The parse step is excluded from timing via
pre-computation in benchmark setup, ensuring the measurement covers
only classification logic.

#### benches/compression.rs

Full pipeline benchmarks (detect + parse + classify + encode):

```
pipeline_json_tabular           -- large_tabular.json
pipeline_jsonl_uniform          -- large_jsonl.jsonl
pipeline_csv_numeric            -- large_csv.csv (numeric coercion path)
pipeline_json_fold_chain        -- deep_fold.json
pipeline_pass_through_unknown   -- prose input (full rejection path timing)
pipeline_below_min_bytes        -- 100-byte JSON (byte-check short circuit)
```

Each group reports: throughput (bytes/sec), latency (ns/iter), and
savings_pct as a recorded custom measurement value.

#### Benchmark output

```bash
cargo bench --package toon-mcp-bench
# HTML reports: target/criterion/
```

Baseline results are committed to `bench/baselines/` as Criterion JSON
snapshots. Compare against saved baseline:

```bash
cargo bench --package toon-mcp-bench -- --load-baseline my-baseline
```

---

## 5. Data Flow

```
Client (opencode or Claude Desktop) calls compress_content("...input...")
    |
    v
handler.rs: record start timestamp
    |
    v
Compressor::decide(input: &str, config: &CompressConfig)
    |
    +-- byte len < TOON_MIN_BYTES?
    |       -> PassedThrough(BelowMinBytes)   skip all parsing
    |
    +-- FormatDetector::detect_and_parse(input)
    |       -> InputFormat::Unknown           -> PassedThrough(UnknownFormat)
    |       -> parse error                    -> PassedThrough(ParseFailed)
    |       -> (InputFormat, Value)
    |
    +-- Classifier::classify(&value)
    |       -> ShapeClass::PassThrough        -> PassedThrough(ShapeNotBeneficial)
    |
    +-- toon_format::encode(&value, &opts)
    |
    +-- savings_pct < threshold?
    |       -> PassedThrough(InsufficientSavings)
    |
    +-- Compressed { toon, original_bytes, toon_bytes, savings_pct }
    |
    v
handler.rs: stop timer, build CompressResult
    |
    v
log_sink.record(LogEvent { ... })   -- non-blocking send into mpsc channel
    |
    v
return CompressResult to client

[background]
DuckDbSink task drains mpsc channel
    -> Appender rows into tool_log
    -> flush trigger: row count OR elapsed time
    -> COPY TO Parquet (hive-partitioned by day)
```

---

## 6. Format Detection Pipeline

```
Input string
    |
    v
[1] JSON probe  -- serde_json::from_str (full input)
    |  success -> InputFormat::Json -> JsonParser
    |
    v
[2] JSONL probe -- split on '\n', parse first two non-empty lines
    |  both succeed AND line count >= 2 -> InputFormat::Jsonl -> JsonlParser
    |
    v
[3] CSV probe   -- csv::ReaderBuilder (delimiter=b',')
    |  first two rows have >= 2 equal-length fields -> InputFormat::Csv -> CsvParser(b',')
    |
    v
[4] TSV probe   -- csv::ReaderBuilder (delimiter=b'\t')
    |  first two rows have >= 2 equal-length fields -> InputFormat::Tsv -> CsvParser(b'\t')
    |
    v
[5] InputFormat::Unknown -> no parser, PassThrough downstream

Normalised output from all parsers: serde_json::Value::Array(...)
    |
    v
Classifier::classify(&value) -> ShapeClass
    |
    v
Compressor threshold gate -> CompressDecision
```

Key invariant: every parser produces a `serde_json::Value`. The classifier
and compressor have no awareness of the original format — they operate
exclusively on the normalised value tree. This keeps classification and
compression logic format-agnostic and independently testable.

---

## 7. Schema Definitions

### DuckDB table: tool_log

```sql
CREATE TABLE IF NOT EXISTS tool_log (
    event_id        VARCHAR      NOT NULL,
    ts_us           BIGINT       NOT NULL,
    tool_name       VARCHAR      NOT NULL,
    input_format    VARCHAR      NOT NULL,
    shape_class     VARCHAR      NOT NULL,
    input_bytes     UBIGINT      NOT NULL,
    output_bytes    UBIGINT      NOT NULL,
    compressed      BOOLEAN      NOT NULL,
    savings_pct     DOUBLE       NOT NULL,
    threshold_used  DOUBLE       NOT NULL,
    duration_us     UBIGINT      NOT NULL,
    pass_reason     VARCHAR,
    client_hint     VARCHAR
);
```

### Parquet export path (hive-partitioned)

```
data/parquet/
  day=2026-04-05/
    data_0.parquet
  day=2026-04-06/
    data_0.parquet
```

### Example queries

Per-format compression effectiveness:

```sql
SELECT
    input_format,
    shape_class,
    COUNT(*)                                      AS calls,
    SUM(compressed::INT)                          AS compressed_count,
    AVG(savings_pct) FILTER (WHERE compressed)    AS avg_savings_pct,
    SUM(input_bytes - output_bytes)               AS total_bytes_saved
FROM read_parquet('data/parquet/**/*.parquet', hive_partitioning = true)
GROUP BY input_format, shape_class
ORDER BY total_bytes_saved DESC;
```

Client split:

```sql
SELECT
    client_hint,
    COUNT(*)                        AS calls,
    AVG(savings_pct)                AS avg_savings,
    SUM(input_bytes - output_bytes) AS bytes_saved
FROM tool_log
GROUP BY client_hint;
```

Pass-through reason distribution:

```sql
SELECT
    pass_reason,
    COUNT(*) AS occurrences
FROM tool_log
WHERE compressed = false
GROUP BY pass_reason
ORDER BY occurrences DESC;
```

---

## 8. Configuration Reference

All values loaded from environment via `dotenvy`. Defaults shown.

| Variable                       | Default                        | Description                                          |
|--------------------------------|--------------------------------|------------------------------------------------------|
| `TOON_COMPRESSION_THRESHOLD`   | `0.85`                         | Encode only if toon_len < input_len * threshold      |
| `TOON_MIN_BYTES`               | `256`                          | Skip classification below this byte count            |
| `TOON_KEY_FOLDING`             | `true`                         | Enable TOON key folding for FoldChain shapes         |
| `TOON_DELIMITER`               | `comma`                        | comma, tab, or pipe                                  |
| `TOON_TABULAR_MIN_ROWS`        | `3`                            | Minimum array length for Tabular classification      |
| `TOON_FOLD_MIN_DEPTH`          | `3`                            | Minimum chain depth for FoldChain                    |
| `TOON_PRIMITIVE_ARRAY_MIN`     | `5`                            | Minimum length for PrimitiveArray                    |
| `TOON_LOG_ENABLED`             | `true`                         | Set false to use NoopSink                            |
| `TOON_LOG_DB_PATH`             | `data/interactions.duckdb`     | DuckDB file path                                     |
| `TOON_LOG_PARQUET_DIR`         | `data/parquet`                 | Parquet export root                                  |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | `300`                          | Periodic Parquet flush interval in seconds           |
| `TOON_LOG_BUFFER_SIZE`         | `1000`                         | mpsc channel capacity (backpressure threshold)       |
| `TOON_LOG_LEVEL`               | `info`                         | tracing filter string                                |
| `TOON_CLIENT_HINT`             | `""`                           | Tag log rows: "opencode" or "claude-desktop"         |

---

## 9. Client Integration

### 9.1 opencode

opencode runs the binary as a local child process via the `"type": "local"`
MCP config. The `opencode.json` at the repo root (section 12) is the
complete configuration.

AGENTS.md instruction for opencode sessions:

```
When receiving JSON, JSONL, CSV, or TSV content larger than 256 bytes before
including it in analysis, call toon compress_content to reduce token
consumption. Use toon compression_stats to preview savings first. TOON output
is human-readable — no decoding is required before use.
```

### 9.2 Claude Desktop

Claude Desktop uses the same stdio binary via its `claude_desktop_config.json`
file. No code change is required — only the host config differs.

**Config file location:**

| OS      | Path                                                              |
|---------|-------------------------------------------------------------------|
| macOS   | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json`                     |
| Linux   | `~/.config/Claude/claude_desktop_config.json`                     |

**claude_desktop_config.json:**

```json
{
  "mcpServers": {
    "toon": {
      "command": "/absolute/path/to/toon-mcp/target/release/toon-mcp-server",
      "args": [],
      "env": {
        "TOON_COMPRESSION_THRESHOLD": "0.85",
        "TOON_MIN_BYTES": "256",
        "TOON_KEY_FOLDING": "true",
        "TOON_DELIMITER": "comma",
        "TOON_LOG_ENABLED": "true",
        "TOON_LOG_DB_PATH": "/absolute/path/to/toon-mcp/data/interactions.duckdb",
        "TOON_LOG_PARQUET_DIR": "/absolute/path/to/toon-mcp/data/parquet",
        "TOON_LOG_FLUSH_INTERVAL_SECS": "300",
        "TOON_LOG_BUFFER_SIZE": "1000",
        "TOON_LOG_LEVEL": "info",
        "TOON_CLIENT_HINT": "claude-desktop"
      }
    }
  }
}
```

Claude Desktop requires absolute paths in `command` and in all env vars
referencing file system locations. Relative paths will not resolve correctly
because Claude Desktop does not set a working directory for child processes.

The `TOON_CLIENT_HINT` value differs between clients so log queries can split
metrics by host. The binary, build, and all server logic are identical.

**Verification after config change:**

Quit and fully reopen Claude Desktop (a window close is not sufficient).
The toon tools (`detect_format`, `compress_content`, `compression_stats`)
should appear in the tools panel. If they do not appear, check the MCP
server log at the path below for startup errors:

| OS    | Log path                                          |
|-------|---------------------------------------------------|
| macOS | `~/Library/Logs/Claude/mcp-server-toon.log`       |
| Linux | `~/.config/Claude/logs/mcp-server-toon.log`       |

---

## 10. Dependency Manifest

### Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members  = [
    "crates/toon-mcp-core",
    "crates/toon-mcp-logging",
    "crates/toon-mcp-server",
    "crates/toon-mcp-bench",
]

[workspace.dependencies]
# MCP
rmcp               = { version = "0.14", features = ["server", "transport-io", "macros", "schemars"] }

# TOON
toon-format        = "0.4"

# Serialization
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
schemars           = "1"

# CSV/TSV parsing
csv                = "1.3"

# Async
tokio              = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
async-trait        = "0.1"

# Database
duckdb             = { version = "1.1", features = ["bundled"] }

# Benchmarking
criterion          = { version = "0.5", features = ["html_reports"] }

# Observability
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Utilities
dotenvy            = "0.15"
thiserror          = "2"
uuid               = { version = "1", features = ["v4"] }
chrono             = { version = "0.4", features = ["serde"] }

[workspace.package]
edition = "2024"
```

### rust-toolchain.toml

```toml
[toolchain]
channel = "1.93.0"
```

### Per-crate Cargo.toml (toon-mcp-core)

```toml
[package]
name    = "toon-mcp-core"
version = "0.1.0"
edition.workspace = true

[dependencies]
toon-format.workspace = true
serde_json.workspace  = true
serde.workspace       = true
csv.workspace         = true
thiserror.workspace   = true
```

### Per-crate Cargo.toml (toon-mcp-logging)

```toml
[package]
name    = "toon-mcp-logging"
version = "0.1.0"
edition.workspace = true

[dependencies]
duckdb.workspace       = true
tokio.workspace        = true
async-trait.workspace  = true
serde.workspace        = true
thiserror.workspace    = true
uuid.workspace         = true
chrono.workspace       = true
tracing.workspace      = true
```

### Per-crate Cargo.toml (toon-mcp-server)

```toml
[package]
name    = "toon-mcp-server"
version = "0.1.0"
edition.workspace = true

[[bin]]
name = "toon-mcp-server"
path = "src/main.rs"

[dependencies]
toon-mcp-core    = { path = "../toon-mcp-core" }
toon-mcp-logging = { path = "../toon-mcp-logging" }
rmcp.workspace         = true
serde.workspace        = true
serde_json.workspace   = true
schemars.workspace     = true
tokio.workspace        = true
async-trait.workspace  = true
thiserror.workspace    = true
dotenvy.workspace      = true
tracing.workspace      = true
tracing-subscriber.workspace = true
uuid.workspace         = true
chrono.workspace       = true
```

### Per-crate Cargo.toml (toon-mcp-bench)

```toml
[package]
name    = "toon-mcp-bench"
version = "0.1.0"
edition.workspace = true

[[bench]]
name    = "detection"
harness = false

[[bench]]
name    = "classification"
harness = false

[[bench]]
name    = "compression"
harness = false

[dependencies]
toon-mcp-core = { path = "../toon-mcp-core" }

[dev-dependencies]
criterion.workspace = true
```

---

## 11. AGENTS.md

```markdown
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
| https://docs.rs/duckdb/latest/duckdb/ | Any work on duckdb_sink.rs or schema changes |
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
- No build.rs files unless a C dependency requires bindgen (duckdb bundled
  uses one internally — do not add another).
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
- The duckdb::Connection MUST live exclusively on the DuckDbSink background
  task. It MUST NOT be wrapped in Arc<Mutex<Connection>>.

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
  Handlers MUST NOT import duckdb directly.
- Claude Desktop requires absolute paths in all env config values that
  reference the file system. Document this clearly in README.md.

---

## Benchmarking

- All benchmark fixtures live in crates/toon-mcp-bench/fixtures/.
- Benchmark harness: Criterion exclusively.
- Benchmarks MUST NOT start a tokio runtime. They measure synchronous
  core functions only.
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
| Arc<Mutex<duckdb::Connection>> | Replaced by writer task + mpsc channel |
| Box<dyn Error> in public APIs | Untyped error surface |
| .unwrap() outside tests/main | Panics on production paths |
| anyhow in library crates | Leaks opaque errors to callers |
| Wildcard dependency versions | Reproducibility |
| Emojis anywhere | Consistency |
| Direct LLM API calls from server | Server is a tool provider only |
| System prompt injection per turn | Instructions registered once in ServerInfo |
| parquet crate (Apache Arrow) | DuckDB COPY TO handles Parquet natively |
| XML, YAML, TOML parsers (v1) | Out of scope; planned for future plugin iteration |
| toon-mcp-bench importing toon-mcp-server | Bench depends on toon-mcp-core (sync benches) or toon-mcp-logging (async benches only); never on toon-mcp-server |
```

---

## 12. opencode.json

```json
{
  "$schema": "https://opencode.ai/config.json",
  "instructions": [
    "AGENTS.md"
  ],
  "mcp": {
    "toon": {
      "type": "local",
      "command": ["./target/release/toon-mcp-server"],
      "enabled": true,
      "environment": {
        "TOON_COMPRESSION_THRESHOLD": "0.85",
        "TOON_MIN_BYTES": "256",
        "TOON_KEY_FOLDING": "true",
        "TOON_DELIMITER": "comma",
        "TOON_LOG_ENABLED": "true",
        "TOON_LOG_DB_PATH": "data/interactions.duckdb",
        "TOON_LOG_PARQUET_DIR": "data/parquet",
        "TOON_LOG_FLUSH_INTERVAL_SECS": "300",
        "TOON_LOG_BUFFER_SIZE": "1000",
        "TOON_LOG_LEVEL": "info",
        "TOON_CLIENT_HINT": "opencode"
      }
    }
  }
}
```

---

## 13. Build, Test, and Release

### Development build

```bash
cargo build
```

### Run locally (reads .env)

```bash
cargo run -p toon-mcp-server
```

### Release binary

```bash
cargo build --release
# binary: ./target/release/toon-mcp-server
# use this absolute path in both opencode.json and claude_desktop_config.json
```

### Tests

```bash
cargo test --workspace
```

Unit test coverage requirements:

- `detector.rs`: one test per InputFormat variant including Unknown. Test
  probe ordering (JSON wins over JSONL on single-line valid JSON).
- `parser/jsonl.rs`: uniform objects, mixed types, malformed line.
- `parser/csv.rs`: header detection, numeric coercion, tab delimiter path.
- `classifier.rs`: one test per ShapeClass variant including edge cases.
- `compressor.rs`: each PassThroughReason path; encode path above and below
  threshold; JSONL and CSV inputs end-to-end.
- `duckdb_sink.rs`: integration test with `Connection::open_in_memory()`,
  send N events, flush, assert row count and column values.
- `handler.rs`: all three tools with NoopSink or MemorySink.

### Benchmarks

```bash
# run all benchmark groups
cargo bench --package toon-mcp-bench

# HTML reports land in: target/criterion/

# save a named baseline after a perf improvement
cargo bench --package toon-mcp-bench -- --save-baseline my-baseline

# compare against a saved baseline
cargo bench --package toon-mcp-bench -- --load-baseline my-baseline
```

### Lint gate

```bash
cargo fmt --check && cargo clippy -- -D warnings
```

Both MUST pass clean before any commit.

### Query the interaction log

```bash
# live DuckDB query
duckdb data/interactions.duckdb \
  "SELECT input_format, shape_class, COUNT(*), AVG(savings_pct)
   FROM tool_log GROUP BY 1, 2 ORDER BY 3 DESC"

# query across all Parquet partitions
duckdb :memory: \
  "SELECT * FROM read_parquet('data/parquet/**/*.parquet',
   hive_partitioning=true) LIMIT 20"
```

---

## 14. Implementation Order

Build in this sequence. Each phase is independently testable before the next
begins.

```
Phase 1 — Workspace scaffold
  1.1  Cargo.toml (workspace), rust-toolchain.toml, rustfmt.toml, .clippy.toml
  1.2  data/.gitkeep, .env.example
  1.3  AGENTS.md, opencode.json

Phase 2 — Core: format detection
  2.1  toon-mcp-core/src/error.rs
  2.2  toon-mcp-core/src/parser/mod.rs          (Parser trait)
  2.3  toon-mcp-core/src/parser/json.rs         + unit tests
  2.4  toon-mcp-core/src/parser/jsonl.rs        + unit tests
  2.5  toon-mcp-core/src/parser/csv.rs          + unit tests (csv + tsv)
  2.6  toon-mcp-core/src/detector.rs            + unit tests (all probes + ordering)
  2.7  toon-mcp-core/src/lib.rs

Phase 3 — Core: classification and compression
  3.1  toon-mcp-core/src/classifier.rs          + unit tests
  3.2  toon-mcp-core/src/compressor.rs          + unit tests
  3.3  Integration tests: JSONL -> Tabular -> Compressed end-to-end
  3.4  Integration tests: CSV  -> Tabular -> Compressed end-to-end

Phase 4 — Logging
  4.1  toon-mcp-logging/src/event.rs
  4.2  toon-mcp-logging/src/error.rs
  4.3  toon-mcp-logging/src/sink.rs             (trait)
  4.4  toon-mcp-logging/src/noop_sink.rs
  4.5  toon-mcp-logging/src/memory_sink.rs
  4.6  toon-mcp-logging/src/duckdb_sink.rs      + integration test
  4.7  toon-mcp-logging/src/lib.rs

Phase 5 — Server
  5.1  toon-mcp-server/src/config.rs
  5.2  toon-mcp-server/src/error.rs
  5.3  toon-mcp-server/src/handler.rs           + tests using NoopSink
  5.4  toon-mcp-server/src/server.rs            (ServerHandler + tool_router)
  5.5  toon-mcp-server/src/main.rs
  5.6  Smoke test via opencode: all three tools, verify log row written
  5.7  Smoke test via Claude Desktop: verify tools appear, verify log row

Phase 6 — Benchmarks
  6.1  crates/toon-mcp-bench/Cargo.toml
  6.2  bench/fixtures/ (generate or copy fixture files)
  6.3  benches/detection.rs                     + baseline save
  6.4  benches/classification.rs                + baseline save
  6.5  benches/compression.rs                   + baseline save
  6.6  Commit baselines to bench/baselines/

Phase 7 — Hardening
  7.1  README.md: setup, build, opencode config, Claude Desktop config,
       absolute path requirement note, log query examples
  7.2  Full clippy clean pass across all crates
  7.3  cargo audit
  7.4  cargo doc --no-deps (verify all public items documented)
  7.5  Release build verification against both clients
```
