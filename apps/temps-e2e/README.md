# @temps-sdk/e2e

End-to-end + load testing CLI for a **live** Temps instance. Most commands
(`scenario`, `tls-scenario`, `email-scenario`, `kv-scenario`,
`audit-scenario`, `managed-services-scenario`, `rbac-scenario`,
`monitoring-scenario`, `error-tracking-scenario`, `logs-scenario`,
`analytics-scenario`, `session-replay-scenario`, `examples`) drive the real
control-plane API directly via the shared
[`@temps-sdk/api`](../../packages/api) client — fast, and enough to prove the
API itself works, but they never exercise `apps/temps-cli` at all.
`cli-scenario` is different on purpose: it spawns the **real
`@temps-sdk/cli` binary as a subprocess** for every step, so it also proves
argv parsing, Commander's command wiring, and stdout/`--json` formatting
actually work — exactly what breaks an agent running `bunx @temps-sdk/cli
...` even when the underlying API is fine. See its section below.

Every `*-scenario` command follows the same shape: build/deploy whatever
real infrastructure the feature needs (never synthetic DB rows), drive it
through the real HTTP surface, assert on genuine round-trip behavior (not
just 2xx), and tear everything down unless `--keep` is passed. Each has its
own "steps" section below documenting exactly what it proves and any real
platform bugs found and fixed while building it.

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

# kv-storage: real Redis-backed data-plane round trip (no console UI exists
# for this feature beyond an enabled/healthy badge).
bun run src/index.ts kv-scenario

# audit-logs: real PROJECT_CREATED/PROJECT_DELETED rows read back exactly,
# plus an RBAC-gate check on the read endpoint itself.
bun run src/index.ts audit-scenario

# managed-services: provision + link a postgres service to a project BEFORE
# deploying, deploy an app that writes through the injected POSTGRES_URL,
# verify an exact row-count round trip, then unlink.
bun run src/index.ts managed-services-scenario --registry localhost:5111

# RBAC/teams: a second, independently-authenticated low-privilege user
# escalated viewer -> deployer -> admin, asserting exact 200/403
# transitions and the audit trail at each tier. Needs DB-direct access.
bun run src/index.ts rbac-scenario --temps-root /path/to/temps --database-url postgres://...

# monitoring/status-page: auto-provisioned + explicit monitors, a real 5xx
# outage caught by the fixed 60s check cycle, incident lifecycle, recovery.
bun run src/index.ts monitoring-scenario --registry localhost:5111

# error-tracking (Sentry-compatible): real Sentry-shaped events authenticated
# via DSN key, fingerprint-based grouping proven live.
bun run src/index.ts error-tracking-scenario

# logs: real container stdout/stderr through the Docker log collector --
# full-text search, level filtering, JSONB fields passthrough, purge.
bun run src/index.ts logs-scenario --registry localhost:5111

# analytics: real visitor/session cookies issued by the proxy, replayed on
# the public ingest endpoint, custom event_data + session stitching proven.
bun run src/index.ts analytics-scenario --registry localhost:5111

# session-replay: real rrweb-shaped event batches (base64+zlib), ingest +
# playback + list visibility + manual duration override + soft delete.
bun run src/index.ts session-replay-scenario --registry localhost:5111
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

### `kv-scenario` steps

14-step round trip against the platform-wide kv-storage feature: enable KV,
create 2 projects, set/get, `incr` (sequential + fresh-key-defaults-to-1),
`keys` pattern exact-match, `nx`/`xx` conditional-write semantics, `ttl`
sentinels (-1 no-expiry / -2 missing-key), `expire`, `del` (exact count +
idempotent second call), cross-project isolation (the `kv:p{project_id}:`
namespace), a `kv_status` read-back, and a 400 on a missing `project_id`.

No console UI exists for this feature beyond an enabled/healthy status
badge, so this is the only coverage that exists. kv-storage is a
platform-wide singleton (one shared Redis container), not a per-project
resource — the scenario never disables it in teardown, since other
concurrent e2e runs or real users on a shared instance may depend on it
staying up.

### `audit-scenario` steps

