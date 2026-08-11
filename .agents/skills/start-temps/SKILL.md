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

- The **first checkout** you run step 0 in claims slot 0, so the familiar
  `http://localhost:3000` / `:8080` belong to whichever worktree you started
  first (usually your primary clone).
- Every other checkout gets the lowest free slot ≥ 1, and **keeps it** — the
  claim is recorded in `~/.temps-dev/slot-<N>.env`, so restarting temps in the
  same worktree always lands on the same ports.
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
  Use **Split-mode procedure** below instead of the default Procedure.

## When to invoke

- "start temps" / "launch temps" / "bring up the server"
- "restart temps" / "kill and restart"
- "start temps split" / "test the proxy/console split" → **Split-mode procedure**
- After backend changes that need the server reloaded
- Anywhere you want a local control plane up for the checkout you're in

## Procedure

### 0. Allocate this checkout's slot (ports, database name, data dir)

Run this **from inside the checkout you are working in** (the worktree, not
necessarily your primary clone). It is idempotent — rerunning it returns the
same slot. Run it verbatim; it prints the port map and writes
`~/.temps-dev/slot-<N>.env`, which every later step sources.

```bash
set -u
mkdir -p "$HOME/.temps-dev"

# Which checkout is this working in?
REPO=$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || pwd)
if [ -f "$REPO/Cargo.toml" ] && [ -d "$REPO/crates/temps-cli" ]; then TEMPS_ROOT="$REPO"
else echo "ERROR: no temps Rust workspace under $REPO — cd into a temps checkout first"; exit 1; fi

DB_CONTAINER="${TEMPS_DEV_DB_CONTAINER:-temps-db}"
DB_URL_BASE="${TEMPS_DEV_DB_URL_BASE:-postgres://temps:temps@localhost:5432}"

SLOT=""
# 1. Reuse an existing claim for this checkout.
for f in "$HOME"/.temps-dev/slot-*.env; do
  [ -e "$f" ] || continue
  if grep -qxF "TEMPS_ROOT=\"$TEMPS_ROOT\"" "$f"; then
    SLOT=${f##*/slot-}; SLOT=${SLOT%.env}; break
  fi
done
# 2. Otherwise take the lowest slot (>= 0) that is neither claimed by a live
#    checkout nor currently listening.
if [ -z "$SLOT" ]; then
  for i in $(seq 0 29); do
    f="$HOME/.temps-dev/slot-$i.env"
    if [ -e "$f" ]; then
      owner=$(sed -n 's/^TEMPS_ROOT="\(.*\)"$/\1/p' "$f")
      [ -n "$owner" ] && [ -d "$owner" ] && continue   # live claim, hands off
    fi
    busy=0
    for p in $((3000+i)) $((8080+i*10)) $((8081+i*10)) $((8443+i*10)); do
      lsof -nP -iTCP:$p -sTCP:LISTEN >/dev/null 2>&1 && busy=1
    done
    [ "$busy" -eq 1 ] && continue
    SLOT=$i; break
  done
fi
[ -z "$SLOT" ] && { echo "ERROR: no free slot in 0..29 — stop some servers first"; exit 1; }

if [ "$SLOT" = 0 ]; then
  DB_NAME=temps
else
  DB_NAME=temps_s$SLOT
fi
DATA_DIR="$TEMPS_ROOT/crates/temps-cli/temps_data"   # gitignored in every checkout

cat > "$HOME/.temps-dev/slot-$SLOT.env" <<EOF
# temps dev slot $SLOT — written by the start-temps skill. Delete to release.
TEMPS_SLOT=$SLOT
TEMPS_ROOT="$TEMPS_ROOT"
TEMPS_HTTP_PORT=$((8080+SLOT*10))
TEMPS_CONSOLE_PORT=$((8081+SLOT*10))
TEMPS_TLS_PORT=$((8443+SLOT*10))
TEMPS_PARKED_PORT=$((8085+SLOT*10))
TEMPS_WEB_PORT=$((3000+SLOT))
TEMPS_DB_NAME=$DB_NAME
TEMPS_DB_CONTAINER="$DB_CONTAINER"
TEMPS_DATABASE_URL=$DB_URL_BASE/$DB_NAME
TEMPS_DATA_DIR="$DATA_DIR"
TEMPS_ADMIN_EMAIL=dev@temps.sh
TEMPS_ADMIN_PASSWORD_FILE=$HOME/.temps-dev/slot-$SLOT.admin-password
TEMPS_SERVE_LOG=/tmp/temps-serve-s$SLOT.log
TEMPS_CONSOLE_LOG=/tmp/temps-console-s$SLOT.log
TEMPS_PROXY_LOG=/tmp/temps-proxy-s$SLOT.log
TEMPS_WEB_LOG=/tmp/temps-web-s$SLOT.log
EOF

cat "$HOME/.temps-dev/slot-$SLOT.env"
echo
echo "slot $SLOT -> api http://localhost:$((8080+SLOT*10))  web http://localhost:$((3000+SLOT))  db $DB_NAME"
echo "env file: $HOME/.temps-dev/slot-$SLOT.env"
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
  printf 'TempsDev!%s\n' "$TEMPS_SLOT" > "$TEMPS_ADMIN_PASSWORD_FILE"
  chmod 600 "$TEMPS_ADMIN_PASSWORD_FILE"
fi
echo "login: $TEMPS_ADMIN_EMAIL / $(cat "$TEMPS_ADMIN_PASSWORD_FILE")"
```

