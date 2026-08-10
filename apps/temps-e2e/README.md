# @temps-sdk/e2e

End-to-end + load testing CLI for a **live** Temps instance. Most commands
(`scenario`, `tls-scenario`, `dns01-wildcard-scenario`, `email-scenario`,
`kv-scenario`, `blob-scenario`, `flags-scenario`, `audit-scenario`,
`managed-services-scenario`, `rbac-scenario`, `monitoring-scenario`,
`error-tracking-scenario`, `logs-scenario`, `analytics-scenario`,
`session-replay-scenario`, `backup-restore-scenario`, `pitr-scenario`,
`git-deploy-scenario`, `otel-quota-scenario`, `deploy-lifecycle-scenario`,
`db-ha-failover-scenario`, `pg-upgrade-scenario`, `redis-restore-scenario`,
`mongodb-restore-scenario`, `s3-restore-scenario`, `mariadb-restore-scenario`,
`env-vars-scenario`, `api-key-scenario`, `examples`) drive the real
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

# pg-upgrade: Postgres major-version upgrade (16 → 17) -- provision a service
# pinned to postgres:16-bookworm, write 5 marker rows, trigger the upgrade
# (pre_backup → snapshot → dump → new_container → restore → swap → analyze),
# and prove via the read-only data-browser API that the marker rows survived
# the pg_dumpall → psql restore cycle. Needs MinIO (docker-compose.e2e.yml).
bun run src/index.ts pg-upgrade-scenario --registry localhost:5111
bun run src/index.ts pg-upgrade-scenario --registry localhost:5111 --upgrade-timeout 900000  # generous timeout for slow hosts

# git-deploy: real clone + build of a public GitHub repo
# (github.com/gotempsh/temps-examples) via trigger-pipeline -- proves the
# actual git pipeline, not an image pull or a synthesized Dockerfile. Hits
# real github.com (documented exception to this suite's "no real internet"
# rule -- see "External-service test infra" below).
bun run src/index.ts git-deploy-scenario

# db-ha-failover: real Postgres HA (pg_auto_failover) automatic-failover
# proof -- provision a 1-monitor + 2-data-node cluster, deploy an app through
# the injected multi-host POSTGRES_URL, `docker stop` the elected primary,
# and assert a surviving replica gets promoted AND the same app (no
# redeploy) resumes writing within a bounded window.
bun run src/index.ts db-ha-failover-scenario --registry localhost:5111

# deploy-lifecycle: deploy version A, deploy version B, rollback to A, pause,
# resume, and promote B into a second environment -- asserts the EXACT
# response body served over the real proxied URL at every step (never just a
# deployment row's status field). Needs a registry the Temps server can pull
# from, same as `examples`/`logs-scenario`/etc.
bun run src/index.ts deploy-lifecycle-scenario --registry localhost:5111
bun run src/index.ts deploy-lifecycle-scenario --keep --json    # inspect afterward (CI)

# otel-quota: real OTLP/HTTP protobuf traces/metrics/logs (hand-encoded, no
# @opentelemetry/* SDK) round-tripped through the query API + Observe feed,
# then real storage-quota enforcement pushed past a configured
# TEMPS_OTEL_QUOTA_GB until ingestion is actually rejected (413). The target
# instance MUST be launched with TEMPS_OTEL_QUOTA_GB=1 (the smallest nonzero
# value) for the quota half to mean anything -- see its section below.
bun run src/index.ts otel-quota-scenario
bun run src/index.ts otel-quota-scenario --max-quota-batches 400 --json

# redis-restore / mongodb-restore / s3-restore: backup + in-place restore of
# each managed service via MinIO, proven by verifying pre-backup data
# survives and post-backup-only data is erased. Needs MinIO (docker-compose.e2e.yml).
bun run src/index.ts redis-restore-scenario --registry localhost:5111
bun run src/index.ts mongodb-restore-scenario
bun run src/index.ts s3-restore-scenario

# mariadb-restore: backup + in-place restore of a real MariaDB service --
# insert pre-backup rows via docker exec, real backup (physical or logical,
# engine-selected) to MinIO, insert post-backup rows, restore in place, and
# verify via the data-browser API that only the pre-backup rows survive.
# Needs MinIO (docker-compose.e2e.yml).
bun run src/index.ts mariadb-restore-scenario

# env-vars: create/update/delete a project env var and redeploy after each
# change, asserting the RUNNING CONTAINER (an echo-server app, not just the
# API response) reflects the new/updated/removed value, plus an
# environment-scoping check.
bun run src/index.ts env-vars-scenario --registry localhost:5111

# api-key: a second low-privilege user creates a scoped API key over real
# HTTP, the key gets 200 on an in-scope request and 403 on an out-of-scope
# one, then revocation makes an immediate retry fail. Needs DB-direct access
# to mint the second user's initial session, same as rbac-scenario.
bun run src/index.ts api-key-scenario --temps-root /path/to/temps --database-url postgres://...
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

### `pg-upgrade-scenario` steps

Closes the real coverage gap noted in the former "Known gaps" section: the
`PostgresUpgradeOrchestrator`
(`crates/temps-providers/src/externalsvc/postgres_upgrade.rs`) and its
`POST /external-services/{service_id}/upgrades` HTTP surface were fully wired
and real, but zero e2e coverage existed for the actual dump+restore data
path -- the only honest proof that a major-version upgrade preserves data is
reading the rows back after it finishes, not just watching a status field flip.

1. build + push the db-probe Go app (`lib/probe-app.ts`) -- the same app
   `backup-restore-scenario` and `pitr-scenario` use. On every `/probe` hit
   it inserts a row into `e2e_probe` and returns the total count
