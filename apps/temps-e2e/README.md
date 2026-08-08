# @temps-sdk/e2e

End-to-end + load testing CLI for a **live** Temps instance. Most commands
(`scenario`, `tls-scenario`, `dns01-wildcard-scenario`, `email-scenario`,
`kv-scenario`, `blob-scenario`, `flags-scenario`, `audit-scenario`,
`managed-services-scenario`, `rbac-scenario`, `monitoring-scenario`,
`error-tracking-scenario`, `logs-scenario`, `analytics-scenario`,
`session-replay-scenario`, `backup-restore-scenario`, `pitr-scenario`,
`git-deploy-scenario`, `examples`) drive the real
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

# Register a Pebble DNS provider, issue a wildcard certificate via a real
# DNS-01 challenge, and verify it. Needs the same Pebble/pebble-challtestsrv
# infra as tls-scenario, but no dedicated fixed-port instance.
bun run src/index.ts dns01-wildcard-scenario
bun run src/index.ts dns01-wildcard-scenario --keep --json      # inspect the issued cert/domain after

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

# blob storage: real RustFS (S3-compatible) data-plane round trip --
# put/download/head/list/copy/delete, deleted-blob 404, cross-project isolation.
bun run src/index.ts blob-scenario

# feature flags: driven through the real @temps-sdk/node-sdk FlagsClient --
# defaults, per-environment overrides, the kill switch, ETag/304, exposure reporting.
bun run src/index.ts flags-scenario

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

# backup-restore: real postgres service backed up to a local MinIO (real
# wal-g backup-push, real S3 upload), then in-place restored, proven by
# reverting writes made after the backup. Needs MinIO (docker-compose.e2e.yml).
bun run src/index.ts backup-restore-scenario --registry localhost:5111

# pitr: point-in-time recovery of a real postgres service via MinIO -- write
# rows, wait for their WAL to archive, capture a recovery-target timestamp,
# write MORE rows, PITR-restore to the captured timestamp, and prove via the
# read-only data-browser API (not /probe) that exactly the pre-target rows
# survive. Needs MinIO (docker-compose.e2e.yml). Runs ~90s of real wall-clock
# wait time so a WAL segment actually archives -- see the steps section below.
bun run src/index.ts pitr-scenario --registry localhost:5111

# git-deploy: real clone + build of a public GitHub repo
# (github.com/gotempsh/temps-examples) via trigger-pipeline -- proves the
# actual git pipeline, not an image pull or a synthesized Dockerfile. Hits
# real github.com (documented exception to this suite's "no real internet"
# rule -- see "External-service test infra" below).
bun run src/index.ts git-deploy-scenario
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

### `dns01-wildcard-scenario` steps

1. register a Pebble-backed DNS provider and add + verify a managed zone for
   it — verification against Pebble always succeeds (`get_zone` is
   unconditionally `Some`), so this never blocks on real DNS propagation
2. create a wildcard domain (`*.<zone>`) with a `dns-01` challenge, which
   auto-requests a real ACME order from Pebble
3. push the order's TXT record(s) to the Pebble provider via `setup-dns`
4. finalize the order — Pebble's real validator queries pebble-challtestsrv
   (its own `-dnsserver`) for the TXT record and only issues if it actually
   finds the value just pushed in step 3
