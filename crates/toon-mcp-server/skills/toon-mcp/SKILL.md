---
name: toon-mcp
description: Compress JSON, JSONL, CSV, or TSV with the toon-mcp server when structured data needs to fit in an agent's context window.
---

# Compress structured data

Use the connected toon-mcp server's `compress_content` tool with the source text in the `input` string. For example, send `{"input":"id,name\n1,Ada\n2,Lin"}`. Preserve the original delimiters and newlines.

Use the returned `output` directly. When `compressed` is false, `pass_reason` explains why the tool returned the original text. Savings are measured in bytes. If CSV or TSV identifiers must retain their spelling and `lossy_coercion_possible` is true, use the original input.

Call `detect_format` when the format is in doubt, or `compression_stats` when only a size estimate is needed. Both accept the same input. Call `toon_diagnostics` with `{}` to inspect server health.