2. provision a real standalone Postgres service pinned to `postgres:16-bookworm`
   via `parameters.docker_image` at creation time. Standard official postgres
   images are used; `extract_postgres_version("postgres:16-bookworm")` returns
   `"16"` and PGDATA is set to `/var/lib/postgresql/16/docker`. The explicit
   pin is required because the upgrade API validates that `to_version > from_version`
   and both images are on the same OS family (Alpine ↔ Alpine, Debian ↔ Debian)
   -- using the platform default 18-bookworm image would leave nowhere to
   upgrade to in an e2e test
3. create a **default** S3 source (`is_default: true`) pointed at the local
   MinIO. `phase_pre_backup` in the orchestrator calls
   `BackupService::default_s3_source_id` (`WHERE is_default = true`) to find
   the backup target; the upgrade API returns 412 immediately if no default
   source is configured -- this is a hard precondition, not an optional step
4. link the service to a project, deploy the db-probe app, wait for it to be
   healthy, and confirm `/health` succeeds (real DB ping)
5. write 5 real rows through `/probe` (the T1 marker set). These land in the
   per-project auto-provisioned database (NOT the service's own `database`
   parameter -- see `PostgresService::get_runtime_env_vars`), named
   `normalize_database_name("{project_slug}_{env_slug}")` as computed by
   `normalizePostgresDatabaseName` in `flows.ts`
6. `POST /external-services/{service_id}/upgrades` with
   from_version="16" / to_version="17" / from_image="postgres:16-bookworm" /
   to_image="postgres:17-bookworm". The orchestrator spawns a
   tokio task and returns immediately with status="pending". It then runs
   seven real phases synchronously:
     - `pre_backup`: wal-g backup to the default MinIO S3 source (safety net)
     - `snapshot`: stop old container, `docker cp`-style volume copy to a
       rollback volume, remove the original volume
     - `dump`: boot throwaway old-version container with rollback volume
       mounted, run `pg_dumpall`, write dump to a separate volume
     - `new_container`: `lifecycle.create_and_start(service_id, to_image)` --
       boots a fresh 17-bookworm container (empty data volume → initdb)
     - `restore`: boot a psql container with the dump volume, run
       `psql < data.sql` against the new container
     - `swap`: persist `to_image` onto the service's `parameters.docker_image`
       column via `lifecycle.set_docker_image`, restart the container
     - `analyze`: run `ANALYZE` so the planner sees the new-version stats
7. poll `GET /external-services/{service_id}/upgrades/{id}` every 5s until
   `status === "completed"` or `status === "failed"`, up to `--upgrade-timeout`
   (default 600s). On failure, the error includes the last-seen phase and
   `error_message` from the upgrade row so the cause is always visible
8. assert the 5 T1 marker rows survived: read them via the read-only
   data-browser API (`GET /external-services/{id}/query/containers/{path}/entities/{entity}/data`
   -- same endpoint `pitr-scenario` uses) and assert `total_count=5` and
   `ids=[1,2,3,4,5]` exactly. This is the only honest proof: the dump captured
   those rows and psql restored them. A status=completed without this
   check would pass even if the restore loaded into the wrong database or no
   database at all
9. assert the service is fully writable on the new version: hit `/probe` once
   more and confirm a 6th row landed, read back via data-browser and assert
   `total_count=6`, first 5 ids exactly `[1,2,3,4,5]`, new id > 5. The
   sequence jumps past the pre-dump high-water mark for the same reason
   PITR does (WAL-logged `SEQ_LOG_VALS` advance), so we assert count and
   monotonicity, not the exact new id
10. assert `GET /external-services/{id}`'s `current_parameters.docker_image`
    now equals `postgres:17-bookworm`. `phase_swap` calls
    `lifecycle.set_docker_image` which persists the new image into
    `external_services.parameters`; this checks the API-visible result
11. teardown (deployment, project, service, S3 source)

### `mariadb-restore-scenario` steps

Closes the last remaining gap in the backup/restore engine parity story:
Postgres (`backup-restore-scenario`, `pitr-scenario`, `pg-upgrade-scenario`),
Redis, MongoDB, and S3 all have restore e2e coverage; MariaDB had the engine
code (`crates/temps-providers/src/externalsvc/mariadb.rs`'s
`restore_capabilities`/`restore_to_new_service`/`restore_pitr`, all fully
wired to the trait) but no live-server proof until now.

1. provision a real MariaDB service and read its root password directly off
   the container via `docker inspect` (`MARIADB_ROOT_PASSWORD`, per
   `MariaDbService::get_container_name`)
2. insert 3 pre-backup rows into a real table via `docker exec mariadb -uroot
   -p... -e ...` -- no synthetic DB rows, the same discipline every other
   restore scenario holds itself to
3. create a MinIO S3 source and trigger a real backup; the backup engine
   auto-selects physical or logical (`crates/temps-backup/src/engines/dispatch.rs`)
4. insert 2 more rows post-backup, to prove restore reverts state rather than
   just leaving current state alone
5. start an in-place restore and poll until it completes
6. verify via the read-only data-browser API (not a direct DB read) that the
   3 pre-backup rows are present with correct values, the 2 post-backup rows
   are absent, and exactly 3 rows remain
7. teardown (S3 source, service)

### `env-vars-scenario` steps

Proves environment-variable changes actually propagate into the running
container -- not just that the API round-trips the value. App env vars have
**no hot-reload**: a changed value only takes effect after a redeploy, so
the scenario redeploys after every change and asserts on the live
container's own response, never the database row.

