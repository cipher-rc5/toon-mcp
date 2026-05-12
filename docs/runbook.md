<!-- file: docs/runbook.md -->
<!-- description: Operator runbook for production toon-mcp-server deployments -->

# Operator Runbook

This runbook is for operators running `toon-mcp-server` in production. Every
incident section follows the same structure:

- **Symptom** — what an operator or user observes.
- **Diagnostic** — specific shell commands or DuckDB queries to confirm.
- **Likely cause** — common root causes, ordered by frequency.
- **Remediation** — concrete actions, usually a config change plus a restart.

All DuckDB queries assume `TOON_LOG_DIR` is `data/logs`. Substitute your
configured path where applicable.

---

## 1. High Memory Usage

### Symptom

Resident set size (RSS) climbs past the expected steady-state for a
`toon-mcp-server` process. Steady-state RSS is typically a few tens of MiB
plus roughly `TOON_MAX_CONCURRENT_CALLS * average_input_bytes` and the
pending-log buffer.

### Diagnostic

```bash
ps -o pid,rss,command -p $(pgrep -f toon-mcp-server)
```

Confirm the effective limits the server is running with:

```bash
ps eww $(pgrep -f toon-mcp-server) | tr ' ' '\n' | grep -E '^TOON_(MAX_INPUT_BYTES|MAX_CONCURRENT_CALLS|LOG_BUFFER_SIZE|LOG_FLUSH_INTERVAL_SECS)='
```

Recent large inputs that may be inflating peak memory:

```sql
SELECT ts_us, tool_name, input_bytes, duration_us, client_hint
FROM read_json('data/logs/**/*.jsonl')
WHERE input_bytes > 1048576
ORDER BY ts_us DESC
LIMIT 50;
```

### Likely Cause

1. Oversized payloads are not being rejected early — `TOON_MAX_INPUT_BYTES`
   is too high for the host.
2. Too many concurrent blocking pipelines holding parsed inputs in memory —
   `TOON_MAX_CONCURRENT_CALLS` is too high.
3. The pending log buffer has grown larger than expected because
   `TOON_LOG_BUFFER_SIZE` is too high or `TOON_LOG_FLUSH_INTERVAL_SECS` is
   too long (events sit in memory between flushes).

### Remediation

Tune one or more of:

- Lower `TOON_MAX_INPUT_BYTES` (default `10485760` / 10 MiB) to reject large
  inputs without parsing.
- Lower `TOON_MAX_CONCURRENT_CALLS` (default `8`).
- Lower `TOON_LOG_BUFFER_SIZE` (default `1000`) and/or
  `TOON_LOG_FLUSH_INTERVAL_SECS` (default `300`).

Restart the server. All config values are read once in `Config::load()` at
startup; there is no live reload.

---

## 2. Repeated `server busy` Errors

### Symptom

Clients see `"server busy: too many concurrent calls"` (the `McpError`
text returned by the tool handler when the concurrency limiter rejects a
call).

### Diagnostic

Handler invocations per second over the last hour, compared against
`TOON_MAX_CONCURRENT_CALLS`:

```sql
SELECT
    strftime(to_timestamp(ts_us / 1000000), '%Y-%m-%d %H:%M:%S') AS sec,
    count(*) AS calls
FROM read_json('data/logs/**/*.jsonl')
WHERE ts_us > (epoch_us(now()) - 3600 * 1000000)
GROUP BY sec
ORDER BY calls DESC
LIMIT 20;
```

Slowest recent calls (these hold concurrency slots the longest):

```sql
SELECT ts_us, tool_name, client_hint, input_bytes, duration_us, pass_reason
FROM read_json('data/logs/**/*.jsonl')
ORDER BY duration_us DESC
LIMIT 20;
```

### Likely Cause

1. Legitimate burst above `TOON_MAX_CONCURRENT_CALLS` (default `8`).
2. A single client running pathologically slow inputs — check `duration_us`
   in the log and identify the offender via `client_hint`.
3. Pipelines effectively wedging slots because `TOON_PIPELINE_TIMEOUT_MS`
   (default `30000`) is too high, letting one bad call hold a slot for 30 s.

### Remediation

- Raise `TOON_MAX_CONCURRENT_CALLS` if the host has headroom (memory and CPU).
- Lower `TOON_PIPELINE_TIMEOUT_MS` to free wedged slots sooner; calls that
  exceed it return an error instead of pinning a slot.
- If a specific `client_hint` is responsible, push back on that client to
  pre-filter inputs or run a dedicated instance for it.

Restart after changes.

---

## 3. Log Directory Permission / Disk Issues

### Symptom

`tracing-subscriber` emits messages like
`"JsonlSink flush (buffer full) failed: ..."`,
`"JsonlSink flush (periodic) failed: ..."`, or
`"JsonlSink flush (channel closed) failed: ..."` to stderr. The
`data/logs/day=*/events.jsonl` files are not growing.

### Diagnostic

Confirm the resolved log directory and that it is writable and has space:

