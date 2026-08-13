---
name: start-temps
description: >
  Start (or restart) a local Temps control plane built from this checkout, and
  the web dev server (`bun dev`) in `<checkout>/web`. Invoke when the user says
  "start temps", "restart temps", "launch the server", "kill and restart
  temps", or asks to bring the local server up after backend changes. Ports,
  database and data dir are allocated PER CHECKOUT (a "slot") so several
  worktrees/branches run side by side without killing each other or
  corrupting each other's schema — the first checkout you start in claims
  slot 0 (the familiar `:8080` / `:3000`); every other worktree gets its own
  slot and its own `temps_s<N>` database. Uses the `fast` cargo profile
  (release semantics, no debug symbols, parallel codegen) for quick rebuilds.
  Pass `split` (e.g. "start temps split", "/start-temps split") to launch the
  two-process proxy/console topology for testing that feature.
---

# Start a local Temps server

Boots the `temps` binary from source (`cargo run --bin temps --package
temps-cli -- serve ...`) for quick local iteration, no debugger required. See
`CONTRIBUTING.md` for the one-shot manual version of this; this skill adds
port isolation across worktrees and a repeatable restart flow.

## Prerequisites

- The database container from `CONTRIBUTING.md` ("Database" section) running
  and reachable, e.g.:

  ```bash
  docker run -d --name temps-db --restart unless-stopped \
    -p 127.0.0.1:5432:5432 \
    -v temps-db-data:/home/postgres/pgdata/data \
    -e POSTGRES_USER=temps -e POSTGRES_PASSWORD=temps -e POSTGRES_DB=temps \
    timescale/timescaledb-ha:pg18
  ```

  If your container has a different name, user, password, or port, set
  `TEMPS_DEV_DB_CONTAINER` / `TEMPS_DEV_DB_URL_BASE` before running step 0, or
  just edit the generated slot env file afterwards.
- `bun install` already run in `<checkout>/web`.
- Docker running, if you'll exercise features that shell out to it
  (container deploys, agent sandboxes, etc.) — see the Docker precheck below.

## Port slots — read this first

If you keep multiple worktrees of this repo around (one per branch/PR), the
naive approach — hardcode `:8080` / `:3000` and kill whatever's listening —
means starting temps in worktree B kills the server another session was using
in worktree A. This skill instead assigns each checkout a *slot* (0–29) and
derives every port from it:

| Thing | Port | Slot 0 |
|---|---|---|
| Backend HTTP (`--address`) | `8080 + slot*10` | 8080 |
| Console (`--console-address`) | `8081 + slot*10` | 8081 |
| TLS (`--tls-address`) | `8443 + slot*10` | 8443 |
| Parked address (split mode only) | `8085 + slot*10` | 8085 |
| Web dev server (`bun dev`) | `3000 + slot` | 3000 |
| Database | `temps_s<slot>` | `temps` |
| `TEMPS_DATA_DIR` | `<checkout>/crates/temps-cli/temps_data` | same, per checkout |

Rules:

- All listeners bind to `127.0.0.1` by default. LAN exposure is not part of
  this workflow; configure TLS and strong non-development credentials before
  opting into a non-loopback bind.
- The **first checkout** you run step 0 in claims slot 0, so the familiar
  `http://localhost:3000` / `:8080` belong to whichever worktree you started
  first (usually your primary clone).
- Every other checkout gets the lowest free slot ≥ 1, and **keeps it** — the
  claim is recorded atomically in `~/.temps-dev/slot-<N>.claim/` with its
  mode-0600 state in `slot-<N>.env`, so restarting temps in the same worktree
  always lands on the same ports. Existing claims remain
  reserved until explicitly released with step 7, even while their server is
  stopped.
- The kill step verifies the listening process's cwd is inside *this*
  checkout before killing it. If it isn't, **stop and report** rather than
  killing it — that process belongs to another session.
- The ranges never overlap (HTTP 8080–8370, parked 8085–8375, TLS 8443–8733,
  web 3000–3029). If you run other local services in the 8080–8730 or
  3000–3029 range, expect port contention and free a slot (step 7) or shift
  the base ports in this skill.

## Modes

- **Default (monolith)** — one `temps serve` process (`--role=all`, the
  single-binary control plane). This is what you get with a bare
  `/start-temps`. Use **Procedure** below.