1. create a project, deploy `examples/echo-server` (its response body
   includes `env: process.env`, so it directly surfaces the real container's
   environment)
2. create an env var scoped to production with a distinct marker value;
   assert the create response round-trips it in plaintext (list responses
   mask non-secret values as `"***"` -- only create/update return plaintext)
3. redeploy, then poll the live container's own response until
   `env["E2E_ENV_VAR_MARKER"]` actually equals the new value
4. update the var to a second marker value, redeploy again, assert the
   response reflects the NEW value (not the stale one) -- proves updates,
   not just creates
5. delete the var, redeploy, assert the key is genuinely absent from the
   running container's environment
6. lightweight, deploy-free check: a var scoped only to production must not
   appear when `GET .../env-vars` is filtered by a second (staging)
   environment's id, but must appear when filtered by production
7. teardown (env vars, deployments, project)

### `api-key-scenario` steps

Promotes `web/e2e/authenticated/api-key-create.spec.ts` (UI-only: open
dialog, fill name, submit, see the key once) to a real API round trip,
covering what only an API-level test can prove: scope enforcement and
revocation actually cutting off access, not just that api-key CRUD returns
2xx.

A real constraint shaped this scenario: `POST /api-keys` is gated by
`SensitiveAction::CreateApiKey`, and `DefaultSensitiveActionAuthorizer`
denies every machine (API-key-authenticated) principal outright
(`machine_principals_are_denied_by_default`,
`crates/temps-auth/src/sensitive_action.rs`) -- so this suite's own primary
bearer-key identity can never call it. Same wall `rbac-scenario` already
documented. The fix here: mint a second, low-privilege user via DB-direct
`temps api-key` (needs `--temps-root`/`--database-url`, same as
`rbac-scenario`), log it in for real via `POST /auth/login`, and drive key
creation/revocation with the resulting `session` cookie via raw `fetch` --
the SDK client always attaches a Bearer header, so it can't be used for this
leg.

1. create a project (primary bearer identity)
2. create + log in a second, low-privilege user; capture its `session` cookie
3. `POST /api-keys` (session-cookie authenticated) with a custom key scoped
   to `projects:read` only; capture the plaintext key (returned only once)
4. use the scoped key (`Authorization: Bearer tk_...`) against `GET
   /projects/{id}` -- assert 200
5. use the same key against `PATCH /projects/{id}` -- assert 403 with
   `required_permission: "projects:write"`
6. revoke the key (`POST /api-keys/{id}/deactivate`, session-cookie
   authenticated)
7. immediately retry the previously-200 request with the revoked key --
   assert it now fails. Revocation is instant (`ApiKeyService::validate_api_key`
   queries `IsActive.eq(true)` directly on every call; the only cache
   throttles `last_used_at` write-back, never the pass/fail read), so this
   is a single request, not a poll
8. teardown (key, second user, project)

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

### `db-ha-failover-scenario` steps

Closes a real gap: `PostgresClusterService`
(`crates/temps-providers/src/externalsvc/postgres_cluster.rs`), the
`cluster-health`/`members`/`promote` API surface
(`crates/temps-providers/src/handlers/handlers.rs`), and the multi-host
`POSTGRES_URL` env-var injection
(`ExternalServiceManager::build_cluster_env_vars_for_resource`) are all real
and wired end-to-end, but had zero e2e coverage before this. This is
single-Docker-host HA (every member is `node_id: null`, placed on the
control plane with unique container names/ports) -- distinct from
multi-node/WireGuard clustering, which needs separate real hosts and is out
of scope here.

1. provision a real 1-monitor + 2-data-node Postgres HA cluster
   (`topology: 'cluster'`) and poll until the service and all 3 members
   report `status: 'running'`
2. poll `GET /external-services/{id}/cluster-health` (reads
   `pgautofailover.node` directly off the monitor) until the cluster
   reaches a steady state: exactly one data node reporting a writable-primary
   state, the other `secondary`
3. independently confirm the elected primary's container is actually
   running via `docker inspect` -- a side channel the platform API can't
   fake
4. link the cluster to a project **before** deploying (env vars resolve at
   deploy-job-creation time), deploy a `db-probe` Go app built with the
   `pgx` driver (not this suite's usual `lib/pq` probe -- see
   `buildHaProbeImage`'s doc comment in `lib/probe-app.ts`: `lib/pq`'s
   latest *released* version cannot parse a multi-host
   `postgresql://host1:port1,host2:port2/db` connection string at all,
   verified live), and confirm it got the cluster's multi-host
   `POSTGRES_URL`
5. write 5 real rows through `/probe`
6. `docker stop` the primary's container -- a real, ungraceful outage, not
   an API call. `docker stop`, not `kill`: every cluster member's
   `HostConfig.RestartPolicy` is `unless-stopped`, so Docker itself would
   silently resurrect a `kill`ed container before pg_auto_failover's
   monitor ever declared it unhealthy, masking whether real failover
   happened at all
7. poll cluster-health until the surviving replica reports a
   writable-primary state -- proves the monitor actually promoted it, not
   just that the old primary's row went stale
8. poll `/probe` (tolerating connection errors while pg_auto_failover
   completes the promotion) until writes succeed again, bounded, proving
   the app's existing connection string routes to the new primary with no
   redeploy, config change, or app restart
9. assert the post-failover row count is monotonic -- the write actually
   landed, not just that the HTTP call returned 200
10. teardown (`delete_service` removes cluster containers by name
    regardless of running state, so the stopped ex-primary is cleaned up
    too)

