# Release E2E Test Scenarios

Concrete user-facing flows that must pass before tagging a `temps` release.
Each scenario has **setup**, **steps**, and **pass criteria**. Run against a
fresh staging instance unless noted. If a scenario fails, do not tag — fix
forward or revert the offending PR.

Conventions used below:

- `temps` = the server binary; `cli` = `bunx @temps-sdk/cli`
- "fresh staging" = disposable single-node control plane, no projects or services;
  add run-owned workers only for explicitly multi-node scenarios
- "sample app" = a generic owned test repository with an HTTP health endpoint
- All HTTP checks use the cookie auth set by the UI login — assert the JSON
  body, not just the status code

---

## v0.1.0 qualification and promotion record

Tracking: [#1195](https://github.com/gotempsh/temps/issues/1195),
[#1214](https://github.com/gotempsh/temps/issues/1214), and
[#1215](https://github.com/gotempsh/temps/issues/1215). This checklist is a runbook,
not evidence of completion. **RC publication and stable promotion remain blocked
until the gates below have actual, reviewed results.** Do not infer qualification
from merged fixes, a green unit test, a maturity badge, or a short idle soak.

Record the exact source commit, candidate tag, binary/image digests, platform,
database version, fixture identities, test commands, UTC start/end times,
expected/actual results, and reviewer for each run. Keep raw reports, credentials,
logs and user feedback private; link sanitized evidence from the release record.
Use `untested`, `passed`, `failed`, `blocked`, or `inconclusive`, and give every
blocked/inconclusive result a reason and owner. A new candidate invalidates prior
candidate results unless the reviewer explicitly documents why evidence applies.

### Before publishing rc.1 (or a replacement RC)

| Gate | Required evidence | Current qualification state |
| --- | --- | --- |
| Combined integration CI | Required Rust, console, CLI, generated SDK, attribution and security checks on the integrated commit; links to full results and runtime skip counts | Untested for the final combined candidate |
| Fresh installation | Empty database and first-admin bootstrap; Docker image, Git/Dockerfile, Node, Compose and static deploy flows; UI/CLI feedback; actual proxy responses, service readback and restart persistence | Untested for the final combined candidate |
| Seeded upgrade from v0.0.8 | Previous released binary and candidate digests; migrations, pre-migration backup, projects/environments/secrets/services/deployments preserved; app served through proxy | Untested |
| Actual rollback to v0.0.8 | Restore pre-migration database and matching data directory with the old binary, read back seeded data and app response; verify schema-ahead refusal without data mutation | Untested |
| Healthy 24-hour workload soak | Continuous workload health, deployed app requests, service checks, deploy/error/upgrade metrics, bounded memory/disk/log growth, no unexplained restart or stuck jobs; log report and UTC window | Untested; no 24-hour run claimed |
| Security | GA-scope findings reviewed, external penetration test report and remediation/retest, authorization/SSRF/secret masking/defaults reviewed independently of labels | External assessment and sign-off pending (#1210, #1216) |
| Release packaging | Required architecture binaries/images, checksums, signatures, SBOM and provenance verified for candidate artifacts; installer channel behavior and upgrade policy reviewed | Candidate artifact verification pending (#1207, #1208, #1213) |
| Maturity and actual defaults | Maintainer approves scope; source registry, UI/API/CLI consistency and each experimental capability's actual enablement, prerequisites and authorization checked | Decision and defaults audit pending (#1214) |

Use the existing `scripts/first-run/run-first-run.sh` and
`scripts/test-upgrade-from-release.sh` as executable starting points; inspect their
prerequisites before running. The upgrade harness expects the previous binary
to expose `migrate`; the checksum-verified macOS arm64 v0.0.8 binary does **not**
(`temps migrate --help` rejects the subcommand). Therefore this harness cannot
qualify v0.0.8 unchanged. Adapt its old-version bootstrap/seeding path, or define
and verify an explicit supported intermediate upgrade route, before using it as
evidence for #1195. Set `OLD_BIN` to the verified previous binary and `NEW_BIN`
to the candidate only after establishing command/API compatibility. Use a
dedicated test database container/engine and unused database names. Never stop,
remove or mutate operator-owned Docker resources. Additional feature-specific scenarios below remain required
for shipped scope; automation definitions alone do not count as successful runs.

The published [v0.0.8 assets](https://github.com/gotempsh/temps/releases/tag/v0.0.8)
include Linux amd64 and macOS amd64/arm64 archives, but no Linux arm64 archive.
Record the architecture actually upgraded and rolled back. A Linux arm64
migration route needs a separately reviewed strategy and execution evidence;
other architecture results do not establish that route.

The quiet-log harness (`SOAK_MINUTES=1440
scripts/first-run/quiet-logs-soak.sh`) checks a small idle fixture and end-of-window
health. It is **only part of** the healthy workload soak: collect periodic health,
request/deploy/error counts, resource usage and restart history throughout the
window. Twenty minutes in CI cannot establish 24-hour reliability. The full run
needs a supervised dedicated host; keep the workload and evidence isolated.

### After RC publication, before stable promotion

- Freeze the candidate scope and document exceptions. A maintainer publishes
  the qualified RC; this checklist and its checker never publish anything.
- Invite beta users to upgrade through the approved communication channel.
  Observe **5–7 actual days** of tester feedback after publication, recording
  upgraded versions, upgrade outcomes, deploy success, error summaries and
  unresolved regressions. No responses is not affirmative compatibility evidence.
- If a blocker requires rc.2, document the fix and repeat affected qualification;
  the reviewer decides and records whether the feedback window must restart.
- Reverify the published artifact digests, signatures/SBOM/provenance, supported
  architecture downloads, and stable installer selection. Verify that stable
  selection excludes prereleases and pinned upgrades select the requested version.
- Publish reviewed release notes (generated changelog plus verified limitations),
  final GA/Beta/Experimental list, and a link to the supported compatibility and
  upgrade/rollback policy. Never hand-edit `CHANGELOG.md`.
- Name the patch owner, security reporting route, and **30-day patch window**
  starting at the actual stable publication time, with an explicit end date.
  Regressions must have severity, reproduction, mitigation and patch/retest plan.
- Require a named maintainer's promotion approval after reviewing all evidence.
  Do not publish automatically, merge the qualification PR, or close operational
  issues merely because the checklist is present. beta.57 is optional.

### Private evidence ledger

The repository includes a fail-closed completeness checker. Initialize outside
the checkout so raw operational evidence cannot enter a commit:

```bash
python3 scripts/check-release-qualification.py /tmp/temps-v010-evidence/ledger.json --init
python3 scripts/check-release-qualification.py /tmp/temps-v010-evidence/ledger.json \
  --phase rc --commit "$(git rev-parse HEAD)"
# After RC feedback and the remaining promotion gates:
python3 scripts/check-release-qualification.py /tmp/temps-v010-evidence/ledger.json \
  --phase stable --commit "$(git rev-parse HEAD)"
```

Initialization marks every gate untested. Fill `candidate_commit`,
`candidate_version`, and each gate's `status`, `reviewer`, and `evidence` list.
Each evidence item contains a path relative to the ledger directory and the
file's lowercase SHA-256 (`sha256sum` or `shasum -a 256`). For `workload-soak` and
`tester-feedback`, also record timezone-qualified `started_at` and `ended_at`.
The checker rejects missing gates, missing/changed files, stale source commits,
short windows and future end times. It cannot authenticate reports or determine
whether a workload was healthy; a reviewer must inspect the evidence and verify
candidate artifact identities. A zero exit code is not authorization to release.

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

### 4.2 Worker join over a WireGuard network (direct mode)

Built-in relay mode (`temps join <cluster-id> --relay-url`) is not usable yet
(no relay ships with Temps, and its tunnel does not outlive `temps join`), so
test the supported WireGuard setup: an operator-run tunnel plus direct mode.

- **Setup**: Worker behind NAT with no inbound ports, connected to the control
  plane only by a WireGuard tunnel (`wg0`); see the multi-node docs,
  "Adding a worker node (WireGuard mode)".
- **Steps**: `temps network setup-multi-node --private-address <cp-wg-ip> --underlay-dev wg0`
  on the control plane, then `temps join <url> <token> --private-address <worker-wg-ip>`
  on the worker.
- **Pass**:
  - The node registers with its tunnel address and goes `active`
  - The worker's `vxlan-temps0` uses `wg0` and an MTU of at most the `wg0` MTU minus 50
  - Deployments scheduled on this worker land there and answer through the control plane's proxy
- **Automated**: `bun run src/index.ts multinode-join-scenario --topology wireguard`
  in `apps/temps-e2e` runs all of the above with nodes that share no network
  (`docs/howto/test-multi-node-wireguard/page.mdx`).

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
  - `docker rm -f temps-preview-gateway temps-preview-gateway-ingress`: the
    preview URL shows the "Preview Unavailable" page (not "check that your
    deployment is running"), and the gateway is back within about a minute
    without a restart
  - Hold its host port (`python3 -m http.server 8090 --bind 127.0.0.1`) and
    remove it again: **AI runtime settings → Preview Gateway** shows "Temps could
    not set the gateway up" with Docker's "address already in use" and the
    next retry time; freeing the port brings it back at that retry

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
  - Old binary boots and logs a WARN that the database schema is newer than the binary, naming the unknown migrations and linking to the rollback docs
  - The unknown migrations stay recorded in `seaql_migrations` (nothing is rolled back) and no migration error is raised
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

### 10.6 Restore source identity and authorization (#1210)

- **Setup**: Two disposable projects with separately owned managed services;
  a backup from service A and an in-place restore target B. Use credentials
  entitled to both services, then repeat with credentials entitled only to B.
- **Steps**: Restore A's backup onto B without `confirm_cross_service`, then
  explicitly confirm and retry. Repeat with a backup location whose source
  identity cannot be established. Exercise both in-place and PITR modes.
- **Pass**:
  - An authorized caller must explicitly confirm a destructive restore from a
    different or unknown source; rejection starts no restore or target mutation.
  - A caller lacking source access is denied even with confirmation and is not
    shown source identity or prompted for MFA before authorization succeeds.
  - Own-source restores work without cross-service confirmation; destructive
    modes still require enrolled MFA step-up when applicable.
  - Audit records identify the authorized source, target, and confirmation.
- **Existing regression coverage**: `crates/temps-backup/src/services/restore.rs`
  tests `RestoreOriginBinding`; handler authorization and confirmation precede
  step-up in `crates/temps-backup/src/handlers/restore_handler.rs`.

### 10.7 Error-group mutations and agent credentials (#1210)

- **Steps**: Update an error group's status with project-authorized credentials,
  repeat with a different project's group ID, and run an agent workflow.
- **Pass**:
  - A successful status change emits `ERROR_GROUP_UPDATED` with actor,
    project/group IDs, and new status. A refused update emits no success event.
  - Agent workflows receive no API token while they require no API permissions.
    If a workflow needs a token in future, it must receive only purpose-built
    permissions, project scope, bounded expiry, and completion/cancellation
    revocation; it must never receive `FullAccess` by default.
- **Existing regression coverage**: `crates/temps-error-tracking/src/handlers/handler.rs`
  tests audit emission; `crates/temps-ai-agents/src/services/executor.rs` tests
  minimal run permissions, absent issuance, and token cleanup.

### 10.8 Metrics injection and alert capacity (#1210)

- **Steps**: In a disposable TimescaleDB schema, round-trip metric engine,
  environment, label keys, and label values containing quotes and backslashes.
  Submit an invalid metric name. Fill a managed service's alert capacity, then
  attempt concurrent creates and enable built-in alerts with insufficient room.
- **Pass**:
  - Accepted strings round-trip as data, invalid names are rejected/dropped,
    and no supplied value changes query structure or executes SQL.
  - At most 100 alert rules exist per managed service, including built-ins;
    concurrent requests cannot bypass the cap. Capacity rejection leaves no
    partial built-in rule set and does not retain the creation lock.
  - Another service retains its independent capacity. This implementation is
    **per service**, not an aggregate per-project cap; a project-wide policy
    would need separately defined semantics for shared managed services.
- **Existing regression coverage**: `crates/temps-metrics/src/store/timescale.rs`
  has SQL binding and real-DB hostile-value tests;
  `crates/temps-providers/src/handlers/metrics_handlers.rs` covers concurrent
  creation, built-in capacity and rollback;
  `crates/temps-alarms/src/default_rules.rs` tests the built-in rule catalog.

### 10.9 Dependency exception review and HA boundary (#1210)

- **Steps**: Run `cargo audit` against the release lockfile with a freshly
  fetched advisory database. Compare every result with the explicit exceptions
  in `.github/workflows/dependency-scan.yml` and the assessments in `Cargo.toml`.
  Check the resolved production feature graph with
  `cargo tree --locked --target all -e normal -i rkyv` and
  `cargo tree --locked --target all -e normal -i lru@0.16.4`.
- **Pass**:
  - No new finding is silently ignored. Each retained exception has a reviewed
    dependency path, reachability assessment, and removal/re-review trigger.
    A passing scan with exceptions is not an independent security assessment.
  - rkyv must remain absent from the production feature graph for its current
    non-reachability rationale to hold. Re-review AWS SDK cache keys/operations
    and RSA private-key use when dependencies or consumers change.
- **Review snapshot (2026-10-06, main `e70c36a78`)**: cargo-audit 0.22.2 with
  advisory-db `ef6173cbc5c50ec8166f9a5b28f07834144373ee` reported RSA in both
  locked versions, rkyv, lru unsoundness, and four unmaintained crates. All are
  already listed exceptions; the scan also warned that locked chacha20 0.10.0
  and spin 0.9.8/0.10.0 are yanked. rkyv had no normal-dependency consumers. The
  locked AWS SDK's only lru cache uses `CacheKey(String)` without custom Drop
  and `get_or_insert_mut`, so the advisory's panicking-key prerequisite was
  absent in that reviewed consumer. RSA risk acceptance remains a maintainer
  gate; no independent timing assessment is available.
- **HA/multi-node boundary**: New HA infrastructure roles use SCRAM, and legacy
  roles have a phased credential/HBA upgrade in
  `crates/temps-providers/src/externalsvc/postgres_cluster.rs`, with permanent
  SCRAM guards before regenerated peer rules. This does not close every
  ADR-020 finding. HA TLS identity, role privileges, network scoping and
  fencing require their own qualification before multi-node GA, as specified
  in `docs/adr/020-multi-node-deployment-hardening.md`. Do not count source
  inspection as a live cluster authentication or failover test.

### 10.10 External penetration test (#1216)

**Pending external evidence.** No assessor engagement, budget approval, test
report, or independent retest is recorded by this checklist. If budgeted,
arrange the engagement during hardening and leave time to fix findings before
RC; automated regression tests do not fulfill this external assessment.

- **Before testing**: Record the authorized assessor, exact candidate commit
  and artifact digests, dates, staging hosts, permitted techniques, request
  rate ceilings, data handling, and emergency stop contact in a private
  engagement record. Use disposable tenants and synthetic data. Agree scope
  and permission before probing any external or production system.
- **Auth scope**: Password/magic-link/OIDC login, reset and MFA flows, session
  expiry/revocation, CSRF, rate limiting, scoped API keys and deployment/run
  credentials, and privilege escalation between unrelated projects.
- **Proxy scope**: Host and forwarded-header spoofing, domain/route ownership,
  private admin-listener exposure, websocket and preview access, path
  normalization, upstream targeting/SSRF, and certificate handling.
- **API scope**: Object-level authorization on reads and writes, backups and
  restores across projects, secret masking/one-time issuance, upload/archive
  traversal, query/metrics injection, resource limits, and audit coverage.
  Inventory intentionally public ingest/webhook routes separately from
  management routes; anonymous acceptance alone is not a finding on a route
  deliberately designed for public ingestion.
- **Completion evidence**: A private report tied to the tested candidate with
  scope, methods, reproducible findings, severity, and coverage limitations;
  fix commits and regression results for each finding; independent retest of
  fixes against the RC. Publish only a sanitized summary without credentials,
  customer identities, or unresolved exploit details.
- **Release decision**: Record unresolved findings and explicit maintainer
  disposition. Until an engagement and retest exist, report this issue as
  pending (or explicitly unbudgeted), never as passed. ADR-020 multi-node GA
  remains a separate gate.

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

## 12. Curated service templates (Beta)

Run this section for every release that changes the curated service catalog,
service-project creation or upgrades, managed-resource bindings, deployment
telemetry, or the generic Docker Compose path. Use a fresh staging database and
clean worker unless a scenario explicitly tests an upgrade.

Curated services are native Temps projects backed by reviewed, versioned
templates. They are not imported from Coolify, they do not depend on an
external catalog, and they are not editable Compose projects.

### 12.1 Discoverability and catalog ownership

- **Steps**: Open **New Project** and select **Services** as an account allowed
  to create projects. Query `GET /api/templates?kind=service`.
- **Pass**:
  - Services is visible with a **Beta** badge.
  - Only bundled, reviewed templates are returned (currently Keycloak and
    Browserless); there is no Coolify CDN request or arbitrary upstream entry.
  - Each card shows the service's real logo, description, version, runtime
    type, required managed resources, and support status.
  - Removing or renaming a bundled template is treated as a versioned product
    change and is covered by upgrade tests.

### 12.2 Native project identity and immutable template snapshot

- **Steps**: Create one project from every bundled service template and inspect
  its project, source, and deployment records.
- **Pass**:
  - The project is stored as `project_type=service` and retains the template
    slug, template version, logo, and immutable configuration snapshot.
  - Docker-image templates use `source_type=docker_image`; choosing a service
    never creates a hidden or editable Compose source.
  - Project cards and project settings identify the project as a service and
    display the template logo.
  - A deployment uses its recorded template snapshot; publishing a newer
    template cannot silently change an existing deployment.

### 12.3 Configuration, defaults, and secrets

- **Steps**: Review and edit image, command, CPU, memory request/limit, exposed
  port, health path, and every template variable before creation. Omit each
  required field in turn and generate credentials twice.
- **Pass**:
  - Safe runtime defaults are prefilled and expandable; editable values are
    validated before project creation.
  - Required inputs carry a visible `*` and block creation with the exact
    missing field. Optional integrations may remain empty.
  - Usernames use conventional defaults such as `admin`, `postgres`, or
    `default`; passwords and tokens are cryptographically random.
  - Public bootstrap values such as an admin username are normal environment
    variables. Passwords and tokens are encrypted secrets and can be revealed
    only through the authenticated, authorized reveal action.
  - Secrets never appear in list responses, logs, telemetry, source snapshots,
    or generated deployment files returned to the browser.
  - Literal connection URLs are not misclassified as credentials and are not
    replaced with random strings.

### 12.4 Managed-resource bindings

- **Steps**: Create Keycloak with a new managed PostgreSQL database, then repeat
  with an existing compatible database.
- **Pass**:
  - Required managed resources are shown before creation and are provisioned or
    selected explicitly by the user.
  - The template maps the managed resource to its application-specific
    variables (for Keycloak: the expected `KC_DB_*` settings); users do not
    copy credentials manually.
  - Bindings are stored by stable resource identity, scoped to the correct
    project/environment, and re-resolved during deployment without exposing
    plaintext credentials.
  - Resource creation emits one success notification, rolls back incomplete
    project creation safely, and does not delete a pre-existing resource.
  - A missing, deleted, incompatible, or unauthorized resource produces a
    typed, actionable error.

### 12.5 One-click deployment smoke tests

Run one cold install for every bundled template on each supported architecture.

- **Keycloak**: reaches its configured health endpoint, uses the managed
  PostgreSQL database, exposes the generated public URL, and accepts the
  generated bootstrap administrator credentials.
- **Browserless**: reaches its health endpoint, requires its generated token,
  exposes the correct external URL, and serves its API and documentation
  assets without duplicated path prefixes.
- **All templates**: deployment logs stream while the job runs, runtime
  container logs are available after success or failure, the project monitor
  reconciles immediately after deployment, and a restart preserves data and
  configuration.

### 12.6 Redeploy and template upgrade

- **Steps**: Change user-editable runtime settings, redeploy, publish a newer
  compatible template revision, inspect the proposed upgrade, and apply it.
- **Pass**:
  - Redeploy preserves user overrides, managed-resource bindings, secrets, and
    persistent data.
  - Build and deployment settings show the originating template and the same
    editable runtime fields offered during creation, including the image.
  - Upgrade shows image/configuration/binding changes before applying them.
    Newly required fields must be completed; removed fields are not injected.
  - Applying an upgrade creates a new immutable snapshot and deployment.
    Historical deployments remain attributable to their original version.
  - A failed upgrade leaves the previous deployment usable and produces
    actionable logs.

### 12.7 Telemetry and privacy

- **Setup**: Enable anonymous product telemetry on staging.
- **Steps**: Trigger a successful deployment, a classified failure, a
  cancellation, and a redeploy for each bundled template. Inspect outbound
  telemetry envelopes.
- **Pass**:
  - Attempt, success, failure, cancellation, and redeploy events include the
    server-attested template slug and version.
  - Failure events contain bounded stage/code/classifier fields, never raw
    errors or container logs.
  - Template attribution cannot be forged by setting a project variable or
    using a normal Docker image, Git, Drop, or Compose project.
  - No image credentials, managed-resource URLs, environment values, project
    names, user data, or secrets are transmitted.
  - Disabling telemetry sends nothing and does not affect deployment behavior.

### 12.8 Multi-container boundary

Native multi-container service templates are not part of the initial Beta.

- **Pass**:
  - A bundled template that declares an unsupported multi-container topology is
    rejected during catalog validation with an actionable authoring error.
  - The UI never represents a Compose stack as a native single-container
    service.
  - Adding multi-container native templates requires an explicit data model for
    per-container image, command, resources, ports, environment mappings,
    placement, dependencies, health, logs, and upgrade diffs, plus multi-node
    scheduling tests.

### 12.9 Generic Git and Drop Compose regression

Docker Compose remains supported as a generic deployment preset for Git
repositories and uploaded Drop archives. It is not a service-catalog storage
format.

- **Steps**: Deploy one Git repository and one Drop archive containing a
  Compose file. Exercise service inclusion, public routes, limited startup
  permissions, health checks, redeploy, failure retention, and deletion.
- **Pass**:
  - Compose is detected from the source revision and the normalized preview is
    available without exposing resolved secrets.
  - The immutable Git commit or uploaded archive is the deployment source;
    there is no standalone editable-Compose source type or Compose-source API.
  - Unsafe sockets, devices, privileged mode, host namespaces, and sensitive
    host paths remain rejected by the final executor policy.
  - Supported services receive correct private networking, routes, health
    checks, and project-scoped persistent volumes.
  - Failed containers are retained without public traffic for authenticated log
    inspection, with inline UI access and CLI retrieval, then removed on the
    next successful deployment or project/environment deletion.
  - Deployment-stage logs and runtime container logs stream successfully; a
    transport failure is distinguishable from a container that emitted no logs.

### 12.10 Authorization and API surface

- **Steps**: Exercise catalog read, project creation, secret reveal, managed
  binding, deploy, upgrade, log retrieval, and delete actions as an owner,
  collaborator, unrelated authenticated user, and anonymous user.
- **Pass**:
  - Every mutation and secret/log read enforces the expected project and
    deployment permissions and produces an audit event where required.
  - Generated OpenAPI clients expose the native template endpoints and generic
    project/deployment APIs.
  - Removed Coolify catalog/preflight/install and standalone Compose-source
    endpoints are absent from the OpenAPI document and generated clients.
  - Error responses use RFC 7807 with contextual identifiers and no secrets.

### 12.11 Beta release gate

The feature may ship as Beta only when:

- Every bundled template passes its cold-install smoke test on supported
  architectures, plus redeploy and upgrade tests where a newer version exists.
- The managed-resource, secret, authorization, telemetry, and generic Git/Drop
  Compose regressions above pass.
- Generated OpenAPI specifications and all checked-in clients are current.
- Database migrations are proven from the previous release and from a fresh
  database.
- No critical/high security findings, failing required checks, stale Coolify
  references, or unsupported catalog entries remain.

There is no arbitrary template-count target. The gate applies to every curated
template actually shipped.

### 12.12 Stable graduation gate

Remove the Beta label only after all of the following hold for two consecutive
releases:

- Every shipped template passes automated cold install, redeploy, restart,
  health, monitor, logs, authorization, and supported-architecture tests.
- Template upgrades, managed-resource replacement/failure, secret rotation,
  rollback, and deletion have automated end-to-end coverage.
- A seven-day staging soak shows no orphan containers, leaked credentials,
  stuck deployments, stale monitors, or unbounded telemetry/log growth.
- The template schema and upgrade contract are documented and versioned, and
  unsupported multi-container or multi-node behavior is stated clearly.
- Operator and troubleshooting documentation is published and the final
  security review is approved.
## How to use this document

1. Create a private release evidence ledger for the release in flight (see above).
2. **Run** every scenario above against staging. Tick the boxes as you go.
3. Skip with a written reason any scenario that does not apply (e.g., "no
   migrations this release"). Never skip silently.
4. **Add a new scenario** at the bottom of the relevant section in this file
   in the same PR that fixes any regression that bit you in production.
   The scenarios should grow with every "we should have caught this" lesson.

## SDK publication in the server release lifecycle

`scripts/sdk_release.py` reads `.github/releases/packages.json` (the SDK catalog)
and `.github/releases/prepared.json` (explicit upcoming server-tag selections).
The catalog covers Node/TypeScript libraries, the Python plugin and workflow
SDKs, Go workflows, the Rust protocol/plugin/workflow crates, and the TypeScript
CLI (`cli` / `@temps-sdk/cli`). The CLI uses the same prepared-version selection,
OIDC publisher, verification, retries, and package tags as the npm SDKs.

### Preparing SDK releases for a server tag

Nobody picks SDK versions by hand. `python3 scripts/sdk_release.py prepare --tag
<server-tag>` (logic in `scripts/sdk_prepare.py`) decides, for every catalog
package, whether it ships with that tag and at which version, then bumps the
manifests, the lockfiles (`sdks/node/bun.lock`, `sdks/python/uv.lock`,
`Cargo.lock`) and in-catalog dependency pins, and writes the selection to
`prepared.json`. It reads git and the public registries only; it never
publishes, pushes or tags. It is deterministic and idempotent: the same history
and registry contents produce the same files, and a second run changes nothing.

- **SDKs follow the server version.** Every catalog package releases with every
  beta and stable server tag, at the tag's version, whether or not it changed:
  `v0.1.0-beta.64` releases `0.1.0-beta.64` (Python `0.1.0b64`), and `v0.1.0`
  releases `0.1.0`. A user matches SDKs to a server by version alone. The
  commits since each package's last release are still listed in the prep PR. A
  server version already published, or below a version the registry or package
  tags already have, stops preparation. Registry and package-tag versions are
  compared by the registry's own ordering (SemVer for npm, crates.io and Go, so
  an npm `-canary.1` counts and `+build` metadata makes no new version; PEP 440
  for PyPI, so a `.devN` counts), and one that cannot be ordered stops
  preparation too. A server tag outside
  `X.Y.Z[-(alpha|beta|rc).N]` cannot version SDKs and is refused.
- **Own version lines.** A catalog entry's `versioning` overrides that:
  `independent` keeps the package on its own line, bumped from its commits by
  the rules below, and `until-server-passes` does the same until the server
  version is above every version the package has published or tagged, then
  follows the server. The CLI uses `until-server-passes`: it shipped `0.1.38`
  and `0.2.0-beta.1` before SDKs followed the server, both above
  `0.1.0-beta.N`, and npm never takes a version back. The rules below apply only
  to packages on their own line.
- **Baseline.** The highest package tag (`sdk/<key>/v*`, or
  `sdks/go/workflows/v*`). Packages released before package tags existed use the
  commit that set their manifest to the newest version the registry has. A
  package the registry has never seen is a first release of its manifest
  version.
- **Changes.** Non-merge commits since the baseline that touched the package
  path or a declared `sources` entry in `packages.json` (`@temps-sdk/api` is
  generated from `apps/temps-cli/openapi.json`; `build` runs its `generate`
  script before testing and packing, as its `prepublishOnly` would, so it ships
  the client for the tag's spec). A catalog package nested inside
  another's path (`sdks/python/workflows` in `sdks/python`) counts only for the
  inner one. A commit that touches no package selects nothing.
- **Bump.** From Conventional Commit subjects and bodies: `fix`/`perf` patch,
  `feat` minor, `!` or a `BREAKING CHANGE:` footer major; other types release
  nothing. Before 1.0.0 everything shifts down one level (breaking minor, feature
  patch). Prereleases increment like node-semver, so `1.3.0-beta.2` plus a
  feature stays core `1.3.0`. An unpublished manifest version already ahead of
  the computed one (a deliberate `1.0.0`) is kept.
- **Channel.** A stable server tag selects stable versions and also graduates a
  package whose last release is a prerelease. A prerelease server tag selects
  `-beta.N` (Python `bN`), with N one above every beta of that core on the
  registry or in package tags. Nightly tags are refused.
- **Cascade.** A selected package re-releases its in-catalog dependents
  (npm `workspace:` ranges, Cargo path dependencies, uv path sources, Go
  `replace` directives) with at least a patch, transitively, and rewrites their
  exact pins (the plugin SDK's `=` requirement on the protocol).
- **Stamped files.** A catalog entry's `stamped` list names committed files
  generated from the package that record `<name>@<version>`, such as the CLI
  command reference `skills/temps-cli/references/COMMANDS.md`. A bump rewrites
  that stamp; a stamp that no longer matches the manifest stops preparation.
- **Channel pins.** A catalog entry's `channel_pinned` globs name files that run
  the package by dist-tag, such as the skills' `bunx @temps-sdk/cli@latest`.
  When a bump changes the channel (a stable version to a beta, or back), every
  `<name>@latest` moves to `<name>@beta` or the reverse, so the documented
  commands exist in the CLI the skills run; finding no pin stops preparation.
- **Guards, failing closed.** A version already published or tagged, a registry
  stable release ahead of the computed version, a stable package depending on a
  prerelease sibling, a Go v2+ version, or a target tag that already exists.
  The result must round-trip through `plan()`, which keeps its ordering
  (analytics core before consumers, protocol before the plugin SDK).

`prepared.json` holds only the upcoming tag; the tag's own commit retains its
selection for release retries. It looks like:

```json
{
  "v1.2.0": {
    "workflows": "0.2.0",
    "python-workflows": "0.2.0",
    "go-workflows": "0.2.0",
    "temps-workflows-sdk": "0.2.0"
  }
}
```

**The SDK Release Prep workflow** (`.github/workflows/sdk-release-prep.yml`)
runs `prepare`, `validate` and `build --tag <server-tag>` as a dry check, then
opens or updates the signed-off PR `chore(release): prepare SDKs for
<server-tag>` from branch `sdk-release-prep/<server-tag>`, with a per-package
table of previous and next versions and the commits behind each bump. The job
that builds packages has a read-only token; the job that pushes and opens the PR
runs no package code and re-derives the change, refusing to continue if the
bytes differ from what was dry-built. Neither has registry credentials or
`id-token`. It only ever force-updates its own commits on its branch.

It fits how beta and stable tags are cut today. `scripts/release.sh` makes the
version-bump commit; it reaches main through a `release/v<version>` PR (the
branch is pushed by hand, and every recent release PR used that name), and the
tag goes on that PR's merge commit. Pushing a `release/v*` branch runs the
workflow for `v<version>`. Preparation reads main, so it fails when the release
branch changes SDK files main lacks (they would merge into the tag unversioned),
whether in a commit, a merge resolution, a package added only on the branch or
`.github/releases`: land those changes on main first. For any other path,
dispatch it:
`gh workflow run sdk-release-prep.yml -f tag=v<version>`. Merge the SDK PR into
main **before** the release PR so the tag contains it.

`release.sh` runs `prepare --check` before its bump commit. `--check` exits 1
when files would change and 2 when the check itself failed (a registry lookup,
an invalid manifest); a failed check stops the release. If the tag already has a
selection that is now stale, it stops: publishing it would ship newer code under
the selected versions. If nothing is selected yet, it asks before releasing the
server without those SDK changes. Nightlies never publish SDKs, so
`nightly-release.yml` does not run it.

Commits can still land on main between `release.sh` and the tag. When a tag has
a selection, `release.yml`'s validation job re-derives it on the tagged commit
(`prepare --tag <tag> --check --at-tag`, read-only) and fails before anything
publishes if the versions no longer match, because a published version cannot
be replaced. Delete the tag, re-run the prep workflow for it, merge its PR and
tag again. A re-run after part of the selection was published still verifies
every package, and so does a fresh run at an older tag: the check hides this
release's own package tags and registry versions, and those of later releases
(package tags not on the tagged commit's history), so it re-derives from the
state before the release.

`release.yml` plans in its existing validation job and tests/packages in its
existing web-assets job. A single isolated `publish-sdks` job waits for the test
gate and GitHub Release, uploads to npm/PyPI/crates.io, verifies registry hashes,
and creates immutable package tags. npm versions are **staged**, not published
(see [Approving staged npm versions](#approving-staged-npm-versions)), so the
tags wait for a maintainer's approval and a rerun. Go uses the required subdirectory tag
`sdks/go/workflows/v<version>`; other packages use `sdk/<catalog-key>/v<version>`.
The tags point to the tested release commit and are never moved. They are output,
not workflow triggers. This works for the existing tag-targeted dispatch path
used by automated releases, including tags created with `GITHUB_TOKEN`.

The existing **Workflows SDK** CI job now tests/packages all Node, Python and Go
SDKs without a new matrix; the existing release-contract job tests the planner
and retry rules. Rust packaging batches selected crates so unpublished sibling
dependencies can be verified together. No registry credentials are given to
these build/check jobs. The only extra release runner isolates publication
permissions (`id-token: write`, package-tag `contents: write`) from builds.

### Libraries-only releases

A libraries-only release publishes the SDKs and the CLI without building or
releasing the server: no web or server build, no daemon, sandbox,
preview-gateway, database or server images, no GitHub Release. It exists to ship
a CLI or SDK beta in the time the SDK tests and packaging take.

- **Tag.** `libraries/vX.Y.Z-beta.N`. GitHub tag filters' `*` never matches
  `/`, so none of the server patterns (`v*.*.*`, `v*.*.*-*`) and none of the
  server tag validators match it, and `cliff.toml`'s anchored `^v[0-9]` keeps it
  (and the `sdk/*` package tags) out of `CHANGELOG.md`.
- **Number.** It reserves a server beta number: `libraries/v0.1.0-beta.64`
  publishes every server-versioned package at `0.1.0-beta.64` (Python
  `0.1.0b64`), and the next server release is `v0.1.0-beta.65`. Packages on
  their own line keep it (the CLI's `until-server-passes` line, so a CLI fix
  ships as `0.2.0-beta.N+1`). `python3 scripts/sdk_release.py next-beta` prints
  the next free number across both kinds of tag. `prepare`, `release.sh` and
  `check-release-tag` refuse a server tag and a libraries tag of the same
  version, and, before a tag is created, a libraries tag at or below any
  release or a server tag at or below a libraries release. `release.yml`
  repeats the same-number check at the tag (`check-release-tag --at-tag`).
  Only betas can be reserved; the rules live in `release_tag_problems` and
  `LIBRARIES_TAG` in `scripts/sdk_release.py`.
- **Workflow.** It runs in `release.yml`, because each registry's trusted
  publisher names that file and its `sdk` environment; a second workflow would
  need a second trusted publisher on every package. The mode comes from the
  tag alone (`validate-release-ref.sh` sets `server=false`): every server job
  carries `if: needs.validate-release-ref.outputs.server == 'true'` or depends
  on one that does, the web steps of the web-assets job are skipped, and that
  job still tests and packages the selected SDKs. `publish-sdks` accepts the
  skipped `create-release` only in this mode. The `rust-tests.yml` gate and the
  at-tag re-derivation of the selection still run.
- **Cutting one** (a CLI beta, say):

  ```sh
  python3 scripts/sdk_release.py next-beta            # e.g. v0.1.0-beta.64
  gh workflow run sdk-release-prep.yml -f tag=libraries/v0.1.0-beta.64
  # review and merge the "chore(release): prepare SDKs for libraries/v0.1.0-beta.64" PR
  git switch main && git pull
  scripts/release.sh --libraries                     # checks, tags, offers to push
  ```

  Then approve the staged npm versions and rerun `publish-sdks`, as below.
  Nothing is committed: there is no `Cargo.toml` bump and no changelog
  section, and the commits appear in the next server release's notes.

### Approving staged npm versions

The npm trusted publishers allow only `npm stage publish`, so the workflow never
makes an npm version installable by itself: a maintainer approves each one with
2FA. A stolen workflow token can at most stage a version nobody approves. Staging
needs npm 11.16.0 or later; the job pins a Node whose bundled npm has it and
checks the version before any upload.

On a tag that selects npm packages, `publish-sdks` stages each one with its
channel's dist-tag (`beta` or `latest`), publishes PyPI and crates.io as before,
then fails at **Create immutable package tags** with the list of npm versions
still waiting. That failure is expected. Then:

1. Approve the staged versions in the order the job lists them (dependencies
   first, such as `analytics-core` before `react-analytics`), on npmjs.com or
   from a terminal logged in to an account with publish rights:

   ```sh
   npm stage list @temps-sdk/analytics-core   # find the stage id
   npm stage approve <stage-id>               # asks for 2FA
   ```

   `npm stage reject <stage-id>` discards a version that should not ship.
2. Rerun the job. It finds the approved versions public with the tested bytes,
   skips every upload and creates the package tags:

   ```sh
   RUN=$(gh run list --workflow release.yml --branch "$TAG" --limit 1 --json databaseId --jq '.[0].databaseId')
   gh run rerun "$RUN" --failed
   ```

Rerunning before approving is harmless but fails again: npm refuses to stage a
version that is already staged, and the job names it. One refusal does not stop
the others from being staged. Approve or reject staged versions before the next
release, since an unapproved version still holds its number.

### Dry runs and recovery

```sh
python3 scripts/sdk_release.py validate
python3 scripts/test_sdk_release.py
python3 scripts/sdk_release.py prepare --tag v1.2.0 --check   # print the selection, change nothing
python3 scripts/sdk_release.py prepare --tag v1.2.0           # write manifests, lockfiles, prepared.json
python3 scripts/sdk_release.py plan --tag v1.2.0
python3 scripts/sdk_release.py build --all --kind npm --output sdk-artifacts/npm
python3 scripts/sdk_release.py build --all --kind pypi --output sdk-artifacts/python
python3 scripts/sdk_release.py build --all --kind go --output sdk-artifacts/go
python3 scripts/sdk_release.py build --all --kind crate --output sdk-artifacts/rust
```

Run Rust packaging from a clean committed checkout. `build` runs tests and real
package verification; it never invokes a publication command. A manual
`release.yml` dispatch with `dry_run=true` on a branch validates every catalog
package; on a release tag it validates that tag's explicit selection. Neither
creates tags nor requests registry credentials. A non-dry dispatch still requires
an existing server version (or libraries-only) tag and the normal release test gate.

On an interrupted publication, rerun the **failed job**, retaining the successful
build's `sdk-packages` artifact (30-day retention). Do not rebuild to recover a
partial upload: artifacts carry the release SHA, plan and file hashes, and the
publisher accepts only identical existing registry bytes. A partial PyPI upload
sends only missing files. Auth failures, timeouts and registry errors never mean
"version absent"; transient metadata reads retry with a bounded delay. Registry
visibility is checked after upload (npm: after approval, when the job is rerun),
and a timeout fails the job for reconciliation on rerun. A conflicting/yanked version requires a new version and release commit.
An existing npm channel ahead of a new upload is rejected rather than downgraded.

Publication is serialized across server releases. GitHub concurrency can replace
a queued pending run when another arrives: finish/reconcile the current release
before starting another, and rerun a displaced release if it still needs SDKs.
If SDK publication fails after the server release, the server assets remain
published and the overall workflow is failed; the prepared-version table in the
release notes records intent, not proof that registries succeeded. Expired build
artifacts require a fresh validated run; changed bytes cannot overwrite an
already uploaded version.

### Operator checklist: one-time registry setup

The workflows cannot do these steps: they need registry accounts, 2FA and
repository admin rights. Do them before the first server tag whose prepared
selection publishes to that registry. A read-only check on 2026-11-19 found:

- **Never published** (no package on the registry): `@temps-sdk/sandbox`,
  `@temps-sdk/workflows`, `@temps-sdk/api`; PyPI `temps-plugin-sdk` and
  `temps-workflows`; crates `temps-plugin-protocol`, `temps-plugin-sdk` and
  `temps-workflows-sdk`.
- **Published, but none of their latest versions carries npm provenance**, so
  none was published by trusted publishing: `@temps-sdk/analytics-browser`,
  `analytics-core`, `blob`, `kv`, `node-sdk`, `plugin`, `react-analytics`,
  `svelte-analytics`, `vue-analytics` and `cli`. Publisher configuration is
  private, so check each package's settings.
- **No `sdk` environment** exists. The repository does not let GitHub Actions
  open pull requests.

Re-check registry state before acting:

```sh
curl -s https://registry.npmjs.org/@temps-sdk%2Fsandbox | head -c 200
curl -s -o /dev/null -w '%{http_code}\n' https://pypi.org/pypi/temps-workflows/json
curl -s -o /dev/null -w '%{http_code}\n' -A 'temps-sdk-release' https://crates.io/api/v1/crates/temps-plugin-protocol
```

1. **Let the prep workflow open its PR** (or open it by hand from the pushed
   branch each time):

   ```sh
   gh api --method PUT repos/gotempsh/temps/actions/permissions/workflow \
     -f default_workflow_permissions=read -F can_approve_pull_request_reviews=true
   ```

   CI runs for a PR the workflow token opens or updates wait for approval: a
   maintainer with write access selects **Approve workflows to run** in the
   PR's merge box before merging it.

2. **Create the environment.** The publish job runs in the `sdk` environment
   for both channels, so every registry trusts a single environment name.
   GitHub would otherwise create it on first use without protection. Allow
   only server version tags (`v*.*.*` also matches `v1.2.0-beta.3`):

   ```sh
   gh api --method PUT repos/gotempsh/temps/environments/sdk \
     -F 'deployment_branch_policy[protected_branches]=false' \
     -F 'deployment_branch_policy[custom_branch_policies]=true'
   gh api --method POST repos/gotempsh/temps/environments/sdk/deployment-branch-policies \
     -f name='v*.*.*' -f type=tag
   ```

   **Optional:** add required reviewers to `sdk` (Settings → Environments →
   `sdk`), so a person approves each publication. This applies to beta and
   stable alike. npm versions already wait for a maintainer's approval; PyPI
   and crates.io uploads do not.

3. **PyPI pending publishers** (allowed before the first upload; a pending
   publisher does not reserve the name, so do it shortly before the tag). In
   PyPI, open **Your account → Publishing** and add a GitHub publisher for each of
   `temps-plugin-sdk` and `temps-workflows`: owner `gotempsh`, repository
   `temps`, workflow `release.yml`, environment `sdk`. The first `release.yml`
   upload creates the project.

4. **npm and crates.io trusted publishers for existing packages.** For each
   package, open **Settings → Trusted publishing** on npmjs.com (or **Settings →
   Trusted Publishing** on crates.io), choose GitHub Actions and enter owner
   `gotempsh`, repository `temps`, workflow `release.yml` and environment
   `sdk`. On npm, leave **Allow npm publish** and **Allow npm dist-tag**
   unticked: `npm stage publish` is always allowed, and the workflow uses only
   that. From a terminal, the same entry is:

   ```sh
   npm trust github @temps-sdk/<package> --file release.yml --repo gotempsh/temps \
     --env sdk --allow-stage-publish --yes
   ```

   To replace an existing entry, such as one for an older environment, find
   its id and revoke it, naming the package (outside its directory npm would
   otherwise pick the current folder's `package.json`):

   ```sh
   npm trust list @temps-sdk/<package> --json
   npm trust revoke @temps-sdk/<package> --id <id>
   ```

   Entries added on npmjs.com sit alongside the CLI's, so check that only the
   `sdk` one is left. An npm
   configuration expires unless it publishes within 2 days. Add it no more than
   2 days before the first tag that publishes, and recreate it if it expired.

5. **First publishes of never-published npm packages and crates.** npm and
   crates.io allow trusted publishing only on a package that already exists, so
   each needs one token-based upload. Publish the release run's own tested
   artifacts, never a local build, so the registry bytes match what
   `publish-sdks` verifies. On the first tag that selects them, npm refuses to
   stage each package without a publisher (the others are still staged) and
   crates.io stops at the first crate without one. That failure is expected.
   Then:

   ```sh
   TAG=v0.1.0-beta.63        # the server tag being released
   RUN=$(gh run list --workflow release.yml --branch "$TAG" --limit 1 --json databaseId --jq '.[0].databaseId')
   git fetch --tags origin && git switch --detach "$TAG"
   gh run download "$RUN" -n sdk-packages -D sdk-artifacts
   python3 scripts/sdk_release.py plan --tag "$TAG"     # selected keys, versions and channels

   # npm, as an account with 2FA and publish rights on @temps-sdk.
   # Use --tag latest instead of --tag beta for a stable server tag.
   npm login
   for key in sandbox workflows api; do
     npm publish "sdk-artifacts/$key/package.tgz" --access public --tag beta --ignore-scripts
   done

   # crates.io, with a token scoped to publish-new for the three crates. Run it
   # on Linux with the pinned toolchain: it repackages, refuses bytes that differ
   # from the tested archive, publishes in dependency order and verifies them.
   CARGO_REGISTRY_TOKEN=<token> python3 scripts/sdk_release.py publish --tag "$TAG" --kind crate --output sdk-artifacts
   ```

   Only packages in that tag's selection have artifacts, so publish each one with
   the first tag that selects it. Then configure their trusted publishers (step 4),
   approve the npm versions the run staged ([Approving staged npm
   versions](#approving-staged-npm-versions)), and resume the release, which
   skips the byte-identical uploads and creates the package tags:

   ```sh
   gh run rerun "$RUN" --failed
   ```

   These token-published versions have no npm provenance; every later version
   gets it from trusted publishing. Revoke the crates.io token afterwards.

6. **Go.** No registry account is needed. The workflow token creates
   `sdks/go/workflows/v<version>`; check repository rulesets do not block it.

Rust package verification also reported existing locked yanked dependencies
(`chacha20 0.10.0`, `spin 0.9.8`); remediating them is separate from this
lifecycle.

Integrate the independent workflow security fixes before preparing the next
workflow SDK versions in all four languages. This lifecycle change does not
contain one-shot invocation admission. Re-run SDK conformance and packaging on
the combined commit, bump each affected SDK version, and select those versions
in the next appropriate server release; publishing this branch alone would not
roll out the security fix.