5. parse the returned certificate PEM directly (no live app to fetch — a bare
   wildcard domain isn't routed to any project) and assert the issuer is
   Pebble's test root and the SAN covers the exact wildcard
6. tear everything down (domain, managed zone, DNS provider) unless `--keep`

Verified passing (3x back-to-back): real DNS provider → real managed zone →
real ACME DNS-01 order → real TXT records pushed to pebble-challtestsrv →
real validation → certificate parsed and confirmed to cover the wildcard,
issued by Pebble's test root.

**Closes a real coverage gap, no code bug found**: `crates/temps-domains`'
own DNS-01 test (`test_dns01_wildcard_with_pebble`) is `#[ignore]`d and sets
`PEBBLE_VA_ALWAYS_VALID=1`, which skips Pebble's real validator entirely —
so nothing before this scenario proved the actual DNS-01 pipeline (TXT
record push → real DNS query → real validation → real issuance) works
end-to-end. This scenario does not set that env var.

Unlike `tls-scenario`, this needs no host-IP registration with
pebble-challtestsrv and no dedicated fixed-port instance: DNS-01 validation
never dials the host machine at all, only pebble-challtestsrv's DNS
interface, which Pebble already points at via its own `-dnsserver` flag —
any normal dev-slot instance works, as long as it's launched with
`ACME_DIRECTORY_URL` / `ACME_INSECURE` / `TEMPS_ALLOW_PEBBLE_PROVIDER=1`.

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

### `blob-scenario` steps

12-step round trip against the platform-wide blob-storage feature (RustFS,
S3-compatible — same shared-singleton shape as kv-storage): enable blob
storage, create 2 projects, `put` (exact metadata echoed back), `download`
(exact byte-for-byte content + Content-Type match), `head` (Content-Length/
Content-Type headers, no body), `list` (prefix-scoped, exact match), `copy`
(byte-identical to the source), `delete` (exact count + idempotent second
call), a deleted blob is actually gone (404 on direct HEAD, not just absent
from list), cross-project isolation (identical pathname, different
project_id, non-crossing content), and a 400 on a missing `project_id`.

**Three real bugs found and fixed while building this**:

1. `blob_put`'s `#[utoipa::path]` never declared its query params
   (`pathname`, `content_type`, `add_random_suffix`, `project_id`) at all,
   and `blob_list`'s declared params were missing `project_id` — both
   existed on the Rust query-extractor structs but were invisible to the
   OpenAPI spec, so the generated SDK typed them as `query?: never` /
   omitted `project_id` entirely. No client could actually call these with
   a project scope. Fixed both `#[utoipa::path]` blocks and regenerated
   `packages/api`.
2. `temps-proxy` unconditionally stripped `Content-Length` from every HEAD
   response, regardless of downstream HTTP version. The existing comment
   explained the real reason (HTTP/2 clients treat a Content-Length on a
   HEAD response as a promise of body bytes and error when none arrive),
   but the code didn't gate on it — so an HTTP/1.1 client got a HEAD
   response with no Content-Length *and* no chunked encoding on a
   keep-alive connection, with no way to tell the response was complete.
   Confirmed live: `curl -I` through the proxy port hung indefinitely,
   while the identical request against the console port (same handler, no
   proxy in front) returned instantly with the header present. Fixed by
   only stripping when the downstream session is actually HTTP/2.
3. `BlobService::del` counted every successful S3 `DeleteObject` call as a
   deletion, but S3's `DeleteObject` is idempotent by design — it returns
   success whether or not the key existed. So the documented "number of
   blobs deleted" was really "number of delete calls that didn't error,"
   and a second delete of the same (already-gone) keys kept reporting them
   deleted. Fixed by `HeadObject`-checking existence before each delete so
   the count reflects what was actually removed.

Like kv-storage, blob storage is a platform-wide singleton — the scenario
never disables it in teardown.

### `flags-scenario` steps

Driven through the real `@temps-sdk/node-sdk` `FlagsClient` for every read
(not just the raw HTTP API), since that's the actual client apps deployed on
Temps use — same shape as pointing `tls-scenario` at a real ACME exchange
instead of asserting a 200:

1. create a project + resolve its production environment
2. create a bool flag and a string flag via the admin API
3. mint a deployment token scoped to (project, environment) — the same
   `TEMPS_API_URL`/`TEMPS_API_TOKEN` pair Temps injects into every real
   deployment
4. `FlagsClient.refresh()` (real HTTP call, real ETag caching) then
   `get()`/`getDetails()`: both flags resolve to their declared defaults,
   and an unrecognized key resolves to the caller's fallback
5. set a per-environment override via the admin API, `refresh()` again,
   confirm the override wins over the default
6. set the kill switch (`enabled: false`) on a flag that *also* carries an
   override value, `refresh()` again, confirm the flag reverts to its
   default — proving the kill switch outranks any override, not just that
   it exists
7. `ETag`/`If-None-Match`: a second `GET /flags/snapshot` with the ETag
   from the first response is a genuine 304
8. exposure reporting: `getDetails()` on a flag marks it evaluated,
   `flushExposure()` reports it, and the admin API's `last_evaluated_at`
   (null beforehand) is now set — proving the whole loop, not just that the
   endpoint accepts a POST
9. auth boundary: the delivery endpoint rejects a plain admin API key with
   400 (deployment-token-only, by design)
10. teardown (delete the deployment token, then the project)

Verified passing (3x back-to-back): every step green on the first attempt —
no platform bugs found.

**Scope note**: only boolean-style values, the kill switch, and
per-environment overrides are live in the shipped Phase 1
(`docs/adr/034-feature-flags.md`). Percentage rollout and targeting rules
exist in the schema but are dead code server-side (`bucket()` is never
called, `rules` is always `[]`), so this scenario doesn't exercise them.

Required `@temps-sdk/node-sdk` to be added as a workspace dependency of
`apps/temps-e2e` (`link:@temps-sdk/node-sdk`, matching the existing
`@temps-sdk/api` link) and built (`bun run build` in
`sdks/node/packages/node-sdk`) — it had no `dist/` yet.

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

### `backup-restore-scenario` steps

1. provision a real standalone postgres service, link it to a project,
   deploy the same `db-probe` app `managed-services-scenario` uses, and
   write 5 real rows through the real injected `POSTGRES_URL`
2. create an S3 source pointed at the local MinIO
3. trigger a real backup (`POST /backups/external-services/{id}/run`) --
   202 + async job, per ADR-014 -- and poll `GET /backups/{backup_id}` to a
   real `completed` state (a real `wal-g backup-push`, a real S3 upload --
   the postgres backup engine is WAL-G, not pg_dump)
4. write 2 MORE rows (count now 7) -- data the backup does NOT contain
5. start an in-place restore from the backup taken at step 3
   (`POST /external-services/{id}/restore`) and poll
   `GET /restore-runs/{id}` to `completed`
6. hit `/probe` once more and assert the count is 6 (5 backed-up rows +
   this 1 new insert), NOT 8 -- proving the restore actually reverted the 2
   post-backup rows, not just that the API calls returned 2xx
7. teardown (deployment, project, service, S3 source)

**Real bug found and fixed**: `PostgresWalgEngine::run`
(`crates/temps-backup/src/engines/postgres_walg.rs`) ran `wal-g backup-push`
without continuous WAL archiving ever having been enabled on the target
container. The base backup's checkpoint LSN then had no archived WAL segment
behind it, so every restore failed at Postgres startup with "could not
locate required checkpoint record", the container crash-looped, and
`wait_for_container_health` eventually timed out at 90s -- a genuinely
unrestorable backup that reported `completed`. Fixed by adding
`ExternalService::enable_continuous_archiving` (no-op default, implemented
on `PostgresService` by reusing the existing `enable_wal_archiving`) and
calling it **before** `wal-g backup-push`, not after: `pg_stop_backup()`
force-completes the current WAL segment as part of finishing the backup, and
Postgres only marks a completed segment `.ready` for archiving if
`archive_mode` is already on at that moment -- enabling archiving
afterward doesn't retroactively archive a segment that closed while
archiving was off. Made a no-op after the first backup on a given service
(checks `walg.env` presence first) so it doesn't recreate the container on
every single backup. Confirmed live 3x, including that the second and third
backups on the same service skip the archiving setup entirely (faster
restore, no extra container recreate).

### `pitr-scenario` steps

1. provision a real standalone postgres service, link it to a project,
   deploy the same `db-probe` app `backup-restore-scenario` uses
2. create an S3 source pointed at the local MinIO
3. trigger a real base backup and poll it to `completed` (real `wal-g
   backup-push`)
4. write 5 real rows through the injected `POSTGRES_URL` (the "T1" marker)
5. wait 75s for the WAL segment covering T1 to actually archive to S3 --
   every managed postgres runs with `archive_timeout=60` (see
   `PostgresService::create_container`), which forces the archiver to close
   and archive the current WAL segment every 60s even though 5 tiny inserts
   never fill the 16MB size threshold on their own. `wal-g wal-fetch` during
   a restore can only see WAL that's actually landed in S3, not whatever's
   still sitting in the live container's `pg_wal`
6. capture a recovery-target timestamp strictly after T1 and strictly
   before the next write
7. write 3 MORE rows (the "T2" marker, count now 8) -- data the PITR target
   must NOT include -- then a short wait so T2's WAL is durably fsynced
8. start a PITR restore in place to the captured T1 timestamp
   (`POST /external-services/{id}/restore`, `mode: "pitr"`) and poll
   `GET /restore-runs/{id}` to `completed`
9. read the actual `e2e_probe` table back through the read-only
   data-browser API (`GET
   /external-services/{id}/query/containers/{path}/entities/{entity}/data`
   -- the same endpoint `temps data rows` uses) and assert the exact row
   count (5) and exact primary-key set (`[1,2,3,4,5]`) -- an INDEPENDENT
   side channel from `/probe`, which mutates on every call, so this is a
   genuine content check, not just "the restore run's status flipped to
   completed"
10. hit `/probe` once more and re-read the table, asserting total_count is
    6, the first 5 ids are still exactly T1s `[1,2,3,4,5]`, and the new
    row's id is strictly greater than all of them -- proving the recovered
    cluster is fully writable on top of the restored data, not just
    readable
11. teardown (deployment, project, service, S3 source)

PITR depends on the same WAL-G continuous-archiving fix documented above
under `backup-restore-scenario` already being on `main` -- without it, no
backup on this platform is restorable at all, PITR included.

Two things worth documenting, neither a platform bug -- both correct
platform/Postgres behavior that this scenario's first draft got wrong:

- Step 9's container path is NOT `{service's own database}/public`.
  Linking a service to a project auto-provisions a SEPARATE
  per-project-per-environment database and points the deployed app's
  injected `POSTGRES_URL` at THAT (`PostgresService::get_runtime_env_vars`
  in `crates/temps-providers/src/externalsvc/postgres.rs`), named
  `normalize_database_name("{project_slug}_{environment_name}")` -- e.g.
  project slug `my-app` + environment `production` ->
  `my_app_production`. `normalizePostgresDatabaseName`
  (`apps/temps-e2e/src/lib/flows.ts`) mirrors the Rust normalization so the
  scenario can compute the real path instead of guessing.
- Step 10 does NOT assert the new row lands as id 6. Postgres WAL-logs
  sequence advances `SEQ_LOG_VALS` (32) values ahead of what's actually
  been handed out, specifically so crash/PITR recovery can never replay a
  value a client already received -- the first `nextval()` after ANY
  recovery legitimately jumps past `count+1`. The scenario asserts the row
  count and the restored ids' exact identity instead of the new row's
  specific id.

### `git-deploy-scenario` steps

1. create a project pointed at a real public repo
   (`github.com/gotempsh/temps-examples`), scoped to one subdirectory
   (`examples/starters/go/net-http`) via `directory`, with a `go` preset
2. resolve its production environment
3. `POST /projects/{id}/trigger-pipeline` -- the same action a real git push
   to the tracked branch would cause -- and poll
   `GET /projects/{id}/last-deployment` for the deployment it created
4. wait for the deployment to go healthy -- real clone + real build, so this
   needs a materially longer timeout (`--deploy-timeout`, default 600000ms)
   than the prebuilt-image scenarios
5. hit the deployed app's `/` and `/health` and assert their EXACT JSON
   bodies match the checked-in source (`{"message":"Hello from Go on
   Temps!"}` / `{"status":"ok"}`) -- proving the real upstream repo got
   cloned and built, not a cached/stale/wrong artifact
6. teardown (deployment, project)

**Real bug found and fixed (test-side, not platform)**: creating a
Git-type project always auto-queues an initial deployment as a side effect
of `POST /projects` itself ("Queueing initial deployment job for Git
project" server-side) -- unconditionally, regardless of the
`automatic_deploy` flag, and asynchronously (the deployment row doesn't
exist the instant `createProject` returns). The first version of this
scenario polled `GET /projects/{id}/last-deployment` once right after
project creation to use as a "baseline" for spotting the deployment the
explicit `trigger-pipeline` call would create, then triggered and waited
for any deployment with a newer id. That race the auto-deployment: its row
can land *after* the baseline check but *before* (or racing) the explicit
trigger, so it looks like "the deployment trigger-pipeline just created".
The platform then correctly cancels/supersedes that auto-deployment the
moment the real explicit-trigger deployment is created for the same
environment -- and the scenario, watching the wrong (auto) deployment,
saw it go straight to `cancelled` and reported a false failure, while the
real deployment was still building underneath. Fixed by having
`triggerPipelineAndGetDeploymentId` (`apps/temps-e2e/src/lib/flows.ts`)
*wait* for the auto-queued deployment to actually land before triggering,
so the baseline id is deterministic, then poll for a deployment id
strictly greater than it. Confirmed live 3x.

This is deliberately the one scenario in this suite that hits real
`github.com` -- see "External-service test infra" below for why the others
don't.

## External-service test infra (TLS, DNS, email, backups)

`tls-scenario`, `email-scenario`, and `backup-restore-scenario` need real
external services to test against — a live ACME CA to issue a real
certificate, an SMTP target that actually receives mail, an S3-compatible
target that actually stores a backup. Hitting the real internet for these
(real Let's Encrypt, a real inbox, a real cloud bucket) would mean real rate
limits, real credentials, and non-reproducible runs, so all three point at
local, purpose-built test servers instead:

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
- **MinIO** (`minio/minio`) — S3-compatible target for `backup-restore-scenario`.
  S3 API on `http://localhost:9092` (not MinIO's own default of 9000 — see
  the compose file comment for why), console on `http://localhost:9093`.
  Same image + credentials (`minioadmin`/`minioadmin`) already proven in
  `crates/temps-backup`'s own testcontainers-based Rust integration tests.
  Create the bucket once before running the scenario:
  `docker exec <minio-container> mc alias set local http://localhost:9000 minioadmin minioadmin && docker exec <minio-container> mc mb local/temps-e2e-backups`.
  Since `temps serve` runs natively on the host (not in a container), point
  `--minio-endpoint` at `http://localhost:9092` — the default — not
  `host.docker.internal`, which only resolves from inside a container.

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

**Postgres major-version upgrade** — deliberately scoped out of
`pitr-scenario` (and not covered by any other scenario), unlike PITR which
IS fully covered. `POST /external-services/{id}/upgrades`
(`crates/temps-backup/src/handlers/pg_upgrade_handler.rs`,
`PostgresUpgradeOrchestrator` in
`crates/temps-providers/src/externalsvc/postgres_upgrade.rs`) is real and
reachable — a multi-phase `pre_backup -> snapshot -> dump -> new_container
-> restore -> swap -> analyze` workflow that dumps the live cluster,
provisions a fresh container on the target major version, restores into it,
and swaps traffic over — but exercising it live needs meaningfully more
setup than PITR: (1) the source service must be pinned to an explicit older
major version at creation time (`version: "16"` or similar) compatible with
`validate_os_family`'s from/to image check, not the platform default; (2)
`phase_pre_backup` requires a **default** S3 source
(`ExternalService::default_s3_source_id`, `is_default: true`) rather than
the ad-hoc, non-default source `pitr-scenario`'s S3 setup uses; (3) the
workflow pulls TWO postgres major-version images and does a real
dump+restore, meaningfully longer and higher-blast-radius than PITR's
WAL-replay. Given the severe background-process instability hit while
building `pitr-scenario` on this shared dev host (the target `temps serve`
instance died unpredictably — no panic, no OOM, no crash report — multiple
times across an otherwise-successful run, requiring repeated restarts to
get 3 clean end-to-end passes), there wasn't a reliable enough window left
to also stand up and live-verify the upgrade path in the same session. A
future pass should build `pg-upgrade-scenario` as its own command following
this same pattern (base backup via a **default** S3 source, provision on an
explicit older `version`, write a marker row, trigger the upgrade, poll to
`completed`, assert the marker row survived via the data-browser API, and
assert the service is reachable on the new major version).

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