Between steps 7 and 8, the scenario also asserts a platform-level symptom
the PR's own root-cause narrative below cites: that the **console/CLI-facing
API** (`GET /external-services/{id}`, backed by
`get_service_members_with_live_state`) reflects the promotion via
`ServiceMemberInfo.live_state` — not just the raw `cluster-health` probe
already checked in step 7. This exercises a genuinely different code path
than step 2/7's polling: `get_service_info`'s own doc comment explains why
`live_state` (not `service_members.role`) is the field to check —
`service_members.role` is intentionally config-only (`monitor`/`replica`)
in the current design and is never written to `"primary"` at runtime
(confirmed by reading the code: `PgAutoFailoverState::to_cluster_role()`,
which maps to a `Primary` role, is defined and unit-tested but not called
from any production code path — `sync_member_roles` only ever demotes
legacy `"primary"` rows to `"replica"` once). Asserting a `role` flip would
therefore assert something that doesn't happen by design; `live_state` is
the faithful, current equivalent.

The PR's narrative also cites the DNS reconciler failing to republish
`primary.<svc>.temps.local`. That assertion was investigated and **skipped
as impractical for this round**: the only HTTP surface that exposes the
internal DNS zone content is `GET /internal/nodes/{node_id}/dns/changes`
(`crates/temps-dns/src/handlers/dns_sync.rs`), gated behind per-node
bearer-token auth (`nodes.token_hash`), not the user/API-key auth this
suite otherwise uses end-to-end — and this scenario deliberately stays
single-Docker-host with no worker node registered, so there is no node
token to present. Reaching the DNS row from the e2e script would mean
either registering a throwaway worker node purely to mint a token (a
disproportionate amount of new surface for one assertion) or querying the
control-plane's own Postgres directly (breaking this suite's API-only
testing philosophy, and requiring connection details this scenario file
otherwise has no reason to know). A cheaper, non-API path would be adding a
narrow admin/debug endpoint that returns a service's internal DNS records —
worth doing if this assertion becomes a priority, but out of scope for this
round.

**Two real platform bugs found and fixed, both in
`crates/temps-providers/src/externalsvc/cluster_role.rs`'s
`PgAutoFailoverState::is_primary()`** -- the single classifier the DNS
reconciler, `member_is_live_primary`'s delete-protection gate, and (via a
copy in this scenario) the e2e assertions all keyed off:

- `is_primary()` excluded `WaitPrimary`, on the documented (but factually
  wrong) theory that it meant "candidate primary, not yet writable."
  Verified live against a real 2-data-node cluster: `docker stop` the
  primary, and pg_auto_failover promotes the survivor straight to
  `wait_primary` -- direct `psql` against that node the whole time showed
  `pg_is_in_recovery() = false`, `default_transaction_read_only = off`, and
  a real `INSERT` succeeding. With only one node left, there's no third
  node to attach as a new standby, so `wait_primary` isn't a brief
  transition here -- it's the cluster's **permanent, correct steady state**
  after a 2-node failover (confirmed: it never left `wait_primary` even
  after a 300s poll). `crates/temps-providers/src/services.rs`'s own
  `cluster_states::WRITABLE` already modeled this correctly
  (`&["primary", "wait_primary", "single", "apply_settings"]`) -- the two
  had drifted apart. Consequence: `drafts_for_snapshot` (the DNS
  reconciler) never republished `primary.<svc>.temps.local` after exactly
  this failover, and `to_cluster_role()` never flipped `service_members.role`
  either -- both would have gone stale forever on any 2-node cluster.
- `member_is_live_primary` (`services.rs`) had its own, independent
  `matches!(reported_state, "primary" | "single")` string check for the
  same concept -- also missing `wait_primary`, and used to gate
  `remove_cluster_member`'s "don't delete the writable node" safety check.
  A `wait_primary` primary would have passed this check as "not the
  primary," meaning an operator could have deleted the cluster's only
  writable node while it was actively serving traffic. Fixed to call
  `PgAutoFailoverState::is_primary()` instead of hand-rolling the match, so
  this can't drift from the reconciler's definition again.

Both fixes are behavioral, not cosmetic: before them, this scenario's
`docker stop` step hung until the 90s failover-timeout expired on every
run, even though the app's own `/probe` endpoint -- hit directly, out of
band -- was already serving fresh writes against the promoted node the
whole time. The platform was already treating the node as the real primary
for actual traffic; only the status classification was wrong. Confirmed
live 3x after the fix (`failoverDetectedMs` ~50-53s, `writesRecoveredMs`
~11ms -- the app's connection string re-resolves to the new primary almost
instantly once pg_auto_failover reports it, since `pgx`'s
`target_session_attrs=read-write` just tries each host in the DSN).

Also fixed along the way (not a platform bug, a startup-ordering bug in the
reconciler's own shutdown path,
`crates/temps-providers/src/externalsvc/postgres_role_reconciler.rs`):
`ReconcilerShutdown` existed as a struct but was never actually wired into
`spawn_role_reconciler`/`stop_role_reconciler`/`run()`, which still used a
bare `tokio::sync::Notify` -- `notify_waiters()` only wakes a task that is
*already* parked in `.notified()`, so a shutdown signal that arrived while
`run()`'s loop was mid-`reconcile_once` (real monitor I/O) was silently
lost forever, leaking one reconciler task per deleted cluster that logged
"Monitor not found" every 5s for the rest of the process's life. Wired
`ReconcilerShutdown`'s `AtomicBool` + `Notify` pair through end-to-end
(`run()` checks `is_stopped()` at the top of every loop iteration, not just
inside the tick `select!`), so a signal is caught within one tick even if
it arrives mid-reconcile.