- **`split`** — the two-process topology: a standalone `temps proxy`
  (Pingora data plane) **plus** a separate `temps serve --role=console`
  (Axum control plane). Use this to verify the proxy keeps serving while the
  console restarts. Trigger with "start temps split" / "/start-temps split".
  Use [references/split-mode.md](references/split-mode.md) instead of the
  default Procedure below.

## When to invoke

- "start temps" / "launch temps" / "bring up the server"
- "restart temps" / "kill and restart"
- "start temps split" / "test the proxy/console split" → [references/split-mode.md](references/split-mode.md)
- After backend changes that need the server reloaded
- Anywhere you want a local control plane up for the checkout you're in

## Procedure

### 0. Allocate this checkout's slot (ports, database name, data dir)

Run this **from inside the checkout you are working in** (the worktree, not
necessarily your primary clone). It is idempotent — rerunning it returns the
same slot. Run it verbatim; it prints the port map and writes
`~/.temps-dev/slot-<N>.env`, which every later step sources. The file is
created with mode `0600`, and every value is shell-escaped before writing so
checkout paths and overrides cannot become commands when the file is sourced.

```bash
set -u
umask 077
mkdir -p "$HOME/.temps-dev"

# Which checkout is this working in?
REPO=$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || pwd)
if [ -f "$REPO/Cargo.toml" ] && [ -d "$REPO/crates/temps-cli" ]; then TEMPS_ROOT="$REPO"
else echo "ERROR: no temps Rust workspace under $REPO — cd into a temps checkout first"; exit 1; fi

DB_CONTAINER="${TEMPS_DEV_DB_CONTAINER:-temps-db}"
DB_URL_BASE="${TEMPS_DEV_DB_URL_BASE:-postgres://temps:temps@localhost:5432}"

SLOT=""
TEMPS_ROOT_ID=$(printf '%s' "$TEMPS_ROOT" | shasum -a 256 | awk '{print $1}')
# 1. Reuse an existing claim for this checkout.
for f in "$HOME"/.temps-dev/slot-*.env; do
  [ -e "$f" ] || continue
  if grep -qxF "TEMPS_ROOT_ID=$TEMPS_ROOT_ID" "$f"; then
    SLOT=${f##*/slot-}; SLOT=${SLOT%.env}; break
  fi
done
# 2. Otherwise atomically claim the lowest slot (>= 0) that is unclaimed and
#    not listening. `mkdir` is the exclusion primitive: two concurrent agents
#    cannot both create the same claim directory.
#    Claims remain reserved until step 7 releases them. A stopped server or
#    temporarily unavailable checkout is not proof its migration DB is stale.
if [ -z "$SLOT" ]; then
  for i in $(seq 0 29); do
    f="$HOME/.temps-dev/slot-$i.env"
    claim="$HOME/.temps-dev/slot-$i.claim"
    { [ -e "$f" ] || [ -e "$claim" ]; } && continue
    busy=0
    for p in $((3000+i)) $((8080+i*10)) $((8081+i*10)) $((8443+i*10)); do
      lsof -nP -iTCP:$p -sTCP:LISTEN >/dev/null 2>&1 && busy=1
    done
    [ "$busy" -eq 1 ] && continue
    mkdir "$claim" 2>/dev/null || continue
    printf '%s\n' "$TEMPS_ROOT_ID" > "$claim/root-id"
    chmod 600 "$claim/root-id"
    SLOT=$i; CLAIM_DIR="$claim"; break
  done
fi
[ -z "$SLOT" ] && { echo "ERROR: no free slot in 0..29 — stop some servers first"; exit 1; }

if [ "$SLOT" = 0 ]; then
  DB_NAME=temps
else
  DB_NAME=temps_s$SLOT
fi
DATA_DIR="$TEMPS_ROOT/crates/temps-cli/temps_data"   # gitignored in every checkout

SLOT_FILE="$HOME/.temps-dev/slot-$SLOT.env"
{
  echo "# temps dev slot $SLOT — written by the start-temps skill. Delete to release."
  printf 'TEMPS_SLOT=%q\n' "$SLOT"
  printf 'TEMPS_ROOT_ID=%q\n' "$TEMPS_ROOT_ID"
  printf 'TEMPS_ROOT=%q\n' "$TEMPS_ROOT"
  printf 'TEMPS_HTTP_PORT=%q\n' "$((8080+SLOT*10))"
  printf 'TEMPS_CONSOLE_PORT=%q\n' "$((8081+SLOT*10))"
  printf 'TEMPS_TLS_PORT=%q\n' "$((8443+SLOT*10))"
  printf 'TEMPS_PARKED_PORT=%q\n' "$((8085+SLOT*10))"
  printf 'TEMPS_WEB_PORT=%q\n' "$((3000+SLOT))"
  printf 'TEMPS_DB_NAME=%q\n' "$DB_NAME"
  printf 'TEMPS_DB_CONTAINER=%q\n' "$DB_CONTAINER"
  printf 'TEMPS_DATABASE_URL=%q\n' "$DB_URL_BASE/$DB_NAME"
  printf 'TEMPS_DATA_DIR=%q\n' "$DATA_DIR"
  printf 'TEMPS_ADMIN_EMAIL=%q\n' 'dev@temps.sh'
  printf 'TEMPS_ADMIN_PASSWORD_FILE=%q\n' "$HOME/.temps-dev/slot-$SLOT.admin-password"
  printf 'TEMPS_SERVE_LOG=%q\n' "/tmp/temps-serve-s$SLOT.log"
  printf 'TEMPS_CONSOLE_LOG=%q\n' "/tmp/temps-console-s$SLOT.log"
  printf 'TEMPS_PROXY_LOG=%q\n' "/tmp/temps-proxy-s$SLOT.log"
  printf 'TEMPS_WEB_LOG=%q\n' "/tmp/temps-web-s$SLOT.log"
} > "$SLOT_FILE"
chmod 600 "$SLOT_FILE"
echo
echo "slot $SLOT -> api http://localhost:$((8080+SLOT*10))  web http://localhost:$((3000+SLOT))  db $DB_NAME"
echo "env file: $SLOT_FILE (mode 0600; contains the database URL — do not print it)"
```