```bash
ps eww $(pgrep -f toon-mcp-server) | tr ' ' '\n' | grep '^TOON_LOG_DIR='
test -w "$TOON_LOG_DIR" && echo writable || echo NOT_WRITABLE
df -h "$TOON_LOG_DIR"
ls -la "$TOON_LOG_DIR"
```

Inspect stderr for recent flush failures:

```bash
journalctl -u toon-mcp 2>/dev/null | grep -i 'JsonlSink flush' | tail -20
```

### Likely Cause

1. A relative `TOON_LOG_DIR` (the default `data/logs`) resolved against an
   unexpected working directory — most commonly under Claude Desktop, which
   does not inherit a shell environment and typically launches with `$HOME`
   as cwd.
2. Filesystem permissions on the configured directory.
3. Disk full on the partition backing `TOON_LOG_DIR`.

### Remediation

- Set `TOON_LOG_DIR` to an **absolute path** as the README and
  `docs/configuration.md` direct (required for Claude Desktop).
- Fix directory ownership/permissions so the process user can write.
- Free space: archive or delete old `day=YYYY-MM-DD/` partition
  directories beyond your retention window. There is no built-in rotation.

Restart after changes.

---

## 4. Corrupted or Truncated JSONL Files

### Symptom

DuckDB `read_json(...)` errors on a specific partition file, e.g.
`Invalid Input Error: JSON parse error at line N`, or the line count of
`events.jsonl` differs from what you expect.

### Diagnostic

```bash
wc -l data/logs/day=*/events.jsonl
```

Validate a suspect partition line by line:

```bash
python3 -c "import json,sys
for i,l in enumerate(open(sys.argv[1]),1):
    try: json.loads(l)
    except Exception as e:
        print(i, repr(l[:200]), e); sys.exit(1)
print('ok')" data/logs/day=YYYY-MM-DD/events.jsonl
```

### Likely Cause

1. Process killed mid-write — JSONL has no atomic guarantee for partial
   lines, and `flush_pending` writes a batch via `BufWriter`.
2. Disk full mid-flush (combine with section 3 diagnostics).
3. A second `toon-mcp-server` process writing to the same `TOON_LOG_DIR`.
   The design assumes one writer per directory — there is no inter-process
   locking, only an in-process file-handle cache.

### Remediation

- Identify the bad line with the Python check above. Truncate the file at
  that line, keeping a backup:

  ```bash
  cp data/logs/day=YYYY-MM-DD/events.jsonl{,.bak}
  sed -i.bak '${/^$/d;}' data/logs/day=YYYY-MM-DD/events.jsonl
  # or: head -n <good_line_count> ... > events.jsonl.fixed && mv
  ```

- Ensure only one `toon-mcp-server` process targets a given `TOON_LOG_DIR`.
  Use `pgrep -af toon-mcp-server` and compare each process's resolved
  `TOON_LOG_DIR`.

---

## 5. Excessive Pass-Through Rate

### Symptom

`compressed=false` for the vast majority of payloads in the log;
compression is not delivering meaningful token savings.

### Diagnostic

Pass-through breakdown by `pass_reason`:

```sql
SELECT pass_reason, count(*) AS n
FROM read_json('data/logs/**/*.jsonl')
WHERE compressed = false
GROUP BY pass_reason
ORDER BY n DESC;
```

Cross-tab `pass_reason` against `shape_class` and `input_format`:

```sql
SELECT input_format, shape_class, pass_reason, count(*) AS n
FROM read_json('data/logs/**/*.jsonl')
WHERE compressed = false
GROUP BY 1, 2, 3
ORDER BY n DESC
LIMIT 30;
```

### Likely Cause by `pass_reason`

- `below_min_bytes` — inputs are smaller than `TOON_MIN_BYTES` (default
  `256`). Lower it if you want to attempt compression on smaller payloads.
- `unknown_format` — inputs are prose or non-supported formats; TOON
  cannot help. No action needed.
- `insufficient_savings` — encoding succeeded but the output exceeded
  `TOON_COMPRESSION_THRESHOLD * input_bytes`. Loosen the threshold
  (e.g. `0.95`) if smaller savings are acceptable.
- `shape_not_beneficial` — the classifier returned `PassThrough`. Inspect
  `shape_class` for those entries; if many are `mixed` or
  `pass_through` you can lower the shape thresholds
  (`TOON_TABULAR_MIN_ROWS`, `TOON_FOLD_MIN_DEPTH`,
  `TOON_PRIMITIVE_ARRAY_MIN`).
- `parse_failed:<format>:<detail>` — format detection succeeded but
  parsing did not. Investigate the offending payload bytes; the suffix in
  `pass_reason` identifies the format and parser error.

### Remediation

Tune the relevant config variables and restart:

| `pass_reason` | Knob |
|---|---|
| `below_min_bytes` | `TOON_MIN_BYTES` |
| `insufficient_savings` | `TOON_COMPRESSION_THRESHOLD` |
| `shape_not_beneficial` | `TOON_TABULAR_MIN_ROWS`, `TOON_FOLD_MIN_DEPTH`, `TOON_PRIMITIVE_ARRAY_MIN` |
| `parse_failed:*` | Fix upstream payload; consider `TOON_CSV_NUMERIC_COERCION=false` for ID-like CSV columns |
| `unknown_format` | No action |

