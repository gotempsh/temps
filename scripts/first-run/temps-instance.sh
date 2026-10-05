#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Start a brand-new Temps instance the way a first-time operator does: the
# server binary, an empty database, and the initial admin created from the
# one-shot bootstrap variables (TEMPS_ADMIN_EMAIL + TEMPS_ADMIN_PASSWORD_FILE).
# No `temps setup`, no direct database writes: the API key the suites use is
# minted by logging in as that admin over HTTP, exactly like the console.
#
# Usage:
#   temps-instance.sh start     start `temps serve` in the background, wait for it
#   temps-instance.sh run       exec `temps serve` in the foreground (supervisors)
#   temps-instance.sh wait      wait until the instance answers HTTP
#   temps-instance.sh mint-key  log in as the bootstrap admin, print a new API key
#   temps-instance.sh stop      stop a server started with `start`
#
# Environment:
#   TEMPS_BIN            path to the temps binary                  (required)
#   DATABASE_URL         empty Postgres/TimescaleDB database       (required)
#   FIRST_RUN_DIR        work dir for data, log, pid, secrets      (/tmp/temps-first-run)
#   FIRST_RUN_ADDRESS    HTTP proxy + API listen address           (127.0.0.1:8760)
#   FIRST_RUN_CONSOLE    console listen address                    (127.0.0.1:8761)
#   FIRST_RUN_TLS        TLS listen address                        (127.0.0.1:8763)
#   ADMIN_EMAIL          bootstrap admin email                     (admin@localho.st)
#   ADMIN_PASSWORD       bootstrap admin password                  (generated, kept in FIRST_RUN_DIR)
#   TEMPS_LOG_LEVEL      server log level                          (info)

set -euo pipefail

FIRST_RUN_DIR="${FIRST_RUN_DIR:-/tmp/temps-first-run}"
FIRST_RUN_ADDRESS="${FIRST_RUN_ADDRESS:-127.0.0.1:8760}"
FIRST_RUN_CONSOLE="${FIRST_RUN_CONSOLE:-127.0.0.1:8761}"
FIRST_RUN_TLS="${FIRST_RUN_TLS:-127.0.0.1:8763}"
ADMIN_EMAIL="${ADMIN_EMAIL:-admin@localho.st}"
LOG_FILE="$FIRST_RUN_DIR/temps.log"
PID_FILE="$FIRST_RUN_DIR/temps.pid"
PASSWORD_FILE="$FIRST_RUN_DIR/admin-password"

die() {
  echo "temps-instance: $*" >&2
  exit 1
}

# 127.0.0.1:8760 -> http://127.0.0.1:8760 (0.0.0.0 is not dialable).
url_for() {
  local address="$1"
  echo "http://${address/0.0.0.0/127.0.0.1}"
}

prepare() {
  [[ -n "${TEMPS_BIN:-}" && -x "$TEMPS_BIN" ]] || die "TEMPS_BIN must point to an executable temps binary (got '${TEMPS_BIN:-}')"
  [[ -n "${DATABASE_URL:-}" ]] || die "DATABASE_URL must point to an empty database"
  mkdir -p "$FIRST_RUN_DIR/data"
  chmod 700 "$FIRST_RUN_DIR"
  if [[ ! -s "$PASSWORD_FILE" ]]; then
    # Meets the complexity rules: upper, lower, digit, symbol, length.
    local password="${ADMIN_PASSWORD:-FirstRun-$(openssl rand -hex 12)-Aa1!}"
    (umask 077 && printf '%s' "$password" > "$PASSWORD_FILE")
  fi
}

serve_args() {
  printf '%s\n' serve \
    --database-url "$DATABASE_URL" \
    --data-dir "$FIRST_RUN_DIR/data" \
    --address "$FIRST_RUN_ADDRESS" \
    --console-address "$FIRST_RUN_CONSOLE" \
    --tls-address "$FIRST_RUN_TLS" \
    --disable-https-redirect \
    --screenshot-provider noop
}

serve_env() {
  export TEMPS_ADMIN_EMAIL="$ADMIN_EMAIL"
  export TEMPS_ADMIN_PASSWORD_FILE="$PASSWORD_FILE"
  # `full` puts the module path on every line; the quiet-logs check groups
  # WARNs by it. NO_COLOR keeps the file free of escape codes.
  export TEMPS_LOG_FORMAT=full
  export TEMPS_LOG_LEVEL="${TEMPS_LOG_LEVEL:-info}"
  export NO_COLOR=1
  export TEMPS_TELEMETRY=0
}

cmd_run() {
  prepare
  serve_env
  local args=()
  while IFS= read -r arg; do args+=("$arg"); done < <(serve_args)
  exec "$TEMPS_BIN" "${args[@]}" >> "$LOG_FILE" 2>&1
}

cmd_start() {
  prepare
  if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    die "a server is already running with pid $(cat "$PID_FILE")"
  fi
  (
    serve_env
    local args=()
    while IFS= read -r arg; do args+=("$arg"); done < <(serve_args)
    nohup "$TEMPS_BIN" "${args[@]}" >> "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"
  )
  echo "temps serve started (pid $(cat "$PID_FILE")), log: $LOG_FILE"
  cmd_wait
}

cmd_wait() {
  local url deadline
  url="$(url_for "$FIRST_RUN_ADDRESS")"
  deadline=$((SECONDS + ${FIRST_RUN_START_TIMEOUT:-300}))
  until curl -sf -o /dev/null "$url/api/health" || curl -sf -o /dev/null "$url/"; do
    if [[ -f "$PID_FILE" ]] && ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
      tail -n 80 "$LOG_FILE" >&2 || true
      die "temps serve exited during startup (log above)"
    fi
    if (( SECONDS > deadline )); then
      tail -n 80 "$LOG_FILE" >&2 || true
      die "temps did not answer on $url within ${FIRST_RUN_START_TIMEOUT:-300}s"
    fi
    sleep 2
  done
  echo "temps is answering on $url"
}

cmd_mint_key() {
  [[ -s "$PASSWORD_FILE" ]] || die "no bootstrap password at $PASSWORD_FILE; start the instance first"
  local url jar body key
  url="$(url_for "$FIRST_RUN_ADDRESS")"
  jar="$(mktemp)"
  trap 'rm -f "$jar"' RETURN
  body="$(jq -n --arg email "$ADMIN_EMAIL" --rawfile password "$PASSWORD_FILE" '{email: $email, password: $password}')"
  if ! curl -sf -c "$jar" -H 'Content-Type: application/json' -d "$body" "$url/api/auth/login" > /dev/null; then
    die "login as bootstrap admin $ADMIN_EMAIL failed at $url/api/auth/login (did TEMPS_ADMIN_EMAIL/TEMPS_ADMIN_PASSWORD_FILE create the admin? see $LOG_FILE)"
  fi
  key="$(curl -sf -b "$jar" -H 'Content-Type: application/json' \
    -d "{\"name\":\"first-run-$(date +%s)\",\"role_type\":\"admin\"}" \
    "$url/api/api-keys" | jq -r '.api_key // empty')" || true
  [[ -n "$key" ]] || die "creating an API key with the admin session failed at $url/api/api-keys"
  printf '%s\n' "$key"
}

cmd_stop() {
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid="$(cat "$PID_FILE")"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$PID_FILE"
    echo "temps serve (pid $pid) stopped"
  fi
}

case "${1:-}" in
  start) cmd_start ;;
  run) cmd_run ;;
  wait) cmd_wait ;;
  mint-key) cmd_mint_key ;;
  stop) cmd_stop ;;
  *) die "usage: $0 start|run|wait|mint-key|stop" ;;
esac