### `multinode-join-scenario` steps

The first-ever e2e coverage for temps's direct-underlay multi-node clustering
feature. Before this, the only coverage anywhere was Rust unit tests
against a `MockDatabase`
(`crates/temps-deployments/tests/multinode_integration_test.rs`) and a
manual, non-automated dev tool (`tools/dev-cluster/`) a human has to run
and eyeball. Nothing asserted that a second real node actually joins the
mesh, that a deployment pinned to it actually lands there, or that drain/
removal actually work.

**Why this scenario owns its entire cluster instead of using the shared
instance.** Every other scenario in this suite points `--url`/`--api-key`
at an already-running `temps serve` (started via the `start-temps` skill)
and drives it over HTTP. Multi-node clustering can't be proven that way:
it needs a genuinely SEPARATE node — its own Docker daemon, its own binary,
its own network identity — registering into the first node's mesh over
single-use enrollment and real mTLS. WireGuard relay enrollment is a separate
topology and is not claimed by this scenario. `tls-scenario` already established the precedent that a
scenario can need "a dedicated instance on a fixed port, not a normal dev
slot" (see that section above) because of Pebble's hardcoded port; this
scenario takes the same idea further: it brings up its own 2-node
Docker-in-Docker cluster (`tools/e2e-multinode-cluster/docker-compose.yml`
— a trimmed, re-subnetted clone of `tools/dev-cluster/`'s topology, safe to
run alongside a developer's own dev-cluster instance or any other local
service; see that compose file's header comment for the exact isolation
guarantees), mints its own admin credential once the cluster is up, and
tears the whole thing down at the end. It does NOT accept `--url`/
`--api-key` — there is no "target instance" to point at.

1. `docker compose up -d --build` the 2-node cluster. First run compiles
   the full `temps` binary from source TWICE (once per DinD container) —
   budget 15-20+ minutes; this is streamed to the scenario's own log output
   line-by-line (prefixed `[compose]`) specifically so a long first build
   is visibly progressing, not indistinguishable from a hang. Bounded by
   `--build-timeout` (default 30 min).
2. poll `docker inspect --format '{{.State.Health.Status}}'` on the
   control-plane container until `healthy`.
3. mint an admin API key directly from the DB: `docker exec ... temps
   api-key --database-url=... --name=e2e-multinode --role=admin
   --user-email=admin@local.dev --output-format=json` — the same DB-direct
   pattern this README documents under "Auth" and `db-apikey.ts`/
   `rbac-scenario` already use elsewhere in this suite. Works because
   `role-control-plane.sh`'s `temps setup --auto` guarantees
   `admin@local.dev` exists by the time the healthcheck passes.
4. from here on, drive everything through the normal `@temps-sdk/api`
   client against `http://localhost:18180`, same as every other scenario.
5. poll `GET /internal/nodes` until a node named `worker-1` reports
   `status: "active"` — the real proof `POST /internal/nodes/register`
   completed a genuine registration, not a mocked one. Bounded by the same
   generous timeout: the worker also compiles its own binary from scratch.
   Then verify the node endpoint uses HTTPS, the worker persisted its mTLS
   leaf/key/CA, legacy shared enrollment is disabled, and the node-bound
   single-use enrollment token has exactly one use.
6. create a throwaway project, resolve its production environment.
7. `PUT /projects/{id}/environments/{id}/settings` with
   `target_nodes: [worker_node_id]` — pins every future deploy in this
   environment to the worker, never the control plane.
8. deploy `traefik/whoami:latest` (same image other scenarios in this repo
   already use for basic deploys) and poll it to a terminal state.
9. the core assertion: `docker exec temps-e2e-mn-worker-1 docker ps` shows
   the deployed container; `docker exec temps-e2e-mn-control-plane docker
   ps` does not — a side channel the platform API can't fake, mirroring how
   `db-ha-failover-scenario` proves promotion via `docker inspect`.
10. real HTTP proof of life: hit the deployed app through the
    control-plane's proxy (`localhost:18180`) with the app's `Host` header
    and assert the actual `traefik/whoami` response body, not just a
    healthy status field.
11. drain the worker (`POST /internal/nodes/{id}/drain`), poll
    `GET /internal/nodes/{id}/drain` until `drain_complete`, then re-run the
    same `docker ps` side-channel check on both containers to confirm the
    container migrated off the worker. In this 2-node cluster it has
    nowhere to go but the control plane, so this step also implicitly
    re-tests the `Local` scheduling fallback path.
12. remove the worker node (`DELETE /internal/nodes/{id}`); confirm it's
    gone from `GET /internal/nodes`.