Report those credentials to the user — a fresh slot DB has no other account.
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

If a debugger session (lldb / codelldb / debugserver) is attached to *this*
checkout's binary:

```bash
source "$HOME/.temps-dev/slot-<N>.env"
pkill -f "debugserver.*$TEMPS_ROOT" 2>/dev/null
pkill -f "codelldb.*$TEMPS_ROOT" 2>/dev/null
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
    --address=0.0.0.0:$TEMPS_HTTP_PORT \
    --tls-address=0.0.0.0:$TEMPS_TLS_PORT \
    --console-address=0.0.0.0:$TEMPS_CONSOLE_PORT \
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
          db temps_s<N>   login dev@temps.sh / TempsDev!<N>
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
if [ "$TEMPS_SLOT" != 0 ]; then
  docker exec -i "$TEMPS_DB_CONTAINER" psql -U temps -c "DROP DATABASE IF EXISTS \"$TEMPS_DB_NAME\" WITH (FORCE)"
fi
rm -rf "$TEMPS_DATA_DIR"
rm -f "$TEMPS_ADMIN_PASSWORD_FILE" "$HOME/.temps-dev/slot-$TEMPS_SLOT.env"
echo "slot $TEMPS_SLOT released"
```

Stale *port* claims whose checkout directory no longer exists are reclaimed
automatically by step 0, but their databases are not — so a deleted worktree
leaves a small stray database behind until you run the above. List the
strays with:

```bash
docker exec -i "${TEMPS_DEV_DB_CONTAINER:-temps-db}" psql -U temps -c \
  "SELECT datname, pg_size_pretty(pg_database_size(datname)) FROM pg_database WHERE datname LIKE 'temps_s%' ORDER BY 1"
```

---

## Split-mode procedure (`/start-temps split`)

Launches the split topology — **two** backend processes from the **one**
`temps` binary — so you can verify the proxy keeps serving while the console
restarts, that the proxy falls back unknown hosts to the console, and that
the health gates behave. Same slot/port rules as above.

| Process | Command | Binds | Log |
|---|---|---|---|
| Proxy (data plane) | `temps proxy` | `$TEMPS_HTTP_PORT`, `$TEMPS_TLS_PORT` | `$TEMPS_PROXY_LOG` |
| Console (control plane) | `temps serve --role=console` | `$TEMPS_CONSOLE_PORT` | `$TEMPS_CONSOLE_LOG` |
| Web dev | `bun dev` | `$TEMPS_WEB_PORT` | `$TEMPS_WEB_LOG` |

The proxy forwards every **unmatched** host to `--console-address`, and
`temps proxy` **errors at startup if `--console-address` is missing** — it
must know where the sibling console lives.

### 1. Allocate the slot, then kill only this slot's processes

Run **step 0** above, then the ownership-checked kill from **step 1** (it
already covers all three ports). Additionally clear prior split processes
started from *this* checkout only:

```bash
source "$HOME/.temps-dev/slot-<N>.env"
pkill -f "$TEMPS_ROOT/target/.*temps proxy" 2>/dev/null
pkill -f "$TEMPS_ROOT/target/.*temps serve --role" 2>/dev/null
```

### 2. Build once, then launch both from the same binary

Both processes share the freshly built `fast`-profile binary. Build first so
the two `nohup` launches don't race two concurrent `cargo` builds against the
same `target/` (which can deadlock the build lock):

```bash
source "$HOME/.temps-dev/slot-<N>.env"
cd "$TEMPS_ROOT" && \
  cargo build --profile fast --bin temps --package temps-cli 2>&1 | tail -5
ls -la "$TEMPS_ROOT/target/fast/temps"   # confirm it exists before launching
```

### 3. Launch the console (`--role=console`)

A stable `--console-address` is **required** in this role. Launch detached
(`nohup ... & disown`, no `setsid` on macOS):

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
  nohup "$TEMPS_ROOT/target/fast/temps" \
    serve \
    --role=console \
    --disable-https-redirect \
    --database-url="$TEMPS_DATABASE_URL" \
    --address=127.0.0.1:$TEMPS_PARKED_PORT \
    --console-address=0.0.0.0:$TEMPS_CONSOLE_PORT \
    --log-level=debug \
    > "$TEMPS_CONSOLE_LOG" 2>&1 & disown
