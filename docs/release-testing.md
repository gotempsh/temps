# Release E2E Test Scenarios

Concrete user-facing flows that must pass before tagging a `temps` release.
Each scenario has **setup**, **steps**, and **pass criteria**. Run against a
fresh staging instance unless noted. If a scenario fails, do not tag — fix
forward or revert the offending PR.

Conventions used below:

- `temps` = the server binary; `cli` = `bunx @temps-sdk/cli`
- "fresh staging" = clean control plane + 1 worker, no projects, no services
- "sample app" = `https://github.com/dviejokfs/sandbox-test-nextjs` (PortBoard)
- All HTTP checks use the cookie auth set by the UI login — assert the JSON
  body, not just the status code

---

## 1. Onboarding & auth

### 1.1 First-run setup

- **Setup**: Fresh DB, no users.
- **Steps**:
  1. `temps setup --non-interactive --email admin@example.com --password 'P4ssw0rd!'`
  2. Open `/`
- **Pass**:
  - Login form appears (no setup wizard re-run)
  - Login with the seeded credentials lands on `/projects` (empty state)
  - `users` table has exactly one row, role = admin

### 1.2 Login session persistence

- **Setup**: Logged-in admin, browser closed and reopened within session TTL.
- **Steps**: Navigate to `/storage`.
- **Pass**: Page loads without a redirect to `/login`. Cookie is `httpOnly`, `Secure` (when TLS on), `SameSite=Lax`.

### 1.3 Permission denial

- **Setup**: Create a second user with role `viewer`.
- **Steps**: Login as viewer. On `/storage/:id`, click **Edit limits**.
- **Pass**: Button is disabled OR a 403 toast fires; no `PATCH /external-services/:id/resources` request goes out.

### 1.4 CLI login round-trip

- **Setup**: Logged out CLI on a clean machine.
- **Steps**: `cli login` (interactive) → complete in browser.
- **Pass**:
  - `~/.temps/.contexts.json` exists with the new context as `active`
  - `cli projects list` returns the seeded project list
  - `cli logout` removes the active context entry; `cli projects list` then errors with "not authenticated"

---

## 2. Project lifecycle

### 2.1 Git push deploy (happy path)

- **Setup**: Empty project linked to the sample app's `main` branch.
- **Steps**:
  1. `git push origin main` with a one-line README change
  2. Watch `/projects/:slug` deployments list
- **Pass**:
  - New deployment row appears within 5s of the push (webhook fires)
  - Phases progress: `cloning → building → image_built → deploying → succeeded` in order, no skips
  - Final container is reachable on the auto-assigned `*.temps.local` route within 30s after `succeeded`
  - `/health` on the deployed app returns 200

### 2.2 Build failure surfaces correctly

- **Setup**: Same project, push a commit that breaks `bun install` (e.g., add `"missing-pkg": "^1.0.0"`).
- **Steps**: `git push`.
- **Pass**:
  - Deployment reaches `failed`
  - Logs page shows the exact `bun install` error message (not "build failed")
  - Previous deployment stays live and reachable at the project URL
  - Error tracking shows zero new error groups (build failures are not exceptions)

### 2.3 Rollback

- **Setup**: Two successful deployments (D1, D2). D2 is current.
- **Steps**: On `/projects/:slug`, click "Rollback to D1".
- **Pass**:
  - D1 becomes current within 10s
  - The container running D2 is removed; D1 container remains
  - The project URL serves D1's response body
  - `audit_logs` has a `deployment_promoted` row with the actor and source/target deploy IDs

### 2.4 Preview environment

- **Setup**: Project with PR previews enabled.
- **Steps**: Open a PR on a feature branch.
- **Pass**:
  - Preview deployment appears tagged `pr-<number>`
  - Preview URL of shape `pr-<number>-<slug>.<domain>` resolves and serves the PR's build
  - Closing the PR removes the preview within 60s
  - `environments` table no longer has the preview row

### 2.5 Promotion across environments

