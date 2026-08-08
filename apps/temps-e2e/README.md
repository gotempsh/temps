# @temps-sdk/e2e

End-to-end + load testing CLI for a **live** Temps instance. Most commands
(`scenario`, `tls-scenario`, `email-scenario`, `examples`) drive the real
control-plane API directly via the shared
[`@temps-sdk/api`](../../packages/api) client — fast, and enough to prove the
API itself works, but they never exercise `apps/temps-cli` at all.
`cli-scenario` is different on purpose: it spawns the **real
`@temps-sdk/cli` binary as a subprocess** for every step, so it also proves
argv parsing, Commander's command wiring, and stdout/`--json` formatting
actually work — exactly what breaks an agent running `bunx @temps-sdk/cli
...` even when the underlying API is fine. See its section below.

## Setup

```bash
cd apps/temps-e2e
bun install
```

The tool depends on `@temps-sdk/api` via a local link. If the link isn't set up:

```bash
cd ../../packages/api && bun install && bun run build && bun link
cd ../../apps/temps-e2e && bun install
```

## Auth

Point it at any instance with `--url` / `--api-key`, or the `TEMPS_URL` /
`TEMPS_API_KEY` env vars (default URL: `http://localhost:8080`).

Mint a key against a **local** instance directly from the server binary:

```bash
temps api-key \
  --database-url=postgres://postgres:password@localhost:5432/temps_development \
  --name=e2e --role=admin --user-email=you@example.com --output-format=json
```

## Commands

```bash
# Verify connectivity + auth
bun run src/index.ts ping

# Generate load against any URL (no Temps deploy required)
bun run src/index.ts load https://example.com -n 10000 -c 100
bun run src/index.ts load https://example.com -d 60s -c 200      # by duration
#   -H "Host: app.localho.st"   to route through a proxy by host header

# Full lifecycle: project -> deploy image -> wait healthy -> load -> verify -> teardown
bun run src/index.ts scenario --image traefik/whoami:latest -n 2000 -c 50
bun run src/index.ts scenario --with-db                          # also provision postgres
bun run src/index.ts scenario --keep                            # leave resources up
bun run src/index.ts scenario --json                            # machine-readable (CI)

# Build the repo's example projects (Go, Python, Node, …) and run the full
# deploy/verify lifecycle for EACH — proves every example in examples/ actually
# deploys to a live Temps and serves real traffic (not just a prebuilt image).
#
# Requires Docker + a registry the Temps server can pull from. Start a local one:
#   docker run -d -p 5111:5000 --name temps-e2e-registry registry:2
#   export TEMPS_E2E_REGISTRY=localhost:5111
bun run src/index.ts examples --list                            # show registered examples
bun run src/index.ts examples                                   # fast subset: go-gin, python-flask, node-nestjs
bun run src/index.ts examples --only go-gin python-flask        # pick specific ones
bun run src/index.ts examples --all                             # every example (incl. heavy rust/vite builds)
bun run src/index.ts examples --registry localhost:5111         # registry (or $TEMPS_E2E_REGISTRY)
bun run src/index.ts examples --json                            # machine-readable (CI)

# Deploy an app, provision a real TLS certificate via Pebble (HTTP-01), and
# verify it's actually served. Needs the dedicated Pebble instance — see
# "External-service test infra" below.
bun run src/index.ts tls-scenario --image traefik/whoami:latest --port 80
bun run src/index.ts tls-scenario --keep --json                 # inspect the issued cert/domain after

# Create an SMTP provider pointed at Mailpit, send a tracked email, and verify
# real receipt + open/click tracking. Needs Mailpit (docker-compose.e2e.yml).
bun run src/index.ts email-scenario
bun run src/index.ts email-scenario --json                      # machine-readable (CI)

# Genuine CLI e2e: spawns the real @temps-sdk/cli binary as a subprocess for
# every step against the same live instance.
bun run src/index.ts cli-scenario
bun run src/index.ts cli-scenario --image traefik/whoami:latest --json  # machine-readable (CI)
```

### `examples`

Verifies that the source projects under the repo's `examples/` tree actually
deploy and serve on a live Temps. Each registered example (`src/lib/examples.ts`)
carries its source path, a minimal generated Dockerfile, its listen port and a
health path. For every selected example the command:

1. renders the Dockerfile into a scratch build context, `docker build`s it, and
   **pushes it to `$TEMPS_E2E_REGISTRY`** (Temps deploys by pulling, so the image
   must live somewhere the server can reach — same path a real user follows),
2. runs the full `scenario` lifecycle against the pushed image (create project →
   deploy → wait healthy → **assert the real app responds, not the Temps console
   fallback** → load → verify proxy logs → teardown),
3. prints a per-example PASS/FAIL summary and exits non-zero if any fails.

