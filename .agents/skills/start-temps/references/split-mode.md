# Split-mode procedure (`/start-temps split`)

Launches the split topology — **two** backend processes from the **one**
`temps` binary — so you can verify the proxy keeps serving while the console
restarts, that the proxy falls back unknown hosts to the console, and that
the health gates behave. Same slot/port rules as the default procedure in
[../SKILL.md](../SKILL.md).

| Process | Command | Binds | Log |
|---|---|---|---|
| Proxy (data plane) | `temps proxy` | `$TEMPS_HTTP_PORT`, `$TEMPS_TLS_PORT` | `$TEMPS_PROXY_LOG` |
| Console (control plane) | `temps serve --role=console` | `$TEMPS_CONSOLE_PORT` | `$TEMPS_CONSOLE_LOG` |
| Web dev | `bun dev` | `$TEMPS_WEB_PORT` | `$TEMPS_WEB_LOG` |

The proxy forwards every **unmatched** host to `--console-address`, and
`temps proxy` **errors at startup if `--console-address` is missing** — it
must know where the sibling console lives.

## 1. Allocate the slot, then kill only this slot's processes

Run **step 0** from the main procedure, then the ownership-checked kill from
**step 1** (it covers all three listeners). Do not use `pkill -f`: command-line
matching treats checkout paths as regular expressions and can kill another
worktree's process. If an orphaned process has no listener, identify its PID
manually and apply the same canonical cwd-under-`$TEMPS_ROOT` check from step 1
before stopping it.

## 2. Build once, then launch both from the same binary

Both processes share the freshly built `fast`-profile binary. Build first so
the two `nohup` launches don't race two concurrent `cargo` builds against the
same `target/` (which can deadlock the build lock):

```bash
source "$HOME/.temps-dev/slot-<N>.env"
cd "$TEMPS_ROOT" && \
  cargo build --profile fast --bin temps --package temps-cli 2>&1 | tail -5
ls -la "$TEMPS_ROOT/target/fast/temps"   # confirm it exists before launching
```

## 3. Launch the console (`--role=console`)

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
    --console-address=127.0.0.1:$TEMPS_CONSOLE_PORT \
    --log-level=debug \
    > "$TEMPS_CONSOLE_LOG" 2>&1 & disown
echo "console launched, pid $!"
```

> `--address=127.0.0.1:$TEMPS_PARKED_PORT` is a parked, unused value: in
> `--role=console` the process does **not** bind the proxy listener, but the
> flag is still required by the parser. The parked series (`8085 + slot*10`)
> never collides with any other slot's HTTP/console/TLS port.

## 4. Launch the standalone proxy, pointed at the console

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
    --address=127.0.0.1:$TEMPS_HTTP_PORT \
    --tls-address=127.0.0.1:$TEMPS_TLS_PORT \
    --console-address=127.0.0.1:$TEMPS_CONSOLE_PORT \
    --log-level=debug \
    > "$TEMPS_PROXY_LOG" 2>&1 & disown
echo "proxy launched, pid $!"
```

## 5. Launch the web dev server

Same as the monolith path (step 3 in the main procedure) — `TEMPS_API_TARGET`
still points at `$TEMPS_HTTP_PORT`, which in split mode is the proxy.

## 6. Wait for both listeners

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

## 7. Verify the split actually works

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

## Split-mode notes

- Both processes hit **this slot's** database and `TEMPS_DATA_DIR` (shared
  `encryption_key` / `auth_secret` — they MUST match or cookie/crypto
  breaks).
- Route changes propagate proxy↔console via PG `NOTIFY` (~100–400 ms), vs
  <5 ms in-process in the monolith — a brief lag after deploys in split mode
  is normal.
- To go back to the monolith, kill both split processes (step 1) and run the
  default procedure in [../SKILL.md](../SKILL.md).
