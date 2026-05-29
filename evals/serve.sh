#!/usr/bin/env bash
# evals/serve.sh — manage a local llama.cpp `llama-server` for the eval harness.
#
# Every host-specific value comes from an environment variable, so nothing
# private is committed. Put your values in evals/.env.eval (gitignored) — copy
# .env.eval.example to start — or export them in your shell:
#
#   TOON_EVAL_MODEL_PATH    (required) absolute path to the .gguf model
#   TOON_EVAL_PORT          (default 8080)
#   TOON_EVAL_HOST          (default 127.0.0.1 — local only)
#   TOON_EVAL_CTX_SIZE      (default 8192; raise to 32768 if the model supports it)
#   TOON_EVAL_NGL           (default 99; GPU layers — lower if VRAM-limited)
#   TOON_EVAL_LLAMA_BIN     (default llama-server)
#   TOON_EVAL_EXTRA_ARGS    (optional) extra flags appended to the launch verbatim
#   TOON_EVAL_START_TIMEOUT (default 180) seconds to wait for readiness
#
# Usage: serve.sh {start|stop|restart|status|logs|wait|foreground}

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Load optional private env file (model path, port, …). Never committed.
if [[ -f "$here/.env.eval" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$here/.env.eval"
  set +a
fi

MODEL_PATH="${TOON_EVAL_MODEL_PATH:-}"
PORT="${TOON_EVAL_PORT:-8080}"
HOST="${TOON_EVAL_HOST:-127.0.0.1}"
CTX_SIZE="${TOON_EVAL_CTX_SIZE:-8192}"
NGL="${TOON_EVAL_NGL:-99}"
LLAMA_BIN="${TOON_EVAL_LLAMA_BIN:-llama-server}"
EXTRA_ARGS="${TOON_EVAL_EXTRA_ARGS:-}"
START_TIMEOUT="${TOON_EVAL_START_TIMEOUT:-180}"

run_dir="$here/results"
LOG_FILE="${TOON_EVAL_LOG_FILE:-$run_dir/llama-server.log}"
PID_FILE="${TOON_EVAL_PID_FILE:-$run_dir/llama-server.pid}"
BASE_URL="http://$HOST:$PORT"

log() { printf '[serve] %s\n' "$*" >&2; }
err() { printf '[serve] error: %s\n' "$*" >&2; }
die() {
  err "$*"
  exit 1
}

is_running() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

health_ok() {
  command -v curl >/dev/null 2>&1 || return 1
  curl -fsS -m 2 "$BASE_URL/health" >/dev/null 2>&1
}

port_in_use() {
  command -v lsof >/dev/null 2>&1 || return 1
  lsof -ti "tcp:$PORT" >/dev/null 2>&1
}

require_model() {
  [[ -n "$MODEL_PATH" ]] ||
    die "TOON_EVAL_MODEL_PATH is not set. Copy $here/.env.eval.example to .env.eval and fill it in."
  [[ -f "$MODEL_PATH" ]] || die "model file not found: $MODEL_PATH"
}

wait_ready() {
  local timeout="${1:-$START_TIMEOUT}" start now
  start="$(date +%s)"
  log "waiting for $BASE_URL/health (timeout ${timeout}s) …"
  while true; do
    if health_ok; then
      log "ready at $BASE_URL"
      return 0
    fi
    if [[ -f "$PID_FILE" ]] && ! is_running; then
      err "server process exited during startup — see $LOG_FILE"
      return 1
    fi
    now="$(date +%s)"
    if ((now - start >= timeout)); then
      err "timed out after ${timeout}s waiting for readiness"
      return 1
    fi
    sleep 1
  done
}

cmd_start() {
  require_model
  if is_running; then
    log "already running (pid $(cat "$PID_FILE")) at $BASE_URL"
    return 0
  fi
  command -v "$LLAMA_BIN" >/dev/null 2>&1 || die "$LLAMA_BIN not found on PATH"
  port_in_use && die "port $PORT is already in use by another process"
  mkdir -p "$run_dir"
  log "starting $LLAMA_BIN on $BASE_URL (ctx=$CTX_SIZE, ngl=$NGL)"
  # shellcheck disable=SC2086
  nohup "$LLAMA_BIN" \
    -m "$MODEL_PATH" \
    --host "$HOST" \
    --port "$PORT" \
    --ctx-size "$CTX_SIZE" \
    -ngl "$NGL" \
    $EXTRA_ARGS \
    >"$LOG_FILE" 2>&1 &
  echo $! >"$PID_FILE"
  log "pid $(cat "$PID_FILE"); logs → $LOG_FILE"
  wait_ready "$START_TIMEOUT"
}

cmd_foreground() {
  require_model
  command -v "$LLAMA_BIN" >/dev/null 2>&1 || die "$LLAMA_BIN not found on PATH"
  port_in_use && die "port $PORT is already in use by another process"
  log "running $LLAMA_BIN in foreground on $BASE_URL (Ctrl-C to stop)"
  # shellcheck disable=SC2086
  exec "$LLAMA_BIN" \
    -m "$MODEL_PATH" \
    --host "$HOST" \
    --port "$PORT" \
    --ctx-size "$CTX_SIZE" \
    -ngl "$NGL" \
    $EXTRA_ARGS
}

cmd_stop() {
  if ! is_running; then
    log "not running"
    rm -f "$PID_FILE"
    return 0
  fi
  local pid
  pid="$(cat "$PID_FILE")"
  log "stopping pid $pid …"
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    is_running || break
    sleep 0.5
  done
  if is_running; then
    log "graceful stop timed out; force-killing pid $pid"
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$PID_FILE"
  log "stopped"
}

cmd_status() {
  if is_running; then
    log "running (pid $(cat "$PID_FILE")) at $BASE_URL"
    if health_ok; then
      log "health: ok"
    else
      log "health: not ready (still loading the model?)"
    fi
  else
    log "not running"
    return 1
  fi
}

cmd_logs() {
  [[ -f "$LOG_FILE" ]] || die "no log file yet at $LOG_FILE (has the server been started?)"
  exec tail -n "${1:-50}" -f "$LOG_FILE"
}

usage() {
  # Print the header comment block (skip the shebang), stop at the first
  # non-comment line so it never leaks code.
  awk 'NR==1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart)
    cmd_stop
    cmd_start
    ;;
  status) cmd_status ;;
  logs)
    shift
    cmd_logs "${1:-50}"
    ;;
  wait)
    shift
    wait_ready "${1:-$START_TIMEOUT}"
    ;;
  foreground | fg) cmd_foreground ;;
  "" | -h | --help | help) usage ;;
  *)
    err "unknown command: $1"
    usage
    exit 1
    ;;
esac