The build context is a scratch copy — the repo's `examples/` tree is never
mutated. Only HTTP-serving examples are registered; `node/vercel-ai-tracing` is
excluded (it's a one-shot OTel script needing LLM keys, not a server).

Verified passing (`--all`, 5/5): Go (Gin), Python (Flask), Node (NestJS),
Vite (React via nginx-unprivileged), Rust (Axum).

### `scenario` steps

1. create a project (`docker_image` source)
2. *(optional `--with-db`)* provision a Postgres external service
3. resolve the production environment
4. deploy a prebuilt public image
5. wait for the deployment to reach a terminal state
6. probe HTTP until the app actually serves (routes via the proxy origin with
   the app's `Host` header — no external DNS/TLS dependency)
7. warm up, then run the measured load test
8. verify the proxy-log count for the host is non-zero
9. tear down the deployment (stops the container + removes the route) and delete
   the project — even on failure, unless `--keep`

Exits non-zero if any step fails, so it's CI-gateable.

### `tls-scenario` steps

1. deploy an app the normal way (reuses the `scenario` deploy path)
2. register the host's IP with pebble-challtestsrv so Pebble's HTTP-01
   validation request actually reaches this machine
3. create a throwaway custom domain + a standalone TLS-certificate record for
   it, linked together
4. finalize the ACME order — a real HTTP-01 exchange against Pebble
5. dial the instance's TLS listener directly (SNI = the test domain) and
   assert both that the real app answered (not the console fallback) *and*
   that the served certificate's issuer is Pebble's test root, not a real CA
6. tear everything down (domain, custom-domain route, deployment, project)
   unless `--keep`

Verified passing (3x back-to-back, `--image traefik/whoami:latest`): real
project → real deploy → real ACME HTTP-01 exchange with Pebble → real HTTPS
fetch of the deployed app → issuer parsed from the actually-served
certificate and confirmed as Pebble's test root.

The default `--image` (`ghcr.io/temps-sh/e2e-hello:latest`, matching
`scenario`'s default) may be blocked by local registry/firewall policy on
some machines — `traefik/whoami:latest` is a reliable public substitute with
no registry-auth requirements.

**Bun gotcha found while verifying this live**: `tls.TLSSocket.getPeerCertificate()`
must be called right after the handshake completes (in the `connect`
callback), not after the response finishes (in the `'end'` handler) — under
Bun, calling it from `'end'` returns `{}` even though the exact same call
made earlier in the connection's lifetime (or at either point under real
Node.js) returns the full certificate, including `issuer`. `fetchOverTls` in
`src/lib/flows.ts` captures the certificate at connect time for this reason.

### `email-scenario` steps

1. create an SMTP email provider pointed at Mailpit
2. register + verify a throwaway sending domain against it (SMTP domains
   verify immediately)
3. send a real email with open + click tracking enabled
4. confirm actual receipt via Mailpit's own REST API — `send_email` silently
   falls back to a "captured, never sent" status on any provider/domain
   problem and still returns 201, so this is the only real proof
5. simulate a recipient opening the email and clicking its one tracked link,
   and confirm both landed as real `email_events` rows
6. tear everything down (provider, domain) unless `--keep`

Verified passing (3x back-to-back): real SMTP send → real receipt in Mailpit
→ real open/click tracking-pixel and redirect hits → real `email_events` rows.

### `cli-scenario` steps

Every step spawns `bun run <apps/temps-cli>/src/index.ts <args>` as a real
child process (`src/lib/cli-exec.ts`) against a live instance — same source
Commander registers as the published `bunx @temps-sdk/cli`, so this is
genuinely testing the CLI binary, not the API:

1. `projects create --manual -y` a project, resolve its slug via
   `projects list --json`, confirm `projects show --json` reflects it
2. `environments vars set/list/get/export/delete` an env var — `get`'s
   assertion is a regression test for a real bug this suite found and fixed
   (see below)
3. `services create -t postgres -y`, resolve via `services list --json`,
   confirm `services show --json` reflects it
4. `deploy:image --no-wait`, resolve the deployment via
   `deployments list --json`, poll `deployments status --json` through the
   CLI itself to a terminal state
5. `domains add`, confirm it via `domains list --json`
6. confirm `apikeys list`/`apikeys permissions --json` are reachable under
   API-key auth (see below for why this doesn't also mint a new key)
7. tear everything down via the CLI's own delete/remove commands (falling
   back to the SDK-based teardown from `flows.ts` for anything the CLI
   itself fails to remove) unless `--keep`

Verified passing (3x back-to-back, `--image traefik/whoami:latest`).

**Two real, pre-existing bugs found live while building this:**
- **FIXED**: `environments vars get` used to read from the list endpoint,
  which the server masks to the literal string `"***"` for **every**
  variable regardless of `is_secret` — so a command whose entire point is
  showing one variable's value never actually showed it. Fixed in
  `apps/temps-cli/src/commands/environments/index.ts` to resolve real
  values through the same audited per-key endpoint `vars export` already
  used (`getEnvironmentVariableValue`,
  `GET /projects/{id}/env-vars/{key}/value`) — secrets are still correctly
  withheld. `cli-scenario`'s "vars get" step is now a regression test for
  this fix. (`vars list --show-values` still shows `"***"` for everything —
  that one reads from the bulk list endpoint on purpose, per its own code
  comment, to prevent a bulk read from ever becoming a credential dump; only
  its help text — "Use `--show-values` to reveal actual values" — is a
  little misleading, since it never actually does for anyone.)
- **NOT FIXED** (out of scope — needs tracing the certificate-listing
  query, not a CLI change): `domains status` 404s on a domain that hasn't
  finished ACME provisioning yet — i.e. exactly the domain state you'd
  actually want to check status on. Root cause: `check_domain_status`
  resolves via `list_certificates(CertificateFilter::default())`, which
  only returns domains that already have an issued certificate row.

**One real, correct security boundary discovered live** (not a bug): minting
a new API key is rejected when authenticated via an API key ("This
authentication method cannot perform the requested sensitive action",
`crates/temps-auth/src/sensitive_action.rs`) — a deliberate
anti-privilege-escalation guard. Since API-key auth is this CLI's only
non-interactive auth mode, `apikeys create` is structurally untestable from
a pure CLI e2e run; the scenario exercises the read paths that are reachable
under API-key auth instead.

## External-service test infra (TLS, DNS, email)

`tls-scenario` and `email-scenario` need real external services to test
against — a live ACME CA to issue a real certificate, an SMTP target that
actually receives mail. Hitting the real internet for these (real Let's
Encrypt, a real inbox) would mean real rate limits, real credentials, and
non-reproducible runs, so both point at local, purpose-built test servers
instead:

```bash
cd apps/temps-e2e
docker compose -f docker-compose.e2e.yml up -d
```

This starts:
- **Pebble** (`ghcr.io/letsencrypt/pebble`) — Let's Encrypt's own ACME v2 test
  server. Directory on `https://localhost:14000/dir`, self-signed test root,
  no rate limits, validates for real (a genuine HTTP request against
  whatever answers the challenge).
- **pebble-challtestsrv** (`ghcr.io/letsencrypt/pebble-challtestsrv`) — the
  companion mock-DNS server Pebble uses to resolve where to send HTTP-01/
  DNS-01 validation requests. Management API on `http://localhost:8056`
  (not its own default of 8055 — see the compose file comment for why).
- **Mailpit** (`axllent/mailpit`) — SMTP catcher. SMTP on `localhost:1025`,
  web UI + REST API (`GET /api/v1/messages`) on `http://localhost:8025`. Same
  image already proven for this in `crates/temps-notifications`'s Rust
  integration tests.

The `temps serve` instance under test needs these env vars to actually talk
to Pebble instead of real Let's Encrypt (already-existing hooks in
`crates/temps-domains/src/tls/providers.rs` — nothing new to build there):

```bash
ACME_DIRECTORY_URL=https://localhost:14000/dir
ACME_INSECURE=1                      # trust Pebble's self-signed test root
TEMPS_ALLOW_PEBBLE_PROVIDER=1        # unlock the Pebble DNS-01 provider
```

**`tls-scenario` needs a dedicated instance on a fixed port, not a normal dev
slot.** Pebble's baked-in config sends HTTP-01 validation requests to port
`5002` (its own default `httpPort`, chosen so Pebble never needs root/
privileged-port access) — so the target `temps serve` process's proxy must
be listening on `--address=0.0.0.0:5002` for Pebble's validation request to
land on temps' real HTTP-01 challenge responder. This is unrelated to the
`start-temps` skill's per-checkout slot ports; run a one-off instance for
this specific scenario rather than trying to fit it into the slot scheme.

Because Pebble and pebble-challtestsrv run in Docker while `temps serve`
runs natively on the host, `tls-scenario`'s setup step resolves
`host.docker.internal` (from a throwaway container on the
`docker-compose.e2e.yml` network) and registers it with pebble-challtestsrv
via `POST /set-default-ipv4` before provisioning, so Pebble's validation
request actually reaches the host.

## Notes

- The load engine is pure `fetch`, worker-pooled (exactly `--concurrency`
  in-flight). Transient connection failures are retried (`--connectRetries`
  equivalent); real HTTP 4xx/5xx are recorded as-is.
- Resources are name-prefixed `e2e-<runid>` so leftovers are identifiable.