**Substitute the printed slot number for `<N>` in every block below**, and
report the two URLs at the end — in a non-zero slot they are *not* the
familiar `:3000` / `:8080`.

### 0b. Provision this slot's database and data dir

Each non-zero slot runs against **its own database** (`temps_s<N>`) and **its
own `TEMPS_DATA_DIR`**, so a branch's migrations, encrypted rows, CAS blobs
and sandboxes can't corrupt another branch's. Slot 0 uses the plain `temps`
database from `CONTRIBUTING.md`.

Every slot starts from a **fresh, empty** database: the branch's own
migrations build the schema on first `serve`, and the admin user is created
non-interactively from `TEMPS_ADMIN_EMAIL` + `TEMPS_ADMIN_PASSWORD_FILE` (no
prompt to feed via stdin).

```bash
source "$HOME/.temps-dev/slot-<N>.env"
PSQL="docker exec -i $TEMPS_DB_CONTAINER psql -U temps"

if $PSQL -tAc "SELECT 1 FROM pg_database WHERE datname='$TEMPS_DB_NAME'" | grep -q 1; then
  echo "database $TEMPS_DB_NAME already exists"
else
  $PSQL -c "CREATE DATABASE \"$TEMPS_DB_NAME\"" && \
    echo "created empty $TEMPS_DB_NAME — migrations run on first serve"
fi

mkdir -p "$TEMPS_DATA_DIR"
if [ ! -f "$TEMPS_ADMIN_PASSWORD_FILE" ]; then
  { openssl rand -base64 24 | tr -d '\n'; printf '!Aa1\n'; } > "$TEMPS_ADMIN_PASSWORD_FILE"
  chmod 600 "$TEMPS_ADMIN_PASSWORD_FILE"
fi
echo "login: $TEMPS_ADMIN_EMAIL / ***"
echo "password file: $TEMPS_ADMIN_PASSWORD_FILE (mode 0600; do not print its contents)"
```

Report the email and password-file path, but never the password — a fresh slot
DB has no other account.
(Password rules: ≥8 chars with upper, lower, digit and a special character —
`validate_password_complexity` rejects anything weaker and `serve` will fail
to start.)

#### Docker precheck

If Docker is down, some plugin/sandbox initialization can fail silently while
the HTTP port still binds — the server *looks* up, but pieces that shell out
to Docker won't work and the reason can be buried in the log. Check first:

```bash
docker info >/dev/null 2>&1 && echo "docker ok" || \
  echo "DOCKER DOWN — start it before testing anything that provisions containers"
```

### 1. Kill only this slot's processes