See `docs/configuration.md` for tuning guidance and example aggressive /
conservative profiles.

---

## 6. Writer Task Panic or Unexpected Exit

### Symptom

stderr shows a structured `error!` line from the supervisor with
`component="jsonl_sink_writer"` and one of:

- `"writer task panicked; subsequent log events will be dropped"`
- `"writer task exited unexpectedly; subsequent log events will be dropped"`

Subsequent tool calls still succeed (logging is fire-and-forget) but no
new lines are appended to `events.jsonl`.

### Diagnostic

Find the supervisor message and any nearby stack info:

```bash
journalctl -u toon-mcp 2>/dev/null | grep -i 'jsonl_sink_writer' | tail -20
```

If you have direct access to a `JsonlSink` (e.g. via a future debug
surface), `record_failed_count()` increments each time a `record()` call
fails because the writer-task channel is closed:

```text
JsonlSink::record_failed_count()        // never-recovered events
JsonlSink::serialization_failed_count() // serde_json::to_string failures
```

### Likely Cause

- Disk error during `flush_pending`.
- Panic in `serde_json::to_writer` or `BufWriter::flush` (rare — flat
  owned-string struct).
- Host out-of-memory killed the writer task.

### Remediation

There is no in-process recovery for a dead writer task by design.
Restart the server. After restart, append-mode reopens of partition
files preserve previous content.

If reproducible, file an issue with the stderr panic message and the
events that preceded the crash (the last events in the partition file
are a good start).

---

## 7. Channel-Full Event Drops

### Symptom

Log files contain fewer events than expected; no JSONL parse errors.
The `record()` channel returns `Err(LogError::ChannelSend(...))` and the
error is discarded by handlers (fire-and-forget semantics).

### Diagnostic

Compare expected client call count to actual partition line count for
the same window:

```sql
SELECT
    strftime(to_timestamp(ts_us / 1000000), '%Y-%m-%d %H') AS hour,
    count(*) AS rows_written
FROM read_json('data/logs/**/*.jsonl')
GROUP BY hour
ORDER BY hour DESC
LIMIT 24;
```

If a `JsonlSink` debug surface is available, query
`record_failed_count()` directly — a non-zero value indicates the
writer-task channel rejected sends.

### Likely Cause

1. The writer task is lagging behind handler throughput — disk slow or
   large flush batches stalling the channel consumer.
2. `TOON_LOG_BUFFER_SIZE` is too small for the call rate; the bounded
   `mpsc::channel(buffer_size)` fills and `send` returns an error.

### Remediation

- Raise `TOON_LOG_BUFFER_SIZE` (default `1000`).
- Investigate writer-task slowness: check disk latency for the partition
  directory; check for large flush batches by lowering
  `TOON_LOG_FLUSH_INTERVAL_SECS` so the buffer is drained more often.
- Restart after config changes.

---

## 8. Shutdown Not Draining Logs

### Symptom

Events from the last few seconds before shutdown are missing from JSONL
files. stderr around the shutdown shows one of:

- `"log sink flush timed out after 10 s: ..."`
- `"writer-task supervisor did not complete within 5 s"`

### Diagnostic

```bash
journalctl -u toon-mcp 2>/dev/null | grep -E 'flush timed out|supervisor did not complete' | tail -20
```

Confirm the shutdown path that ran. The server logs:

- `"received SIGTERM"` or `"received SIGINT"` on signal-driven shutdown
- `"MCP service terminated cleanly (stdin closed)"` on stdin EOF
- `"toon-mcp-server shutting down — flushing log sink"` before the
  acknowledged `flush()`

### Likely Cause

Disk slow during shutdown — the acknowledged `flush()` in `main.rs` has
a 10 s timeout, and the writer-task supervisor has an additional 5 s
timeout. Both firing means a flush is still in flight at SIGKILL.

### Remediation

- Give the process more time before SIGKILL. If running under systemd,
  raise `TimeoutStopSec=` to at least 20 s.
- Investigate disk latency; the writer's final `flush_pending` is bound
  by the slowest partition file's `write_all` + `flush` syscalls.

---

## Operational Checklist

Run through these at deploy time and on a recurring basis:

- `TOON_LOG_DIR` is an absolute path (mandatory under Claude Desktop).
- The process user can write to `TOON_LOG_DIR` and the partition has
  headroom (`df -h`).
- Exactly one `toon-mcp-server` writes to a given `TOON_LOG_DIR`.
- Retention: archive or delete `day=YYYY-MM-DD/` directories beyond
  your retention window. Optionally export old data to Parquet using
  the DuckDB `COPY ... TO ... (FORMAT PARQUET)` query in
  `docs/logging.md`.
- Startup line — confirm config loaded as intended:

  ```text
  [INFO] toon_mcp_server::config: loaded config compression_threshold=0.85 min_bytes=256 ...
  ```

- Readiness anchor — scrapers can match on:

  ```text
  status="ready" component="toon-mcp-server" "toon-mcp-server ready"
  ```
