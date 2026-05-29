# toon-evals

Comprehensive evaluation harness for the toon-mcp compression pipeline. Uses a
local model (via an OpenAI-compatible `llama-server`) to **generate** high
volumes of varied structured payloads and to **judge** comprehension, then
scores the pipeline on four axes:

1. **Token savings** — exact, via the model's own tokenizer (`/tokenize`). This
   is the real context-window win, distinct from the byte ratio the pipeline
   gates on.
2. **Lossless round-trip** — `parse → encode → decode` must reproduce the parsed
   value (path-expansion on, so safe key-folding is reconstructed).
3. **Comprehension parity** — the model answers deterministic retrieval
   questions over the JSON form vs the TOON form; accuracy is compared.
4. **Classification accuracy** — the classifier's shape vs the shape the
   generator intended.

The source is tracked, but generated data under `results/` and your private
server config (`.env.eval`) are gitignored. It builds as its own standalone
crate against `../crates/toon-mcp-core` and is **not** a member of the root
cargo workspace.

> **Full documentation** lives in [`docs/`](docs/): [usage](docs/usage.md)
> (commands + llama.cpp interfacing), [architecture](docs/architecture.md),
> [methodology](docs/methodology.md), and [metrics](docs/metrics.md) — with
> mermaid diagrams throughout.

## 1. Configure your model server

All host-specific values come from environment variables, so nothing private is
committed. Copy the example file and fill in your model path / port:

```bash
cp evals/.env.eval.example evals/.env.eval   # then edit (gitignored)
```

`serve.sh` and the `just serve*` recipes source `.env.eval` automatically. The
key variables (full list in [`serve.sh`](serve.sh) / [`.env.eval.example`](.env.eval.example)):

| var                    | default                 | meaning                                                                     |
| ---------------------- | ----------------------- | --------------------------------------------------------------------------- |
| `TOON_EVAL_MODEL_PATH` | _(required)_            | absolute path to your `.gguf` model                                         |
| `TOON_EVAL_PORT`       | `8080`                  | server port                                                                 |
| `TOON_EVAL_HOST`       | `127.0.0.1`             | bind address (local only)                                                   |
| `TOON_EVAL_CTX_SIZE`   | `8192`                  | context window; raise to `32768` if supported                               |
| `TOON_EVAL_NGL`        | `99`                    | GPU layers to offload; lower if VRAM-limited                                |
| `TOON_EVAL_BASE_URL`   | `http://localhost:8080` | URL the eval client calls                                                   |
| `TOON_EVAL_MODEL`      | `local-model`           | name sent in chat requests (cosmetic; llama-server serves the loaded model) |
| `TOON_EVAL_RESULTS`    | `results`               | output directory                                                            |

## 2. Start the server

From the repo root (recipes wrap [`serve.sh`](serve.sh)):

```bash
just serve          # start in the background, wait until /health is ready
just serve-status   # running? + health check
just serve-logs     # follow the log
just serve-stop     # stop
just serve-fg       # or run in the foreground for debugging
```

Or call the script directly: `evals/serve.sh start|stop|restart|status|logs|wait|foreground`.

## 3. Run the evals

```bash
just eval-all                       # generate → pipeline → comprehend → report

# or stage by stage:
just eval-generate 2                # synthesize corpus.jsonl (per-cell count)
just eval-pipeline                  # bytes/tokens/round-trip/classification
just eval-comprehend 40             # JSON-vs-TOON Q&A parity (max items)
just eval-report                    # report.md + summary.json

# equivalently, without just:
cd evals && cargo run --release -- all
```

Outputs land in `evals/results/`:

- `corpus.jsonl` — generated payloads + intended shape/format/domain
- `pipeline.jsonl` — per-item metrics
- `comprehension.jsonl` — per-question JSON/TOON answers + correctness
- `report.md`, `summary.json`

`pipeline` and `report` run without the server (token columns are simply left
empty); `generate` and `comprehend` require it.