Ownership-checked: it refuses to kill anything whose cwd is outside this
checkout. If it refuses, do **not** work around it with `kill -9` — another
session owns that process. Re-run step 0 after deleting this slot's env file
to get a different slot instead.

```bash
source "$HOME/.temps-dev/slot-<N>.env"

for spec in "$TEMPS_HTTP_PORT backend" "$TEMPS_CONSOLE_PORT console" "$TEMPS_WEB_PORT web"; do
  set -- $spec; PORT=$1; LABEL=$2
  for PID in $(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null); do
    CWD=$(lsof -a -p "$PID" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1)
    case "$CWD" in
      "$TEMPS_ROOT"|"$TEMPS_ROOT"/*) ;;
      *) echo "REFUSING to kill pid $PID on :$PORT ($LABEL) — cwd '$CWD' is outside $TEMPS_ROOT"; continue ;;
    esac
    kill "$PID" 2>/dev/null; sleep 2
    kill -0 "$PID" 2>/dev/null && { kill -9 "$PID"; sleep 1; }
  done
  lsof -nP -iTCP:$PORT -sTCP:LISTEN 2>/dev/null || echo "port $PORT free ($LABEL)"
done
```

> Never use a bare `pkill -f temps` / `pkill -f cargo` — it takes down every
> other checkout's server too.

### 2. Launch the server

`--profile fast` gives release-grade runtime speed with much faster link
times than `--release` (see Notes). `TEMPS_ADMIN_EMAIL` +
`TEMPS_ADMIN_PASSWORD_FILE` make the first-run admin bootstrap
non-interactive — without them a fresh database wedges a detached process in
an infinite email/password re-prompt loop.

```bash
source "$HOME/.temps-dev/slot-<N>.env"

cd "$TEMPS_ROOT/crates/temps-cli" && \
  RUST_BACKTRACE=full \
  TEMPS_LOG_FORMAT=full \
  TEMPS_DATA_DIR="$TEMPS_DATA_DIR" \
  TEMPS_ADMIN_EMAIL="$TEMPS_ADMIN_EMAIL" \
  TEMPS_ADMIN_PASSWORD_FILE="$TEMPS_ADMIN_PASSWORD_FILE" \
  TEMPS_DISABLE_HTTPS_REDIRECT=true \
  TEMPS_TELEMETRY=0 \
  nohup cargo run --profile fast --bin temps --package temps-cli -- \
    serve \
    --disable-https-redirect \
    --database-url="$TEMPS_DATABASE_URL" \
    --address=127.0.0.1:$TEMPS_HTTP_PORT \
    --tls-address=127.0.0.1:$TEMPS_TLS_PORT \
    --console-address=127.0.0.1:$TEMPS_CONSOLE_PORT \
    --log-level=debug \
    > "$TEMPS_SERVE_LOG" 2>&1 & disown
echo "serve launched (slot $TEMPS_SLOT) -> $TEMPS_SERVE_LOG"
```

