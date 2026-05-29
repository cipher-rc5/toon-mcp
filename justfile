# toon-mcp task runner. Run `just` to list recipes.

# Directory of the standalone evaluation harness (own cargo workspace).
evals_dir := "evals"

# llama-server connection used by the eval recipes (override on the CLI, e.g.
# `just eval-all base_url=http://localhost:9090 model=my-model`).
base_url := env_var_or_default("TOON_EVAL_BASE_URL", "http://localhost:8080")
model := env_var_or_default("TOON_EVAL_MODEL", "local-model")

_default:
    @just --list

# --- core workspace ---------------------------------------------------------

# Build the published workspace (release).
build:
    cargo build --release

# Run the full test suite.
test:
    cargo test --workspace

# Format + lint gate (matches CI).
lint:
    cargo fmt --all --check
    cargo clippy --workspace -- -D warnings

# --- model server -----------------------------------------------------------
# Wraps evals/serve.sh. All host-specific values (model path, port, ctx size,
# GPU layers) come from environment variables or evals/.env.eval — copy
# evals/.env.eval.example to get started. Nothing private is committed.

# Start llama-server in the background and wait until it reports ready.
serve:
    {{evals_dir}}/serve.sh start

# Run llama-server in the foreground (Ctrl-C to stop).
serve-fg:
    {{evals_dir}}/serve.sh foreground

# Stop the background server.
serve-stop:
    {{evals_dir}}/serve.sh stop

# Restart the background server.
serve-restart:
    {{evals_dir}}/serve.sh restart

# Report whether the server is running, with a health check.
serve-status:
    {{evals_dir}}/serve.sh status

# Follow the server log.
serve-logs:
    {{evals_dir}}/serve.sh logs

# --- evaluation harness -----------------------------------------------------
# These shell out to the standalone `evals/` crate. `generate` and `comprehend`
# need a running llama-server (`just serve`); `pipeline` and `report` do not
# (token columns are simply omitted when the server is unreachable).

_eval +args:
    cd {{evals_dir}} && TOON_EVAL_BASE_URL={{base_url}} TOON_EVAL_MODEL={{model}} \
        cargo run --release -- {{args}}

# Build the eval harness only.
eval-build:
    cd {{evals_dir}} && cargo build --release

# Generate a synthetic corpus with the model. `per_cell` payloads per matrix cell.
eval-generate per_cell="1":
    @just _eval generate --per-cell {{per_cell}}

# Score the corpus: bytes, exact tokens, round-trip fidelity, classification.
eval-pipeline:
    @just _eval pipeline

# JSON-vs-TOON comprehension parity. `max_items` caps the LLM call volume.
eval-comprehend max_items="40":
    @just _eval comprehend --max-items {{max_items}}

# Aggregate results into evals/results/{report.md,summary.json}.
eval-report:
    @just _eval report

# End-to-end: generate -> pipeline -> comprehend -> report.
eval-all per_cell="1" max_items="40":
    @just eval-generate {{per_cell}}
    @just eval-pipeline
    @just eval-comprehend {{max_items}}
    @just eval-report

# Remove generated eval data (keeps the harness source).
eval-clean:
    rm -rf {{evals_dir}}/results
