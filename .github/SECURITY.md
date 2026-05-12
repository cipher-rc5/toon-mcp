# Security Policy

## Supported Versions

Only the latest commit on the active development branch is actively
maintained. The active branch is `master`; CI also runs on `main` while both
branch names exist.

| Version                          | Supported |
| -------------------------------- | --------- |
| latest commit on `master`        | Yes       |
| older commits or tagged releases | No        |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report vulnerabilities through GitHub's private vulnerability reporting:

1. Open <https://github.com/cipher-rc5/toon-mcp/security/advisories/new>.
2. Fill in the advisory form with the details below.
3. Submit. The report is visible only to the maintainers until a coordinated
   disclosure date is agreed.

If GitHub's private reporting is unavailable to you, open a minimal public
issue titled "Security contact request" without any vulnerability details,
and a maintainer will arrange a private channel.

Include the following in your report:

- A description of the vulnerability and its potential impact.
- Steps to reproduce the issue, ideally with a minimal input that triggers it.
- The commit SHA or release tag the report is against.
- Any suggested remediation if known.

You will receive an acknowledgement within 72 hours. We aim to release a fix
within 14 days of confirming the vulnerability. We will coordinate a public
disclosure date with you after the fix is available, and credit you in the
advisory unless you ask to remain anonymous.

## Scope

toon-mcp is a local MCP server binary. It runs on the local machine and
communicates exclusively over stdio with an MCP client. Treat client-provided
tool input as untrusted or accidental-hostile: a local client can still send
malformed, oversized, or expensive structured data. Network-facing attack
surfaces are out of scope unless a downstream operator wraps the binary in a
network service.

In-scope vulnerabilities include:

- Denial-of-service via malformed inputs that exhaust memory or CPU beyond
  the configured limits (`TOON_MAX_INPUT_BYTES`, `TOON_MAX_CONCURRENT_CALLS`,
  `TOON_PIPELINE_TIMEOUT_MS`).
- Path traversal in `TOON_LOG_DIR` that writes files to unintended locations.
- Arbitrary code execution via crafted input.
- Cross-process log confusion or corruption when one configured log directory
  is unintentionally shared by multiple server processes.