(If you're testing multi-node worker join locally, add
`--private-address=<your LAN IP>` — workers use it to reach this control
plane's services.)

Run this **detached** (`nohup ... & disown`, not a backgrounded foreground
job) so the process survives past this command finishing.

### 3. Launch the `web` dev server

`rsbuild.config.ts` proxies `/api` to `TEMPS_API_TARGET || http://localhost:8080`
— so **any non-zero slot must set `TEMPS_API_TARGET`**, otherwise the SPA
talks to whatever server owns :8080 instead of yours.

```bash
source "$HOME/.temps-dev/slot-<N>.env"

cd "$TEMPS_ROOT/web" && \
  TEMPS_API_TARGET="http://localhost:$TEMPS_HTTP_PORT" \
  nohup bun dev --port $TEMPS_WEB_PORT > "$TEMPS_WEB_LOG" 2>&1 & disown
```

Rsbuild typically reports `ready built in <1s` and the listener appears
within a few seconds. Verify:

```bash
source "$HOME/.temps-dev/slot-<N>.env"
for i in $(seq 1 12); do
  sleep 1
  if lsof -nP -iTCP:$TEMPS_WEB_PORT -sTCP:LISTEN >/dev/null 2>&1; then
    echo "web ready after ${i}s on http://localhost:$TEMPS_WEB_PORT"
    break
  fi
done
lsof -nP -iTCP:$TEMPS_WEB_PORT -sTCP:LISTEN >/dev/null 2>&1 || tail -30 "$TEMPS_WEB_LOG"
```

If the web port doesn't come up, `tail -30 "$TEMPS_WEB_LOG"` — usually a
missing `node_modules` (run `bun install` in `<checkout>/web` first) or a
port collision.

### 4. Wait for the backend listener

Poll the slot's ports (build can take 30s–5min depending on cache state).
Don't sleep in a single long block — short polls so readiness can be
reported promptly.

**Check the console port, not just the HTTP port.** In `--role=all` the proxy
binds `$TEMPS_HTTP_PORT` even when console startup fails, so an HTTP listener
alone is not proof the server works — you can get a proxy that 503s every
request with no console behind it.

```bash
source "$HOME/.temps-dev/slot-<N>.env"
for i in $(seq 1 30); do
  sleep 10
  if lsof -nP -iTCP:$TEMPS_CONSOLE_PORT -sTCP:LISTEN >/dev/null 2>&1; then
    echo "ready after $((i*10))s — api http://localhost:$TEMPS_HTTP_PORT console :$TEMPS_CONSOLE_PORT"
    curl -s -o /dev/null -w "console /readyz -> %{http_code}\n" http://localhost:$TEMPS_CONSOLE_PORT/readyz
    exit 0
  fi
done
echo "console never bound :$TEMPS_CONSOLE_PORT after 5min"
sed 's/\x1b\[[0-9;]*m//g' "$TEMPS_SERVE_LOG" | grep -iE "FAILED|failed to start|Plugin registration failed" | head -5
```

A fresh slot DB should also log
`Initial admin created from TEMPS_ADMIN_EMAIL and password secret file` — if
instead you see the "Welcome to Temps!" banner, the env vars didn't reach the
process and it is now wedged on a prompt.

### 5. Report the URLs

Always finish by reporting the actual URLs for this slot — they differ per
worktree — plus the login, which for a fresh slot DB is a brand-new account
that exists nowhere else:

```
slot <N>: web http://localhost:<3000+N>   api http://localhost:<8080+N*10>
          db temps_s<N>   login dev@temps.sh / ***
          password file ~/.temps-dev/slot-<N>.admin-password
```

### 6. After-restart housekeeping (optional)

If you changed backend types/handlers, regenerate the web SDK:

```bash
source "$HOME/.temps-dev/slot-<N>.env"
cd "$TEMPS_ROOT/web" && bun run openapi-ts
```

Regenerate after every backend restart that changes the OpenAPI surface.

### 7. Releasing a slot

When a worktree is deleted, or you want to free ports/disk. Confirm before
running — this destroys that slot's data. Slot 0 uses the shared `temps`
database from `CONTRIBUTING.md`, so releasing slot 0 only clears its data
dir, not the database.

```bash
source "$HOME/.temps-dev/slot-<N>.env"
EXPECTED_ROOT=$(git -C "$TEMPS_ROOT" rev-parse --show-toplevel 2>/dev/null) || {
  echo "REFUSING cleanup: TEMPS_ROOT is not a Git checkout"; exit 1;
}
EXPECTED_DATA_DIR="$EXPECTED_ROOT/crates/temps-cli/temps_data"
EXPECTED_PASSWORD_FILE="$HOME/.temps-dev/slot-$TEMPS_SLOT.admin-password"
EXPECTED_CLAIM_DIR="$HOME/.temps-dev/slot-$TEMPS_SLOT.claim"
[ "$TEMPS_ROOT" = "$EXPECTED_ROOT" ] && [ -f "$TEMPS_ROOT/Cargo.toml" ] && \
  [ -d "$TEMPS_ROOT/crates/temps-cli" ] || {
  echo "REFUSING cleanup: invalid Temps workspace root"; exit 1;
}
[ "$TEMPS_DATA_DIR" = "$EXPECTED_DATA_DIR" ] || {
  echo "REFUSING cleanup: data dir is outside the expected checkout path"; exit 1;
}
[ "$TEMPS_ADMIN_PASSWORD_FILE" = "$EXPECTED_PASSWORD_FILE" ] || {
  echo "REFUSING cleanup: password file is outside the expected slot path"; exit 1;
}
[ -f "$EXPECTED_CLAIM_DIR/root-id" ] && \
  grep -qxF "$TEMPS_ROOT_ID" "$EXPECTED_CLAIM_DIR/root-id" || {
  echo "REFUSING cleanup: slot claim is missing or belongs to another checkout"; exit 1;
}
case "$TEMPS_SLOT" in
  0) EXPECTED_DB_NAME=temps ;;
  [1-9]|[1-2][0-9]) EXPECTED_DB_NAME="temps_s$TEMPS_SLOT" ;;
  *) echo "REFUSING cleanup: slot must be an integer in 0..29"; exit 1 ;;
esac
[ "$TEMPS_DB_NAME" = "$EXPECTED_DB_NAME" ] || {
  echo "REFUSING cleanup: database does not match the validated slot"; exit 1;
}
if [ "$TEMPS_SLOT" != 0 ]; then
  docker exec -i "$TEMPS_DB_CONTAINER" psql -U temps -c "DROP DATABASE IF EXISTS \"$TEMPS_DB_NAME\" WITH (FORCE)"
fi
rm -rf -- "$TEMPS_DATA_DIR"
rm -f "$TEMPS_ADMIN_PASSWORD_FILE" "$HOME/.temps-dev/slot-$TEMPS_SLOT.env"
rm -f "$EXPECTED_CLAIM_DIR/root-id"
rmdir "$EXPECTED_CLAIM_DIR"
echo "slot $TEMPS_SLOT released"
```

Claims are not automatically reclaimed: a stopped server or temporarily
unavailable checkout is not proof that its migration database is abandoned.
Run step 7 before deleting a worktree. If one is already gone, inspect its
mode-0600 claim file, confirm the exact checkout/slot/database with the user,
and clean it up manually. List candidate databases with:

```bash
docker exec -i "${TEMPS_DEV_DB_CONTAINER:-temps-db}" psql -U temps -c \
  "SELECT datname, pg_size_pretty(pg_database_size(datname)) FROM pg_database WHERE datname LIKE 'temps_s%' ORDER BY 1"
```

---

## Split-mode procedure (`/start-temps split`)

Testing the ADR-017 two-process proxy/console topology instead of the default
monolith is a separate, less-common workflow — see
[references/split-mode.md](references/split-mode.md) for the full procedure
(build, launch console + proxy, wait for both listeners, verify independence).
It reuses this file's step 0 (slot allocation) and step 1 (ownership-checked
kill) first.