Create + delete a project, then read back the exact `PROJECT_CREATED`/
`PROJECT_DELETED` rows via `GET /audit/logs` and `/audit/logs/{id}` —
id/data/actor/timestamp match, not just "the list is non-empty" — and
confirm the read endpoint itself is RBAC-gated (401/403 with no bearer
token).

### `managed-services-scenario` steps

1. build + push a throwaway Go probe app
2. provision a Postgres external service and link it to a project **before**
   deploying (the injected `POSTGRES_URL` only exists if the link happens
   first)
3. deploy the probe; it writes through the injected connection string on
   every `/probe` hit
4. verify an exact row-count round trip through repeated `/probe` calls
5. verify the resolved-env-vars reveal endpoint returns the real
   (non-masked) connection string
6. unlink the service and confirm it disappears from the resolved env vars

**Real bug found and fixed**: `scenario --with-db` created its Postgres
service with no parameters, but `PostgresParameterStrategy::validate_for_creation`
requires `database`/`username` with no defaults — every `--with-db` run was
400ing. Fixed alongside this scenario.

### `rbac-scenario` steps

Proves the actual permission **boundary**, not just that team/access CRUD
returns 2xx: a second, independently-authenticated low-privilege user is
granted team access to a project, then escalated viewer → deployer → admin,
asserting the exact 200/403 transitions and `required_permission` strings
the guard enforces at each tier, plus the exact audit trail
(`PROJECT_ACCESS_GRANTED` / `TEAM_MEMBER_ROLE_UPDATED` /
`PROJECT_ACCESS_REVOKED` / `TEAM_DELETED`).

Needs DB-direct access (`crates/temps-cli`'s own `api-key` subcommand, via
`--temps-root`/`--database-url`) to mint the second user's bearer key —
minting a new key while already authenticated via API key is deliberately
blocked (anti-privilege-escalation), and login only sets a session cookie.
Uses a second "guard" team (granted and never revoked) so that revoking the
primary team's grant at the end doesn't drop the project's total grant
count to zero, which would (correctly, per documented platform behavior)
reopen the project to everyone and defeat the revoke assertion for the
wrong reason.

**Real, pre-existing CLI bug found and fixed**: `apps/temps-cli`'s
`AVAILABLE_ROLES` offered `developer`/`viewer` as instance-wide user
roles — neither is valid; those are **team** roles, a separate concept.
Fixed to `admin`/`user`.

### `monitoring-scenario` steps

1. create a project — confirm its production environment auto-gets a
   default monitor (most projects never get an explicit one; this is the
   path real users actually depend on)
2. deploy a toggleable Go app (`lib/toggle-app.ts` — health flips over HTTP
   via `POST /toggle?state=up|down`, so it works identically against a
   remote instance)
3. create a second, explicit monitor via the CRUD API
4. confirm `MonitorCreated` triggers an immediate first check (not a 60s
   wait)
5. toggle the app down — wait for the next periodic check cycle (a fixed
   60s global interval, no per-monitor on-demand trigger) and assert
   current-status flips to `major_outage`, a real incident is created, the
   bucketed chart reflects the outage, and the project-level overview
   flips to `partial_outage`
6. toggle back up — wait for recovery and assert the incident auto-resolves
7. delete the explicit monitor; teardown cascades the auto-created one