13. teardown (in a `finally`, same discipline as every other scenario):
    `docker compose down` (no `-v`, so the cargo-registry/cargo-git/
    workspace-target cache volumes survive for a near-instant re-run), then
    explicitly `docker volume rm` the identity/state volumes (postgres
    data, both containers' `/var/lib/docker` + `/var/lib/temps`, the
    worker's `/root`) so the next run proves a genuinely fresh
    registration/join rather than silently skipping it via one of the role
    scripts' own idempotency marker files. `--keep` skips all of this —
    unlike every other scenario's `--keep`, this leaves an entire running
    2-node cluster behind, not just one container.

**What makes step 9 work without a registry.** Unlike most `--registry`-
requiring scenarios in this suite, deploying a bare public image tag to a
remote node needs no registry at all: `ensure_image_on_remote` in
`crates/temps-deployments/src/jobs/deploy_image.rs` has the control plane's
own `DockerImageBuilder` pull (if needed) and `docker save` the image to a
tar, stream it to the worker agent's `POST /agent/images/import`, and the
agent `docker load`s it there. `image_builder` is injected into the deploy
job per `workflow_execution_service.rs` (`builder.image_builder(...)`,
around the node-scheduler wiring) specifically for this transfer path. So
`traefik/whoami:latest` works exactly as described in this section's
design with zero extra image-transfer complexity — this was verified by
reading the actual remote-deploy code path, not assumed.

```bash
bun run src/index.ts multinode-join-scenario
bun run src/index.ts multinode-join-scenario --keep --json    # inspect the running cluster after (CI)
bun run src/index.ts multinode-join-scenario --build-timeout 2400000   # more generous on a slow machine
```

### `deploy-lifecycle-scenario` steps

Rollback / pause / resume / promote are the platform's primary safety valve
for a bad deploy on live traffic. Every other scenario in this suite only
proves create -> health -> teardown; this one proves the actual
traffic-affecting operations work, by asserting the EXACT response body
served over the real proxied URL at each step -- never just "the deployment
row's status field flipped".

1. build + push two genuinely different images (`versioned-app.ts`, a
   throwaway Go app whose entire response body is a string baked in at
   `docker build --build-arg VERSION_TEXT=...` time via `-ldflags -X`) --
   "version A" and "version B"
2. deploy version A; assert live traffic serves `"version A"` byte-for-byte
3. deploy version B to the SAME project/environment; assert live traffic now
   serves `"version B"` byte-for-byte
4. `POST .../deployments/{A}/rollback`; assert live traffic reverts to
   `"version A"` byte-for-byte -- proves a real rollback, not just a state
   transition
5. `POST .../deployments/{current}/pause`; assert the live URL genuinely
   stops serving the app -- see the real-bug note below for what "paused"
   actually renders as and why
6. `POST .../deployments/{current}/resume`; assert live traffic serves
   `"version A"` again
7. create a second environment, `POST .../deployments/{B}/promote` into it;
   assert ITS live URL serves `"version B"` byte-for-byte, and that
   production is unaffected (proves promote is a genuinely distinct
   mechanism from rollback: an arbitrary historical deployment's image,
   copied into a different environment -- not "restore a previous version in
   place")
8. teardown (deployments, project -- cascades the second environment)

`cancel_deployment` is deliberately not covered: it aborts an in-flight
(pending/deploying) job, a different lifecycle stage than the four
"deployment is already live" operations above, and there's nothing serving
yet to assert a body against.

**Three real platform bugs found and fixed (`temps-deployments` +
`temps-routes`)**:

1. **Rollback/promote rejected their own primary use case.** Both validated
   the source deployment's state against `["deployed", "completed"]` (promote
   also allowed `"ready"`) -- but a deployment superseded by a newer one in
   its own environment is `"stopped"` (see `cancel_previous_deployments` /
   `teardown_deployment`), which is exactly the state any deployment you'd
   actually want to roll back to or promote is in. Every realistic "rollback
   to the previous version" or "promote that known-good build" call 400'd
   with `Cannot rollback to deployment in 'stopped' state`. Reproduced live:
   step 4 below 400'd on every run before this fix. Fixed by adding
   `"stopped"` to both allow-lists.

2. **Pause and resume used incompatible Docker operations.**
   `pause_deployment` used to `docker stop` **and** force-`docker rm` each
   container, but `resume_deployment` called `deployer.resume_container` --
   Docker's `unpause` (the reverse of a cgroup-freeze `docker pause`), which
   only ever undoes a *genuine* `docker pause`. Nothing in the real pause
   path ever paused (froze) a container; it removed it outright, so resume
   always failed against a real deployment ("no such container") the instant
   pause had actually run. The existing unit tests never caught it because
   neither one set up a real backing container to pause/resume. Fixed:
   `pause_deployment` now only `docker stop`s (never removes) each
   container, and `resume_deployment` now calls `deployer.start_container`
   -- the correct reverse of `stop` -- instead of `resume_container`.

3. **Even with (2) fixed, a paused container stayed live in the proxy
   indefinitely.** `route_table::load_routes` filtered candidate upstream
   containers ONLY on `deleted_at IS NULL`, never on `status` -- so a
   stopped-but-not-removed container's row still looked routable. Worse,
   neither `pause_deployment` nor `resume_deployment` ever requested a
   route-table reload: the only DB triggers wired to the in-process
   route-table listener are on `environments`/`projects`
   (`m2025*_add_*_route_trigger.rs`), and a bare `deployment_containers`
   status `UPDATE` fires neither. **Reproduced live**: after pause, the
   proxy kept retrying the OLD (still "valid-looking") container address and
   returned Pingora's own `503 Service Unavailable` ("Fail to connect ...
   Connection refused") -- not a clean "paused" signal, just an accident of
   a stale cached route; the "an untested guess would reach for a 503"
   sentence that used to be here was itself exactly that untested guess,
   and turned out to be the pre-fix bug, not the fixed behavior. Fixed by
   (a) filtering `route_table::load_routes` to `status IS NULL OR status =
   'running'`, so a stopped container's route is skipped once reloaded, and
   (b) having pause/resume publish `Job::ForceRouteReload` (the same
   in-process broadcast `mark_deployment_complete.rs` already uses after a
   normal deploy) so the reload happens immediately. With both fixes,
   pausing makes the route disappear entirely and the proxy falls through to
   its existing unknown-host console-fallback response (HTTP 200,
   `<title>Temps</title>`) -- that fallback is the real, asserted "paused"
   behavior in step 5 above.

Confirmed live 3x back to back (after the environment-flakiness note below),
including that resume correctly restarts the SAME container (not a rebuild)
and traffic recovers within ~1.5s of the resume call.

Also worth noting for anyone re-running this: the local dev instance used to
verify this crashed several times mid-run with no panic/error logged (just
stops writing to its log) while this branch was being built, on a heavily
loaded shared dev machine running many other agents' builds/containers
concurrently -- unrelated to this scenario's own logic (it reproduced before
any pause/resume traffic ran, e.g. mid-`docker pull`). Launching the server
with the harness's `run_in_background` tool option (or
`dangerouslyDisableSandbox` + `nohup ... &`) rather than a plain backgrounded
shell command was what made it survive a full run.