echo "console launched, pid $!"
```

> `--address=127.0.0.1:$TEMPS_PARKED_PORT` is a parked, unused value: in
> `--role=console` the process does **not** bind the proxy listener, but the
> flag is still required by the parser. The parked series (`8085 + slot*10`)
> never collides with any other slot's HTTP/console/TLS port.

### 4. Launch the standalone proxy, pointed at the console

```bash
source "$HOME/.temps-dev/slot-<N>.env"

cd "$TEMPS_ROOT/crates/temps-cli" && \
  RUST_BACKTRACE=full \
  TEMPS_LOG_FORMAT=full \
  TEMPS_DATA_DIR="$TEMPS_DATA_DIR" \
  TEMPS_DISABLE_HTTPS_REDIRECT=true \
  TEMPS_TELEMETRY=0 \
  nohup "$TEMPS_ROOT/target/fast/temps" \
    proxy \
    --disable-https-redirect \
    --database-url="$TEMPS_DATABASE_URL" \
    --address=0.0.0.0:$TEMPS_HTTP_PORT \
    --tls-address=0.0.0.0:$TEMPS_TLS_PORT \
    --console-address=127.0.0.1:$TEMPS_CONSOLE_PORT \
    --log-level=debug \
    > "$TEMPS_PROXY_LOG" 2>&1 & disown
echo "proxy launched, pid $!"
```

### 5. Launch the web dev server

Same as the monolith path (step 3 above) — `TEMPS_API_TARGET` still points at
`$TEMPS_HTTP_PORT`, which in split mode is the proxy.

### 6. Wait for both listeners

```bash
source "$HOME/.temps-dev/slot-<N>.env"
for spec in "$TEMPS_CONSOLE_PORT console $TEMPS_CONSOLE_LOG" "$TEMPS_HTTP_PORT proxy $TEMPS_PROXY_LOG"; do
  set -- $spec; PORT=$1; NAME=$2; LOG=$3
  for i in $(seq 1 30); do
    sleep 5
    if lsof -nP -iTCP:$PORT -sTCP:LISTEN >/dev/null 2>&1; then
      echo "$NAME ready after $((i*5))s on :$PORT"; break
    fi
  done
  lsof -nP -iTCP:$PORT -sTCP:LISTEN >/dev/null 2>&1 || \
    { echo "$NAME NOT up"; tail -25 "$LOG"; }
done
```

### 7. Verify the split actually works

```bash
source "$HOME/.temps-dev/slot-<N>.env"

# Console health gates (served by the console process):
curl -s -o /dev/null -w "console /healthz -> %{http_code}\n" http://localhost:$TEMPS_CONSOLE_PORT/healthz
curl -s -o /dev/null -w "console /readyz  -> %{http_code}\n" http://localhost:$TEMPS_CONSOLE_PORT/readyz   # 503 until plugins init, then 200

# Console admin API directly:
curl -s -o /dev/null -w "console /api/projects -> %{http_code}\n" http://localhost:$TEMPS_CONSOLE_PORT/api/projects  # 401 unauth (expected) or 200

# Proxy serving + unmatched-host fallback to console SPA (should be 200, NOT 502):
curl -s -o /dev/null -w "proxy / (fallback) -> %{http_code}\n" http://localhost:$TEMPS_HTTP_PORT/

# The smoking gun — independence: restart ONLY the console, proxy keeps serving.
CONSOLE_PID=$(lsof -nP -iTCP:$TEMPS_CONSOLE_PORT -sTCP:LISTEN -t | head -1)
kill "$CONSOLE_PID"; sleep 1
curl -s -o /dev/null -w "proxy / while console DOWN -> %{http_code}\n" http://localhost:$TEMPS_HTTP_PORT/  # proxy process itself stays up (may 502 the body)
# ...then relaunch the console (step 3) and confirm /healthz returns 200 again.
```

What "passing" looks like:
- console `/healthz` → **200**, `/readyz` → **200** once plugins finish.
- proxy `/` → **200** via the console fallback (`grep "routing to console" "$TEMPS_PROXY_LOG"` confirms the fallback path fired).
- Killing the console does **not** kill the proxy process (its listener stays bound); it recovers once the console is back.
- `grep -i "version skew" "$TEMPS_PROXY_LOG"` — should be silent when both halves are the same binary.

### Split-mode notes

- Both processes hit **this slot's** database and `TEMPS_DATA_DIR` (shared
  `encryption_key` / `auth_secret` — they MUST match or cookie/crypto
  breaks).
- Route changes propagate proxy↔console via PG `NOTIFY` (~100–400 ms), vs
  <5 ms in-process in the monolith — a brief lag after deploys in split mode
  is normal.
- To go back to the monolith, kill both split processes (step 1) and run the
  default **Procedure**.

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