**Four real platform bugs found and fixed** (see the four commits preceding
this scenario's own commit): `calculate_overall_status`/bucketed-status SQL
only recognized the literal `"down"`/`"degraded"` strings, not the
finer-grained `major_outage`/`partial_outage` `health_check_service.rs`
actually writes; `CurrentStatusQuery`/`UptimeQuery`/`BucketedQuery` declared
`start_time`/`end_time` as required despite documented defaults, so every
default-range call 400'd; the synthetic bootstrap "unknown" check row
counted toward every uptime denominator without ever counting as a success;
and two divergent `MonitorService` instances meant API-created monitors
skipped the immediate-check path the auto-provisioned ones got. Also fixed
the `AVG(response_time_ms)` Postgres `NUMERIC`→`f64` cast bug that made
`get_status_overview` silently report "unknown" for fully healthy monitors.

The incident-auto-resolve step polls rather than asserting once:
current-status reads `status_checks` directly and flips the instant a
check commits, but incident resolution is a side effect of the *same*
check processed asynchronously through the job queue, which can trail by
anywhere from a few ms to most of another 60s cycle under load.

### `error-tracking-scenario` steps

1. create a DSN (not auto-provisioned, unlike monitoring's auto-monitor)
2. send real Sentry-shaped events authenticated with the DSN's public key
   via `X-Sentry-Auth` — the one route in the platform using a different
   auth scheme than the normal bearer token
3. prove fingerprint-based grouping: an identical repeat groups into the
   same issue, a genuinely different exception creates its own issue
4. confirm the computed group title (`"{type}: {value}"`), event-detail
   round-tripping the stored exception data, error-stats splitting
   resolved/unresolved after a status update, and the DSN auth boundary (a
   garbage key gets a real 401)

New `lib/sentry-events.ts` (payload builder + raw sender) exists because
the ingest endpoint's generated SDK type (`SentryEventRequest`) only
declares `event_id`/`message`/`platform`/`timestamp` — the real handler
takes arbitrary JSON and expects a full exception/stacktrace shape, so the
OpenAPI schema for this one route is decorative only.

No platform bugs found — ran clean end-to-end on the first real attempt.
(One assertion fix on the test's own end: stored event data is wrapped
under a `sentry` key alongside a `source` discriminator, not at the top
level, since this crate ingests from multiple SDK sources.)

### `logs-scenario` steps

1. deploy a throwaway Go app (`lib/log-emitter-app.ts`) whose `/emit`
   endpoint prints a structured JSON line to stdout or stderr on demand —
   the only way to get real lines through the real Docker log collector
   instead of inserting synthetic rows into storage
2. emit an info-level and an error-level (with a `code` field) log line
3. poll full-text search for a run-unique marker — chunks flush on a
   30s/1MB timer, so a fresh line is not immediately queryable
4. assert both lines come back with the right computed `level` and that
   the error line's extra `code` field survived into the searchable
   `fields` JSONB
5. assert a `levels: ["ERROR"]` filter narrows to just the error line, and
   an `envs` filter on the production environment also narrows correctly
6. fetch grep-style context around the error line via `chunk_id` +
   `line_offset`
7. purge everything before "now"; confirm the marker is gone from search
   afterward

No platform bugs found — ran clean 3x back-to-back on the first attempt.
`GET /logs/tail` (SSE live-tail) is deliberately not covered: it needs the
exact Docker-label `service`/`env` strings the collector stamped on the
container, which no deploy response exposes.

### `analytics-scenario` steps

1. deploy a throwaway Go app (`lib/analytics-app.ts`) that serves a real
   `text/html` page at `/` — required because `should_track_page`
   (`temps-proxy`) only issues visitor/session cookies for HTML responses
   (or 4xx/5xx); a plain-text "ok" response never triggers cookie issuance
2. confirm a fresh project reports `has_events=false`
3. `GET /` through the proxy and capture the real `_temps_visitor_id`/
   `_temps_sid` `Set-Cookie` values the proxy issues — these are
   encrypted, proxy-minted tokens with no way to synthesize them client-side
4. `POST /api/_temps/event` twice with those same cookies (a pageview, then
   a custom event carrying extra fields) — the unauthenticated public
   ingest path a real browser tracking snippet uses
5. poll `GET /analytics/event-entries` for the custom event and assert its
   custom `event_data` round-tripped into the queryable `props` JSON, and
   that it resolved to a numeric `visitor_id` (the visitor/session upsert
   is a 500ms-batched background writer, so this genuinely polls)
6. call `GET /analytics/visitors/{id}/journey` and assert
   `total_sessions=1`, `total_events=2`, and both event names appear in the
   same session — proving the two independent POSTs were stitched into one
   session purely from the shared cookie pair
7. confirm `has_events` is now `true`

**Real gap found and fixed** (see `chore(sdk): regenerate @temps-sdk/api`):
`packages/api/openapi.json` — the source for the SDK this entire suite
depends on — had drifted to 590 of the live server's 674 paths. The whole
`temps-analytics` query surface used by this scenario
(`getEventEntries`/`checkAnalyticsHasEvents`/`getVisitorJourney`/etc.) was
unreachable from any TypeScript consumer until the SDK was regenerated from
a live spec. Not a Rust bug — every route was already fully wired and
working, just unreachable from generated clients. Also added the missing
`prettier` devDependency the regen's post-processor step needed.

Not covered: server-side ingestion (`POST /projects/{id}/events/ingest`,
used by app backends forwarding already-authenticated cookies) and the
segment-filter/attribution query surface (referrer/UTM/country
breakdowns) — both real, but better covered by the existing Rust unit
tests than by another live HTTP round trip.

### `session-replay-scenario` steps

1. deploy the same HTML app used by `analytics-scenario` and `GET /` to
   capture a real `_temps_visitor_id` cookie — `init_session_replay`
   requires one and 400s without it
2. `POST /api/_temps/session-replay/init` with a client-generated session
   id, carrying that cookie — this genuinely retries rather than asserting
   once: the visitor lookup races the proxy's own 500ms-batched background
   writer, so the very first attempt can hit a visitor row that isn't
   committed yet
3. `POST /api/_temps/session-replay/events` twice with base64+zlib
   -compressed rrweb-shaped event batches (3 events total, spanning 2000ms)
4. poll `GET /session-replays` (project list) for the new session — this
   list is filtered to `duration > 0`, only computed once events with
   distinct timestamps land
5. fetch the session's full event stream and assert all 3 events came back
   with a custom marker field intact, and that `duration` reflects the
   real timestamp span
6. confirm the visitor-scoped list also surfaces it (a second,
   independently-implemented read path)
7. `PUT` a manual duration override and confirm it sticks
8. `DELETE` the session (soft delete) and confirm it drops out of the
   project list afterward

**Two real bugs found and fixed**: the duration-override endpoint was
registered as `POST` in `configure_routes()` but documented (and SDK'd) as
`PUT` — any client following the documented contract got a 405. And
`delete_session_replay` correctly soft-deletes a session (`is_active =
false`), but none of the four read paths (`get_sessions_for_project`,
`get_sessions_for_visitor`, `get_session_replay`,
`get_session_replay_without_events`) ever checked the flag — a "deleted"
recording, potentially containing DOM mutations/keystrokes, stayed fully
visible in both list views and fetchable in full by ID. Both fixed;
confirmed live that a deleted session's direct-by-ID playback now 404s.

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

## Known gaps (not covered end-to-end)

**Slack notifications** — deliberately not covered by any scenario command,
and not expected to be: two independent guards stand between a real
`POST /notification-providers/slack` → `.../test` flow and any local target,
and neither should be worked around for test convenience.
`crates/temps_core::url_validation::validate_external_url` (referenced from
`crates/temps-notifications`) rejects loopback/private/link-local addresses
as a deliberate SSRF boundary, and even if that were bypassed,
`SlackProvider::initialize()` (`crates/temps-notifications/src/services.rs`)
separately hard-requires `webhook_url.starts_with("https://hooks.slack.com/")`
— a real Slack workspace is the only thing that satisfies both. `send()`
itself has no such gate and is covered by a `wiremock`-based Rust unit test
in the same file, which is the right layer for this: it proves the exact
payload shape (channel, attachments, mrkdwn-escaping) without needing a live
HTTP-driven e2e run. `vercel-labs/emulate` (used elsewhere in this repo for
mocking third-party APIs test-harness-side) can't help here either, for the
same reason — see `web/e2e/README.md`'s "Mocking third-party services"
section, which documents the identical constraint for the console UI tests.

## Notes

- The load engine is pure `fetch`, worker-pooled (exactly `--concurrency`
  in-flight). Transient connection failures are retried (`--connectRetries`
  equivalent); real HTTP 4xx/5xx are recorded as-is.
- Resources are name-prefixed `e2e-<runid>` so leftovers are identifiable.