### `otel-quota-scenario` steps

Requires the target instance to be launched with `TEMPS_OTEL_QUOTA_GB=1`
(the smallest nonzero value the knob accepts -- it's parsed as a whole
`u64` GB count, see `crates/temps-otel/src/plugin.rs`). Without it, quota
is disabled instance-wide and step 5 fails fast with a clear "you didn't
configure this" error instead of a false pass.

1. create a project. OTel's `tk_` (API-key) ingest path authenticates with a
   bearer `tk_` token plus `X-Temps-Project-Id`
   (`crates/temps-otel/src/ingest/auth.rs::authenticate_api_key`) -- the same
   shape the harness's own control-plane calls already use, so this scenario
   ingests with that same key rather than minting a fresh one via
   `POST /api-keys`: creating an API key is a `SensitiveAction::CreateApiKey`
   that the OSS `RequireVerificationAuthorizer` always downgrades to "needs
   interactive step-up verification" for every principal type, including an
   already-authenticated API key, by design (so a leaked `tk_` key can't mint
   itself more credentials). A scriptable run has no session to step up
   with, so it uses the credential it was given -- exactly what a real
   deployed collector does.
2. hand-encode one real root span, one gauge metric point, and one log
   record as raw OTLP/HTTP protobuf (`apps/temps-e2e/src/lib/otlp.ts` --
   field numbers copied directly from the vendored `.proto` files at
   `crates/temps-otel/proto/opentelemetry/proto/**`, not guessed) and POST
   each to `/otel/v1/{traces,metrics,logs}`
3. read them all back through the real query API -- `GET /otel/traces`
   (by `trace_id`), `GET /otel/traces/{project_id}/{trace_id}`,
   `GET /otel/logs` (body + trace correlation), `GET /otel/metrics` (exact
   value) -- asserting decoded field values, not just "a row exists"
4. confirm the root span appears in the unified Observe feed
   (`GET /projects/{id}/observe/events?kinds=span`), and that
   `kinds=log` is rejected 400 (there is no `Log` variant in
   `ObservabilityEvent` by design -- logs have their own page; see the bug
   fix below)
5. read `GET /otel/quota/{project_id}` and confirm a fresh project starts
   well under 100% with a nonzero configured limit
6. push ~9 MB OTLP trace batches (one span each, one giant filler
   attribute) in a loop, polling the **uncached** `GET /otel/quota`
   endpoint after every batch, until `usage_pct >= 100` (capped at
   `--max-quota-batches`, default 250)
7. sleep past the ingest-time quota cache TTL (30s, see
   `crates/temps-otel/src/ingest/quota_cache.rs::QUOTA_CACHE_TTL`) --
   `check_quota` on the hot ingest path deliberately reuses a cached
   result for up to 30s (an exact per-project `COUNT(*)`-based estimate on
   every request would be too expensive), so a burst inside that window
   can legitimately land after crossing 100%; that's a documented
   accuracy/hot-path tradeoff, not a bug, and pushing through it would
   make the next assertion flaky
8. send one more (small) batch with a unique canary span name and assert
   it's rejected with HTTP 413 (`OtelError::QuotaExceeded`, body mentions
   "quota") -- twice, to rule out a one-off race
