# Deployment

This page gives release-binary deployment examples for operators who run `toon-mcp-server` outside an interactive shell. All filesystem paths are examples; replace them with absolute paths for your host.

## Build or Install

Build locally:

```bash
cargo build --release --package toon-mcp-server
install -m 0755 target/release/toon-mcp-server /usr/local/bin/toon-mcp-server
```

Or download a release binary for one of the supported targets and verify `checksums.sha256` from the GitHub Release. Release signing and SBOM artifacts are not published yet.

Supported release targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

## Environment File

Use an environment file for supervised deployments:

```bash
TOON_LOG_ENABLED=true
TOON_LOG_DIR=/var/lib/toon-mcp/logs
TOON_LOG_LEVEL=info
TOON_CLIENT_HINT=systemd
TOON_MAX_INPUT_BYTES=10485760
TOON_MAX_CONCURRENT_CALLS=8
TOON_PIPELINE_TIMEOUT_MS=30000
TOON_CSV_NUMERIC_COERCION=true
```

Create the log directory before starting the service and make it writable by the service user.

## systemd Unit

Example system service at `/etc/systemd/system/toon-mcp.service`:

```ini
[Unit]
Description=toon-mcp local MCP server
After=network.target

[Service]
Type=simple
User=toon-mcp
Group=toon-mcp
EnvironmentFile=/etc/toon-mcp.env
ExecStart=/usr/local/bin/toon-mcp-server
Restart=on-failure
RestartSec=5s
TimeoutStopSec=20s
WorkingDirectory=/var/lib/toon-mcp
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/toon-mcp/logs

[Install]
WantedBy=multi-user.target
```

Commands:

```bash
sudo useradd --system --home /var/lib/toon-mcp --create-home toon-mcp
sudo install -d -o toon-mcp -g toon-mcp /var/lib/toon-mcp/logs
sudo systemctl daemon-reload
sudo systemctl enable --now toon-mcp.service
sudo journalctl -u toon-mcp.service -f
```

Note: MCP stdio servers are usually launched by an MCP client. A standalone systemd service is useful only when another supervisor or wrapper connects stdio appropriately. For normal desktop MCP use, configure the client to launch the binary directly.

## macOS launchd Agent

Example user agent at `~/Library/LaunchAgents/com.example.toon-mcp.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.example.toon-mcp</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/bin/toon-mcp-server</string>
  </array>
  <key>WorkingDirectory</key>
  <string>/Users/you/Library/Application Support/toon-mcp</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>TOON_LOG_ENABLED</key>
    <string>true</string>
    <key>TOON_LOG_DIR</key>
    <string>/Users/you/Library/Logs/toon-mcp</string>
    <key>TOON_CLIENT_HINT</key>
    <string>launchd</string>
    <key>TOON_MAX_CONCURRENT_CALLS</key>
    <string>8</string>
    <key>TOON_PIPELINE_TIMEOUT_MS</key>
    <string>30000</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>StandardErrorPath</key>
  <string>/Users/you/Library/Logs/toon-mcp/stderr.log</string>
  <key>StandardOutPath</key>
  <string>/Users/you/Library/Logs/toon-mcp/stdout.log</string>
</dict>
</plist>
```

Commands:

```bash
mkdir -p "$HOME/Library/Logs/toon-mcp"
launchctl load "$HOME/Library/LaunchAgents/com.example.toon-mcp.plist"
launchctl start com.example.toon-mcp
tail -f "$HOME/Library/Logs/toon-mcp/stderr.log"
```

As with systemd, desktop MCP clients usually launch stdio servers themselves. Prefer direct Claude Desktop or opencode configuration unless you have a wrapper that connects to the launchd process.

## Claude Desktop Deployment

Claude Desktop should launch the binary directly:

```json
{
  "mcpServers": {
    "toon": {
      "command": "/Users/you/projects/toon-mcp/target/release/toon-mcp-server",
      "env": {
        "TOON_LOG_DIR": "/Users/you/projects/toon-mcp/data/logs",
        "TOON_CLIENT_HINT": "claude-desktop",
        "TOON_MAX_CONCURRENT_CALLS": "8",
        "TOON_PIPELINE_TIMEOUT_MS": "30000"
      }
    }
  }
}
```

Claude Desktop does not inherit your shell environment. Use absolute paths.

## Upgrade

1. Build or download the new binary.
2. Verify checksums when using release artifacts.
3. Stop the client or supervisor that owns the process.
4. Replace the binary atomically where possible.
5. Start the client or supervisor.
6. Confirm startup stderr includes `toon-mcp-server ready`.
7. Confirm new `events.jsonl` rows are written if logging is enabled.

## Rollback

1. Stop the client or supervisor.
2. Restore the previous known-good binary.
3. Restore the previous environment file if configuration changed.
4. Start the client or supervisor.
5. Confirm the binary starts and logs are writable.

JSONL logs are append-only by date partition. Rollback does not require schema migration today, but pre-`1.0.0` releases may change fields. Keep analysis queries tolerant of missing or extra fields.

## Retention Job Examples

Delete partitions older than 30 days with cron:

```cron
15 3 * * * find /var/lib/toon-mcp/logs -maxdepth 1 -type d -name 'day=*' -mtime +30 -exec rm -rf {} \;
```

For systemd, pair a oneshot service with a timer and run the same cleanup command as the `toon-mcp` user.
