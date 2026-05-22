# Production Readiness

toon-mcp is production-usable as a local stdio MCP server when operators treat it as a bounded, single-process tool provider and size it for the host. It is not a network service and does not expose a health or metrics endpoint today.

## Supported Deployment Modes

| Mode | Status | Notes |
| ---- | ------ | ----- |
| opencode local MCP process | Supported | Relative paths work when opencode starts from the repository root. |
| Claude Desktop local MCP process | Supported | Use absolute paths for the binary and every filesystem env value. |
| systemd user or system service | Supported | Use an absolute binary path and environment file. See `docs/deployment.md`. |
| macOS launchd agent | Supported | Use absolute paths in the plist. See `docs/deployment.md`. |
| Network-exposed service | Not supported | The binary speaks MCP over stdio only. Add a separately reviewed wrapper if needed. |
| Multi-writer shared log directory | Not supported | Use one `TOON_LOG_DIR` per process. There is no advisory lock. |

## Compatibility

- Rust: exactly the toolchain pinned in `rust-toolchain.toml` (`1.95.0`).
- Release artifacts: Linux GNU and macOS for `x86_64` and `aarch64`.
- MCP: local stdio clients that can launch a subprocess and pass environment variables.
- Data formats: JSON, JSONL, CSV, TSV. XML, YAML, and TOML are out of scope for v1.
- API stability: pre-`1.0.0`; tool schemas, public Rust APIs, and environment variables may change in minor releases.

## Resource Sizing

The main resource controls are:

| Variable | Default | Production role |
| -------- | ------- | --------------- |
| `TOON_MAX_INPUT_BYTES` | `10485760` | Hard admission limit before parsing. Lower this first on memory-constrained hosts. |
| `TOON_MAX_CONCURRENT_CALLS` | `8` | Maximum simultaneous blocking pipelines. This is the primary CPU and memory concurrency cap. |
| `TOON_PIPELINE_TIMEOUT_MS` | `30000` | Handler-visible deadline for permit wait and pipeline result. Started blocking work may continue after timeout. |
| `TOON_LOG_BUFFER_SIZE` | `1000` | Maximum queued/buffered logging pressure before flush and channel backpressure matter. |
| `TOON_LOG_FLUSH_INTERVAL_SECS` | `300` | Upper bound on normal telemetry flush delay. |

Sizing profiles:

| Host | Suggested starting point |
| ---- | ------------------------ |
| Small/shared laptop or 1-2 vCPU VM | `TOON_MAX_INPUT_BYTES=1048576`, `TOON_MAX_CONCURRENT_CALLS=2`, `TOON_PIPELINE_TIMEOUT_MS=10000` |
| Normal developer workstation | defaults: `10485760`, `8`, `30000` |
| Dedicated high-core host | raise `TOON_MAX_CONCURRENT_CALLS` gradually only after observing `server busy` errors and confirming CPU/RSS headroom |

Memory planning is approximate because parsing normalises inputs into `serde_json::Value` and compression creates output strings. Budget for the input string, parsed value tree, output string, and per-call overhead multiplied by `TOON_MAX_CONCURRENT_CALLS`, plus the log buffer. For hostile or unknown inputs, prefer a smaller `TOON_MAX_INPUT_BYTES` over relying on timeout.

## Operational Readiness Checklist

- Build and deploy the release binary, not a debug binary.
- Set `TOON_LOG_DIR` to an absolute path under process supervisors and Claude Desktop.
- Use a unique `TOON_LOG_DIR` per server process.
- Set `TOON_CLIENT_HINT` when multiple clients or environments write logs.
- Confirm startup stderr includes the ready line: `status="ready" component="toon-mcp-server"`.
- Confirm JSONL logs are growing if `TOON_LOG_ENABLED=true`.
- Define a retention window before production use.
- Keep rollback binaries available for the previous known-good release.

## Durability and Observability

JSONL logs are best-effort telemetry. They are useful for troubleshooting, capacity planning, pass-through analysis, and rough usage counts. They are not audit-grade records.

Current limitations:

- Handler success is independent from logging success.
- Logging errors are not exposed through a health endpoint or metrics endpoint.
- Events can be lost on process crash, disk error, channel closure, or shutdown timeout.
- File writes flush process buffers but do not fsync with `sync_data` or `sync_all`.
- There is no inter-process lock for a shared log directory.
- The `JsonlSink` keeps one open file handle per UTC day partition for the lifetime of the process; long-running daemons (multi-month uptime) accumulate one handle per day. Restart the process if file-descriptor pressure matters in your environment.

If audit-grade logging is required, add durable queueing, fsync policy, explicit log-loss surfacing, and multi-process coordination before relying on JSONL output.

## Retention

The server writes hive partitions at `day=YYYY-MM-DD/events.jsonl` and does not rotate or delete files.

Recommended policy:

- Keep 7-30 days online for local troubleshooting.
- Export older records to Parquet if long-term analysis is needed.
- Delete whole `day=YYYY-MM-DD` directories after export or expiry.

Example cleanup command:

```bash
find /absolute/path/to/toon-mcp/data/logs -maxdepth 1 -type d -name 'day=*' -mtime +30 -print -exec rm -rf {} \;
```

## Known Non-Goals

- No HTTP health, metrics, or readiness endpoint.
- No network listener.
- No multi-tenant isolation.
- No built-in log retention or rotation.
- No signed or attested release artifacts yet; releases currently publish checksums only.
- No SBOM artifact in releases yet.
- No cancellation of already-started blocking compression work after handler timeout.

## See Also

- `docs/deployment.md` for systemd, launchd, upgrade, and rollback examples.
- `docs/configuration.md` for the full environment variable reference.
- `docs/runbook.md` for incident diagnostics and remediation.
- `docs/logging.md` for JSONL schema, query examples, and durability details.