9. query `GET /otel/traces` for the canary span name and confirm it never
   landed -- proves the rejection is real (the row didn't get written),
   not just that the HTTP call happened to error while ingestion secretly
   continued
10. teardown: delete the project

**Real bug found and fixed (the headline one): quota enforcement was
silently inert for every project.** `TimescaleDbStorage::get_storage_quota`
(`crates/temps-otel/src/storage/timescaledb.rs`) estimates a project's
share of each OTel hypertable as `hypertable_size * (project_rows /
approximate_row_count(hypertable))` -- but the code being fixed called
`pg_total_relation_size('otel_spans')` (etc.) instead of
`hypertable_size('otel_spans'::regclass)`. A hypertable's "root" relation
(the name you create it under) holds no rows and almost no bytes of its
own -- TimescaleDB partitions all actual data into child "chunk" tables
under `_timescaledb_internal`, and `pg_total_relation_size` on the root
name measures only that root's tiny catalog/index footprint. Verified live:
with `TEMPS_OTEL_QUOTA_GB=1` configured, pushing over 2GB of real OTLP
trace data into one project (250 batches, `--max-quota-batches`'s cap)
never moved `total_bytes` past ~15MB or `usage_pct` past ~1.5% -- the quota
could not be crossed no matter how much was ingested. `hypertable_size()`
is TimescaleDB's chunk-aware equivalent (sums every chunk's own
`pg_total_relation_size`, still cheap -- proportional to chunk count, not
row count). After the fix, the same volume test trips the quota
consistently at ~110 batches / ~990MB sent against a 1024MB (1 GiB)
configured limit, across 4 consecutive clean runs. This is the exact
160GB/day-flood failure mode the quota exists to prevent, and it could not
have been caught by unit tests against a mocked storage layer -- only a
real TimescaleDB instance has real chunk partitioning to get wrong.
Alongside this, `LEAST(1.0, ratio)` was added around each per-table
proportion: `approximate_row_count` is a planner-statistics estimate that
can lag the true count right after a write burst, and without the clamp a
stale (too-low) denominator could compute a per-project share above 100%
of the table, inflating `total_bytes` past what the table itself reports.

**Residual limitation found (not fixed here, documented as a known
follow-up)**: because `approximate_row_count` is planner-statistics-based
and shared across every project on the same hypertable, a project that
checks its quota shortly after a DIFFERENT project on the same instance
just finished a large write burst can be told it's already near/over 100%
usage from a handful of its own rows -- the stale (too-low) row-count
denominator combined with the `LEAST(1.0, ..)` clamp above conservatively
(but incorrectly) attributes the whole table to whichever project asks
first. Reproduced live: running this scenario twice back-to-back against
the *same* database (no fresh project/DB in between) makes the second
run's very first quota check fail with `usage_pct` already `>100` on a
project with only its own 3 initial spans. It self-heals once
autovacuum/`ANALYZE` catches up (seconds, in practice), and does not
recur when each run gets an isolated database (the normal way this suite
runs, and how CI would run it). A proper fix -- e.g. a periodic background
`ANALYZE` of the three OTel hypertables, decoupled from any single
project's request path -- is a bigger design/cost tradeoff than fits this
PR; flagged here rather than silently patched around or ignored.

**Real bug found and fixed (secondary)**: `EventsQuery.kinds`'s doc comment
(which `utoipa` surfaces into the OpenAPI spec, and from there into
`@temps-sdk/api`'s generated docs) advertised `log` as a valid Observe
`kinds` filter value alongside `request,span,error,revenue`. It never was
-- `ObservabilityEvent` has no `Log` variant (a deliberate, documented
design choice: runtime logs are too high-volume to interleave with
business signals, and have their own retention/storage model), and
`EventKind::parse("log")` returns `None`, so `kinds=log` has always 400'd
with `InvalidKindsFilter`. Fixed the doc comment in
`crates/temps-observability/src/handlers/events.rs` to match what the code
has always actually done, instead of leaving a promise nothing implements
for the next reader (human or agent) to trip over. Step 4 asserts the real
behavior directly: `kinds=span` finds the span, `kinds=log` 400s.

**Real bug found and fixed (third, minor)**: `query_traces`'s utoipa
`#[utoipa::path(params(...))]` list (`crates/temps-otel/src/handlers/query_handler.rs`)
was missing `attributes` and `name_pattern` -- both real, functioning query
params on `TraceQueryParams` (and both already correctly documented on the
neighboring `query_trace_summaries` handler a few lines below), silently
absent from the generated OpenAPI spec and therefore from `@temps-sdk/api`'s
typed `queryTraces()` signature: a real server capability the typed SDK
couldn't express. Fixed by adding both to the params list. This scenario
doesn't depend on the fix (it filters the quota canary span by `trace_id`,
which was already typed correctly) -- found while checking why `name_pattern`
wasn't available on the typed client during development.

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

**Generic outbound webhooks** — same guard, same conclusion, no
`webhook-scenario` command. `POST /projects/{project_id}/webhooks` runs every
URL (create *and* update) through
`WebhookService::validate_webhook_url` (`crates/temps-webhooks/src/service.rs`),
which calls the exact same `temps_core::url_validation::validate_external_url`
Slack goes through, then `validate_domain_async` for the DNS-resolved case.
Unlike the DNS-01/Pebble guard in `crates/temps-dns/src/providers/pebble.rs`
(which has an explicit `TEMPS_ALLOW_PEBBLE_PROVIDER=1` opt-in for exactly this
kind of test), there is no dev/test bypass anywhere in the webhooks path —
grepped `crates/temps-webhooks/` and `crates/temps-core/src/url_validation.rs`
for `TEMPS_ALLOW*`, `debug_assertions`, `is_dev`/`dev_mode`/`test_mode`, and
found nothing. A local receiver is also unreachable via the DNS-rebinding
route: actual delivery in `deliver_webhook` re-resolves and pins the HTTP
client to the validated IPs (`delivery_client_for`), so a domain that
resolves publicly at create-time and privately at delivery-time is rejected
too. Net effect: no loopback, RFC1918, or link-local target — including
anything inside `docker-compose.e2e.yml`'s bridge network — can ever be a
legal webhook URL against a real instance, create or update. The signing math
(`sha256=` HMAC over `{timestamp}.{payload}`) is covered by
`test_signature_generation` in `crates/temps-webhooks/src/service.rs`, which
is the right layer for that piece, same as Slack's `wiremock` unit test one
section up. Proving actual delivery end-to-end would need a real public receiver (e.g. a
tunnel or hosted catcher) wired into the harness, which does not exist in
this repo today. It was deliberately not added, and the SSRF guard was not
weakened, for test convenience — the same call made for Slack above.

## Notes

- The load engine is pure `fetch`, worker-pooled (exactly `--concurrency`
  in-flight). Transient connection failures are retried (`--connectRetries`
  equivalent); real HTTP 4xx/5xx are recorded as-is.
- Resources are name-prefixed `e2e-<runid>` so leftovers are identifiable.