- **Setup**: Project with `staging` and `production` environments configured.
- **Steps**: Push to staging, verify, then click "Promote to production" on the staging deployment.
- **Pass**:
  - Production deployment uses the **same image hash** as staging (no rebuild)
  - Production env vars override staging where defined; rest are inherited
  - Promotion completes in < 30s (it's a container swap, not a build)

---

## 3. External services (storage)

### 3.1 Provision Postgres

- **Setup**: No services.
- **Steps**: `/storage` → "Create Service" → Postgres → defaults.
- **Pass**:
  - Service status reaches `running` within 60s
  - Health probe goes `operational` within 90s
  - `psql` connect using the env vars from the service detail page returns a row from `pg_stat_activity`
  - Service appears in the `service_members` table with role `standalone`

### 3.2 Resource limits — apply CPU + memory cap on a running container

- **Setup**: Running Postgres service.
- **Steps**:
  1. `/storage/:id` → "Edit limits"
  2. Toggle Memory on, set 512 MiB. Toggle CPU on, set 2 cores. Save.
- **Pass**:
  - Toast: "Resource limits applied · 1 live"
  - `docker inspect <container> --format '{{.HostConfig.Memory}}'` returns `536870912`
  - `docker inspect <container> --format '{{.HostConfig.NanoCpus}}'` returns `2000000000`
  - Resources panel CPU bar reads `X.X% of 2 cores capped (host N)`
  - Resources panel Memory bar shows usage / 512 MiB (no "of host RAM" caveat)
  - Container restart count is unchanged (live update, no restart)
  - `external_services.config` decrypted JSON has `resources: { memory_mb: 512, memory_swap_mb: 512, nano_cpus: 2000000000 }`

### 3.3 Resource limits — switch memory back to unlimited on running container

- **Setup**: Service from 3.2 (memory cap = 512 MiB).
- **Steps**: Edit limits → toggle Memory off. Save.
- **Pass**:
  - Toast: "Limits saved — restart required"
  - `docker inspect` still shows `Memory: 536870912` (Docker can't hot-remove)
  - Resources panel shows the cap until you restart the container
  - After `temps services restart <id>`, panel switches to "Unlimited", `docker inspect` Memory = 0

### 3.4 Resource limits — OOM kill is observed

- **Setup**: Postgres with memory cap 256 MiB (intentionally too small).
- **Steps**:
  1. Inside container: `pg_dump` of a large database to force memory pressure (or `stress --vm-bytes 300M`)
  2. Wait 30s
- **Pass**:
  - Container restart count increments by ≥ 1
  - `oom_killed: true` on the runtime endpoint at least once
  - Resources panel "OOM-killed" badge appears (red)
  - Container restarts automatically (RestartPolicy::ALWAYS); status returns to `running`

### 3.5 Backup → restore in-place (Postgres)

- **Setup**: Postgres service with a known table `users` containing 100 rows.
- **Steps**:
  1. `/storage/:id` → "Trigger backup". Wait for `succeeded`.
  2. `INSERT INTO users` 50 more rows. Confirm count = 150.
  3. "Restore" → pick the backup → in-place mode.
- **Pass**:
  - Restore reaches `succeeded`
  - `SELECT count(*) FROM users` returns **100** (pre-backup state)
  - `external_service_backups.size_bytes` is non-zero
  - S3 prefix listed has `base_*`, `wal_*` objects (WAL-G layout)

### 3.6 Backup → restore to new service (Postgres)

- **Setup**: Same as 3.5.
- **Steps**: "Restore" → "To new service" → name `pg-restored`.
- **Pass**:
  - New `external_services` row appears with name `pg-restored`, status `running`
  - New container has its own host port (no conflict with the original)
  - `psql` against `pg-restored` shows the 100 rows
  - Original service is untouched (still 150 rows or current state)

### 3.7 PITR (Postgres only)

- **Setup**: Postgres with WAL archiving enabled. Make 3 distinct INSERTTs spaced 60s apart, noting timestamps T1, T2, T3.
- **Steps**: Restore → PITR → target = T2 (between INSERT 2 and INSERT 3) → to new service.
- **Pass**:
  - Restored service contains rows from INSERT 1 and 2, but not 3
  - `pg_last_wal_replay_lsn()` is at or before T3's WAL position

### 3.8 Major upgrade (Postgres pg17 → pg18)

- **Setup**: Postgres service on `gotempsh/postgres-walg:17-bookworm` with test data.
- **Steps**: `/storage/:id` → "Upgrade" → select `18-bookworm`.
- **Pass**:
  - Upgrade reaches `succeeded` (15-30 min depending on data size)
  - `SELECT version()` reports PG 18
  - All test data is preserved
  - `pg_upgrade` log artifact is available in the upgrade detail view
  - Original volume backed up to `<volume>_backup_*` and the upgraded data is on a fresh volume

### 3.9 Redis ACL + persistence

- **Setup**: Redis service with password.
- **Steps**:
  1. `redis-cli -a <password>` from outside the cluster network → `SET foo bar`
  2. `temps services restart <id>` → reconnect → `GET foo`
- **Pass**:
  - `GET foo` returns `bar` (RDB persistence works)
  - Same call without `-a` returns `NOAUTH Authentication required.`

### 3.10 MongoDB replica set transactions

- **Setup**: MongoDB service with `replica_set: rs0`.
- **Steps**: From a deployed app, run a multi-document transaction.
- **Pass**:
  - `db.serverStatus().repl.setName === "rs0"`
  - Transaction commits without `Transaction numbers are only allowed on a replica set member or mongos` error

### 3.11 S3/RustFS bucket operations

- **Setup**: RustFS service.
- **Steps**: `mc cp` a 50MB file using the service's access key/secret.
- **Pass**:
  - Upload completes, `mc stat` shows correct size + ETag
  - File is readable from a deployed app via env vars
  - `mc rm` removes it; subsequent `mc stat` 404s

### 3.12 Postgres HA cluster — failover

- **Setup**: Postgres HA cluster (1 monitor + 1 primary + 1 replica).
- **Steps**:
  1. Connect via the cluster connection string (`target_session_attrs=read-write`)
  2. `INSERT INTO test VALUES (1)` — succeeds
  3. `docker kill <primary_container>` — primary down
  4. Wait 30s, then INSERT another row using the **same connection string**
- **Pass**:
  - pg_auto_failover promotes the replica within 30s (`pg_autoctl show state`)
  - Insert in step 4 succeeds (libpq retries land on the new primary)
  - Resources panel shows the killed container's restart count incremented
  - `service_members.role` for the promoted node updates to `primary` within one reconciler tick

### 3.13 Postgres HA cluster — scale up

- **Setup**: Cluster from 3.12 (post-failover, 1 primary + 1 replica/dead).
- **Steps**: "Add member" → role replica → ordinal 2.
- **Pass**:
  - New replica reaches `running` within 90s
  - `pg_autoctl show nodes` lists 3 data nodes (or 2 + 1 dead)
  - Replication lag drops to < 1s within 30s of the new replica catching up

---

## 4. Multi-node

### 4.1 Worker join (direct mode)

- **Setup**: Control plane reachable on a private network. Fresh worker host.
- **Steps**: `temps join --private-address <ip>` on the worker.
- **Pass**:
  - `nodes` table has the new worker, status `online`
  - `temps agent` runs as a systemd unit on the worker
  - Control plane can `inspect_container` against the worker via the agent API
  - No WireGuard interface created (direct mode)

### 4.2 Worker join (relay mode via WireGuard)

- **Setup**: Worker behind NAT.
- **Steps**: `temps join` (no `--private-address`).
- **Pass**:
  - WireGuard interface `wg-temps` exists on the worker (`ip a`)
  - Control plane and worker can ping each other on the WireGuard subnet
  - `nodes.public_endpoint` and `wg_public_key` are populated
  - Deployments scheduled on this worker actually land there (`docker ps` on worker shows the container)

### 4.3 Cross-node deployment scheduling

- **Setup**: Control plane + 2 workers, both online.
- **Steps**: Deploy 5 services. Watch where they land.
- **Pass**:
  - LeastLoaded scheduler distributes them roughly evenly (no node gets all 5)
  - Deploying a service with explicit `node_id` lands on that node only

### 4.4 Node drain

- **Setup**: Worker with 3 running deployments.
- **Steps**: `temps node drain <node_id>` from the control plane.
- **Pass**:
  - All 3 containers re-deploy onto another node within 5 minutes
  - Original node's `docker ps` shows no temps-managed containers
  - `nodes.status` becomes `draining`, then `offline` once empty
  - End-user requests served by the drained app see at most one connection error during the swap

### 4.5 Node failure recovery

- **Setup**: Worker with running services, then `systemctl stop temps-agent` on it.
- **Steps**: Wait for the health-check window (default ~60s).
- **Pass**:
  - `nodes.status` flips to `offline`
  - Services on that node show `health_status = down` within 90s
  - Restarting the agent flips status back to `online` and `health_status = operational`

---

## 5. Domains, TLS, and proxy

### 5.1 Custom domain with auto-TLS

- **Setup**: Project deployed and reachable on `*.temps.local`. Own a real domain pointing at the control plane IP.
- **Steps**: `/domains` → "Add domain" → enter `app.example.com` → wait.
- **Pass**:
  - Cert issued via Let's Encrypt within 90s
  - `curl https://app.example.com` returns the project's response with a valid TLS cert (`openssl s_client` shows a chain to ISRG Root X1)
  - HTTP→HTTPS redirect: `curl http://app.example.com` returns 301 to https
  - Cert renewal scheduled in `domains` table at < 60 days

### 5.2 Wildcard cert renewal under load

- **Setup**: Domain with cert expiring within 7 days. Active traffic at 100 RPS.
- **Steps**: Trigger renewal (`temps domains renew <domain>` or wait for the scheduler).
- **Pass**:
  - Renewal succeeds without dropping in-flight TLS connections (Pingora hot-reloads the cert)
  - `error_rate` stays under 0.1% during the renewal window
  - New cert is served on the next handshake; old cert is no longer in memory

### 5.3 Workspace preview routing

- **Setup**: Active workspace session with public ID `wss_<16hex>` and exposed port 3000.
- **Steps**: Open `https://ws-<16hex>-3000.<domain>`.
- **Pass**:
  - Page loads (no 404)
  - Without preview password set: anonymous access works
  - With preview password set: 401 challenge until correct password entered
  - With session expired: 409 NotConfigured page (not 404)

### 5.4 Sandbox preview routing

- **Setup**: Active sandbox with public ID containing the hex suffix.
- **Steps**: Hit `https://<hex-suffix>.<domain>`.
- **Pass**:
  - Routes to the sandbox's exposed port without auth (sandboxes bypass user auth)
  - Returns 404 (not 500) if the sandbox is shut down

### 5.5 DNS resolver

- **Setup**: Cluster with multiple Postgres members, all under `*.temps.local`.
- **Steps**: From inside an app container: `dig postgres-mydb-1.mydb.temps.local`.
- **Pass**:
  - Returns the correct compute IP
  - `dig google.com` from the same container returns a public address (Hickory upstream forwarder works)

---

## 6. Sandboxes & workspaces

### 6.1 Sandbox cold start

- **Setup**: Host without the sandbox image cached.
- **Steps**: `/sandboxes` → "New sandbox" → wait.
- **Pass**:
  - Image pull + start < 60s on a warm host, < 3 min on cold
  - Terminal attaches and `whoami` returns `temps`
  - `bunx @temps-sdk/cli projects list` works without sourcing `~/.env`
  - `~/.temps/.contexts.json` and `~/.config/temps-cli-nodejs/config.json` are present

### 6.2 Workspace bind-mount permissions

- **Setup**: Workspace session attached to a project.
- **Steps**: In the workspace shell: `touch /home/temps/workspace/test.txt`.
- **Pass**:
  - File is created, owner = `temps:temps`
  - No `EACCES` despite the host work_dir being root-owned

### 6.3 Sandbox image channel

- **Setup**: Two hosts, one with `TEMPS_SANDBOX_CHANNEL=beta`, the other default.
- **Steps**: Start a sandbox on each.
- **Pass**:
  - Beta host pulls `:beta`; default host pulls `:stable` (or `:vX.Y.Z`)
  - Bumping `SANDBOX_IMAGE_VERSION` and restarting forces re-pull on both

### 6.4 Sandbox terminal reattach

- **Setup**: Sandbox running tmux + a TUI (e.g., `htop`) inside.
- **Steps**: Close the browser tab, reopen `/sandboxes/:id`.
- **Pass**:
  - TUI re-renders correctly (no garbled output from raw byte replay)
  - tmux session persists; htop is still running

---

## 7. Observability

### 7.1 Observe page time-merged stream

- **Setup**: A project with traffic — at least 100 requests, 5 errors, 2 traces, 1 revenue event in the last hour.
- **Steps**: `/projects/:slug/observe` → time range = "Last 1h".
- **Pass**:
  - Sparklines render one per kind (request/trace/error/revenue)
  - Stream is sorted `ts DESC` and contains all four kinds
  - Toggling a kind off via the cockpit chip removes those rows from the stream + URL updates with `?kinds=` param
  - Click an error row → side panel opens with the stack frames already loaded (no follow-up fetch in network tab)

### 7.2 traceparent extraction

- **Setup**: Same project. Send a request with `traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`.
- **Pass**:
  - `proxy_logs.trace_id` for that request = `4bf92f3577b34da6a3ce929d0e0e4736`
  - Observe row links to the matching trace via the correlation column

### 7.3 Sentry captureMessage shows real text

- **Setup**: Deployed app using the Sentry SDK. Trigger `Sentry.captureMessage("custom event")`.
- **Pass**:
  - Error group title in `/observe` reads `custom event` (not `Error: Unknown error`)
  - Error detail page shows the same text
  - Future events from the same SDK call increment the same group's count

### 7.4 Web Vitals

- **Setup**: Deployed React app with `@temps-sdk/react-analytics` initialized.
- **Steps**: Load a page in a real browser.
- **Pass**:
  - `/analytics/performance` shows entries for LCP, FID, CLS, INP within 60s
  - p75 values look reasonable (LCP < 2.5s on a fast page)

### 7.5 Session recording privacy

- **Setup**: App with session recording on, an `<input type="password">` and an element marked `data-temps-block`.
- **Steps**: Type in both, then play back the session.
- **Pass**:
  - Password input renders as masked dots
  - Blocked element shows a placeholder rectangle, not its content
  - Recording size for a 60s session is < 500KB (rrweb compression sane)

---

## 8. CLI

### 8.1 Deploy from CLI

- **Setup**: Logged-in CLI, sample app cloned locally.
- **Steps**: From the app directory: `cli deploy`.
- **Pass**:
  - Same flow as 2.1, but driven from the CLI
  - Logs stream live to stdout; exits 0 on success
  - Exit code = the deployment's terminal status (0 for success, non-zero for failed)

### 8.2 Backup → restore via CLI

- **Setup**: Postgres service with data.
- **Steps**:
  1. `cli backups create <service-id>` → wait
  2. Wipe data
  3. `cli backups restore <backup-id>`
- **Pass**:
  - Same outcome as 3.5, just via CLI
  - Output is parseable as JSON when `--json` flag is passed

### 8.3 Service management

- **Setup**: Running services.
- **Steps**: `cli services list`, `cli services stop <id>`, `cli services start <id>`.
- **Pass**:
  - List shows all services with their statuses
  - Stop transitions the service to `stopped`; container is stopped (not removed)
  - Start brings it back to `running`; data is preserved

### 8.4 CLI auto-upgrade across channels

- **Setup**: CLI installed at version v0.0.X.
- **Steps**: `cli upgrade --channel stable` then `cli upgrade --channel beta`.
- **Pass**:
  - Stable lands on the latest non-prerelease tag
  - Beta lands on the latest tag including prereleases
  - `cli upgrade --version vX.Y.Z` ignores the channel and pins to that version

---

## 9. Migrations & upgrade safety

### 9.1 Server upgrade with no downtime

- **Setup**: Old version running with active traffic + at least one running deployment.
- **Steps**: Stop the old binary, run new binary's migrations (`temps migrate`), start the new binary.
- **Pass**:
  - All migrations succeed
  - Existing containers keep running through the binary swap
  - Active HTTP requests during the swap window see at most one connection error
  - Deployments table shows no spurious `failed` rows from the swap

### 9.2 Worker/control-plane skew

- **Setup**: Control plane on the new version, one worker still on the previous version.
- **Steps**: Try to schedule a deployment on the old worker.
- **Pass**:
  - Deployment succeeds (the agent API is forward/backward compatible across one minor)
  - OR fails with a clear `agent_version_too_old` error — never a panic or partial state

### 9.3 Database rollback safety

- **Setup**: Production DB after migrations applied. Take a `pg_dump`. Roll back the binary one minor.
- **Steps**: Old binary boots against the new schema.
- **Pass**:
  - Old binary either boots cleanly (forward-compatible schema) OR refuses to start with a clear "schema_version_ahead" error
  - Never silently corrupts data

---

## 10. Security gates

### 10.1 Secret masking

- **Setup**: Service with a `password` parameter set to `correcthorsebatterystaple`.
- **Steps**:
  1. GET `/external-services/:id`
  2. `/storage/:id` → expand the password in the Configuration card
- **Pass**:
  - API response shows `***` (never the raw value)
  - UI shows `***` until "Show" is clicked; "Show" then fetches the unmasked value via a separate authenticated endpoint
  - Audit log records the unmask access

### 10.2 SQL injection — query api

- **Setup**: Query API enabled on a Postgres service.
- **Steps**: POST to `/external-services/:id/query` with body `{ "sql": "SELECT 1; DROP TABLE users; --" }`.
- **Pass**:
  - Query returns one row from `SELECT 1`
  - `users` table is **not** dropped (the runner blocks multi-statement queries OR runs in a read-only transaction)

### 10.3 Path-escape on sandbox FS handlers

- **Setup**: Sandbox session.
- **Steps**: POST to the sandbox FS endpoint with path `../../../etc/passwd`.
- **Pass**:
  - Returns 400 with `path_escape_rejected` (or similar)
  - No file content from outside the sandbox is returned

### 10.4 Auth on every write endpoint

- **Setup**: A logged-out HTTP client.
- **Steps**: For every POST/PATCH/DELETE in the OpenAPI doc, send a request without a cookie.
- **Pass**:
  - Every endpoint returns 401 (or 403 if the route requires elevated perms)
  - **Zero** endpoints accept the request anonymously

### 10.5 Rate limiting on auth endpoints

- **Setup**: Logged-out client.
- **Steps**: 100 failed login attempts in 60s from the same IP.
- **Pass**:
  - After ~10 attempts the endpoint returns 429
  - Successful login from a different IP still works (no global lockout)

---

## 11. Performance regression gates

### 11.1 Cold-start latency

- **Setup**: Server just started, no warm caches.
- **Steps**: Measure TTFB on `/projects` for an authenticated user.
- **Pass**: < 500ms on a c7a.large equivalent.

### 11.2 Deployment list scaling

- **Setup**: Project with 1000 deployments.
- **Steps**: Open the deployments page.
- **Pass**: First page renders in < 1s. Pagination works (no SELECT \* without LIMIT).

### 11.3 Observe page query budget

- **Setup**: 24h time range, 100K events across the four kinds.
- **Steps**: Open `/observe`.
- **Pass**:
  - Initial query budget < 2s total wall time
  - No single underlying query > 800ms
  - Result is paginated; scrolling fetches the next page in < 500ms

---

## 12. Service template catalog (Beta)

Run this section for every release that changes the service-template catalog,
Compose deployment path, project source revisions, runtime logs, routes, or
status monitors. Use a fresh staging database and a clean Docker host unless a
scenario explicitly tests an upgrade.

### 12.1 Maturity and discoverability

- **Setup**: Admin and project-creator accounts; no catalog request made yet.
- **Steps**: Open **New Project** and select **Services** with each account.
- **Pass**:
  - Services is visible and carries the canonical **Beta** badge in the source
    picker and catalog title.
  - `GET /api/service-templates` has OpenAPI `x-maturity: beta` and requires
    both project-create and deployment-create permissions.
  - The UI explains Beta limits without hiding Custom Compose or unsupported
    templates.

### 12.2 Catalog availability, bounds, and stale-cache behavior

- **Setup**: A mock catalog endpoint plus a staging host whose outbound access
  to `cdn.coollabs.io` can be toggled.
- **Steps**: Load a valid catalog, exceed the response/entry/Compose limits,
  return invalid JSON, time out, then disable outbound access after one good
  refresh.
- **Pass**:
  - The first valid load is paginated and cached; concurrent readers cause one
    upstream refresh.
  - Oversized, invalid, redirected, and timed-out responses return typed,
    actionable errors without excessive memory or task growth.
  - A prior successful snapshot remains usable during a refresh outage; a
    fresh host shows the retryable connectivity error and keeps Custom Compose
    available.
  - A catalog refresh never mutates an installed project's Compose revision.

### 12.3 Compatibility inventory and host-access boundary

- **Setup**: Current production catalog snapshot.
- **Steps**: Export every template's tier and issue codes; inspect all entries
  containing Docker/Podman sockets, absolute bind mounts, devices, privileged
  mode, host namespaces, external Docker resources, or guarded interpolation.
- **Pass**:
  - `standard + elevated + blocked == catalog_total`; `host_access` is reported
    as an explicit subset of blocked templates.
  - Every raw Docker socket and equivalent host-control request is
    `host_access`, cannot preflight, and cannot be installed through a direct
    API call.
  - The final `ComposeExecutor` policy independently rejects the same document
    even if catalog analysis is bypassed.
  - No template can grant its own capability approval or convert a host path
    through environment interpolation.

### 12.4 Standard single-service installation

- **Setup**: A standard template with one HTTP route and no writable volume.
- **Steps**: Search, inspect, preflight, install, follow deployment logs, and
  open the generated URL.
- **Pass**:
  - Search retains keyboard focus while debounced requests run.
  - Fixed host ports become random loopback bindings and no fixed container or
    Compose project name survives normalization.
  - Preflight returns the planned slug and canonical public variables; create
    claims that exact slug or safely replans after a collision.
  - Deployment succeeds, the route serves the expected response, and both the
    Compose stage log stream and runtime container log stream contain data.

### 12.5 Elevated multi-service installation

- **Setup**: A template with an application plus a bundled Postgres/Redis
  dependency and writable named volumes.
- **Steps**: Try preflight without approval, approve only the named services,
  install, restart all containers, and redeploy unchanged.
- **Pass**:
  - Missing approval blocks preflight with each exact service/reason; unrelated
    approval is ignored with a warning.
  - Only the limited startup capability profile is restored—never privileged
    mode, arbitrary `cap_add`, host networking, or devices.
  - Dependencies remain project-scoped, persist across restart/redeploy, and
    do not collide with another project installed from the same template.

### 12.6 Variables, defaults, and secrets

- **Setup**: Templates containing optional variables, required `${VAR?}` and
  `${VAR:?}` expressions, generated credentials, shared literal bootstrap
  credentials, URL/FQDN variables, and Supabase JWT dependencies.
- **Steps**: Leave optional fields empty, omit each required field, generate
  values twice, install, then inspect API responses, deployment files, logs,
  and the database.
- **Pass**:
  - Optional variables have no required marker and do not block preflight;
    required variables fail with their exact name.
  - Generated values use Web Crypto, preserve required equality, and are never
    returned by catalog/preflight responses or printed in logs.
  - Stored secrets are encrypted/write-only and Compose receives the exact
    value, including quotes, dollar signs, and Unicode, without interpolation.

### 12.7 Routes, health, and monitor convergence

- **Setup**: Templates whose root path returns 404, whose Compose healthcheck
  targets a non-root HTTP path, and whose stack exposes multiple services.
- **Steps**: Install each template and observe deployment, route, and monitor
  state from cold start through healthy.
- **Pass**:
  - The detected health path is snapshotted on the deployment and used by the
    managed monitor; a non-200 `/` does not mark an otherwise healthy service
    down.
  - Deployment success triggers an immediate monitor check/reconciliation;
    the project header and monitor detail converge without waiting for a stale
    pre-deployment interval.
  - Each supported public service receives the correct route and container
    port; ambiguous multi-port services remain blocked with a specific reason.

### 12.8 Editable source, redeploy, and rollback

- **Setup**: A successfully installed template with image tag `v1`.
- **Steps**: Change the stored Compose source to `v2`, save, deploy, redeploy
  the `v2` deployment, then redeploy/roll back to the historical `v1`
  deployment.
- **Pass**:
  - Build settings identify the source as **Compose**, show editable YAML, and
    save an immutable revision using optimistic concurrency.
  - Each deployment uses its selected source revision, Compose path, working
    directory, environment snapshot, public services, and health path—not the
    latest mutable project fields.
  - Historical redeploy restores `v1`; a stale editor save returns 409 without
    losing either revision.

### 12.9 Failure, cancellation, and concurrent deployment safety

- **Setup**: One live stack plus candidate revisions that fail during pull,
  create, and health wait; two simultaneous deploy requests.
- **Steps**: Fail/cancel before teardown and after teardown begins; race two
  deployments and restart the workflow executor between attempts.
- **Pass**:
  - Pre-teardown failure leaves the old stack and secrets live.
  - Post-teardown failure/cancellation performs compensating container,
    network, candidate-secret, and temporary-source cleanup with a contextual
    deployment error.
  - The per-data-directory/Compose-project lock serializes all executor
    instances; no orphan stack or plaintext generation remains.

### 12.10 Lifecycle cleanup and resource isolation

- **Setup**: Two projects from the same template, including volumes, networks,
  env files, and random loopback ports.
- **Steps**: Deploy both, delete one, reinstall it, and run source/stack garbage
  collection.
- **Pass**:
  - Project names, containers, networks, volumes, ports, and source generations
    do not collide.
  - Delete removes only the selected project's containers/network and preserves
    data according to the explicit volume-retention contract.
  - Temporary preflight directories and obsolete secret/source generations are
    bounded and reclaimed.

### 12.11 Architecture, remote worker, and resource pressure

- **Setup**: amd64 and arm64 workers plus the 3-vCPU/4-GB reference host.
- **Steps**: Preflight architecture-pinned templates, deploy a representative
  standard/elevated matrix locally and remotely, refresh the catalog under 50
  concurrent readers, and install two stacks concurrently.
- **Pass**:
  - Architecture mismatches fail before project creation; matching remote
    workers receive the complete immutable source bundle and policy metadata.
  - Catalog analysis does not block the async runtime, respects the four-slot
    preflight limit, and stays within the documented response/memory bounds.
  - Proxy latency and unrelated deployments remain within their release SLOs.

### 12.12 Beta release gate

The feature may ship as Beta only when all scenarios above that apply to the
changed surface pass, zero critical/high security findings remain, and the
qualification matrix records three cold installs for at least 30 installable
templates spanning every represented category, bundled dependency kind, and
both compatibility tiers. Record every failure with template revision,
architecture, stage, and sanitized logs; do not relabel a failing template as
supported to improve the percentage.

### 12.13 Stable graduation gate

Remove the Beta label only after all of the following are true for two
consecutive releases:

- A pinned nightly matrix automatically preflights every catalog entry and
  cold-deploys every installable entry on amd64; architecture-compatible entries
  also run on arm64.
- At least 95% of entries classified installable complete three consecutive
  cold installs, reach their declared health target, stream deployment and
  runtime logs, and converge their managed monitor. Any remaining failure is
  reclassified with a reviewed, actionable reason before release.
- Source edit/redeploy/rollback, cancellation compensation, catalog outage,
  catalog drift, RBAC, and cross-project isolation have dedicated automated
  end-to-end coverage.
- A seven-day staging soak shows no orphan containers/networks, plaintext
  generations, stuck deployments, stale monitors, or unbounded catalog/cache
  growth.
- The public API/source-revision contract is frozen for the documented
  compatibility window, upgrade/rollback migrations are proven, operator and
  troubleshooting docs are published, and security review approves the final
  host-access boundary.

## How to use this document

1. Open a `RELEASE_CHECKLIST_vX.Y.Z.md` for the release in flight.
2. **Run** every scenario above against staging. Tick the boxes as you go.
3. Skip with a written reason any scenario that does not apply (e.g., "no
   migrations this release"). Never skip silently.
4. **Add a new scenario** at the bottom of the relevant section in this file
   in the same PR that fixes any regression that bit you in production.
   The scenarios should grow with every "we should have caught this" lesson.