## Notes

- **Profile**: `fast` is defined in the workspace root `Cargo.toml`. Inherits
  release; `codegen-units = 16`, `debug = false`, `strip = "symbols"`,
  `lto = false`, `incremental = true`. ~30–60s incremental rebuild after
  small changes.
- **Why not `--release`**: full release with `codegen-units = 1` and default
  symbols takes ~5–10min for a clean build. `fast` is the everyday default.
- **Why not debug**: the debug build is meant for attaching a debugger.
  Without one attached, runtime is meaningfully slower for no benefit.
- **Isolation boundaries.** A slot owns its ports, its database
  (`temps_s<N>`), and its `TEMPS_DATA_DIR` (encryption key, CAS blobs,
  sandboxes, stacks, plugin data). Branches with different migration sets
  don't fight over one schema. What is still shared: the Postgres *instance*
  (disk, connections, `shared_buffers`), the Docker daemon, and any container
  names/host ports the branch's own deployments allocate.
- **Fresh-DB first run**: with `TEMPS_ADMIN_EMAIL` + `TEMPS_ADMIN_PASSWORD_FILE`
  set (step 0b writes both), bootstrap is non-interactive. Without them
  `serve` prompts for the admin email, prints a generated password, and asks
  "Have you saved the password?" — a detached process then wedges in an
  infinite re-prompt loop and floods the log. If you ever do need the
  interactive path, feed it stdin:
  `printf 'you@example.com\ny\n' | temps serve ...`.
- **Log files**: per slot — `/tmp/temps-serve-s<N>.log`,
  `/tmp/temps-web-s<N>.log`, `/tmp/temps-console-s<N>.log`,
  `/tmp/temps-proxy-s<N>.log`. Tail with `tail -f "$TEMPS_SERVE_LOG"` after
  sourcing the slot env.
- **Web port**: the backend does not serve the SPA in dev — the rsbuild dev
  server does, and it proxies `/api` to `TEMPS_API_TARGET`.
- **Override**: if you want the original debug-symbol build (to attach lldb
  later), swap `--profile fast` for plain `cargo run` (no `--profile`).
