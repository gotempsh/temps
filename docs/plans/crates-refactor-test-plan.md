# #1682 test plan: manual checks and missing automated tests

Companion to [`crates-refactor-retest-map.md`](./crates-refactor-retest-map.md),
which explains *why* each area is risky. This document is the checklist:
what to run by hand before merging, and which automated tests are still
missing, rated by need.

## Auto-merge: not yet

Branch protection on `main` requires **0 reviews** and only these 11 checks:
sign-off, commit validation, Formatting, Cargo Check, Web TypeScript Check,
OpenAPI Spec Format, Unit Tests (unit-a, unit-b, unit-integration), Skill
security gate and CodeRabbit.

The suites that exercise a running Temps are **not** required:
- E2E;
- scenario E2E;
- upgrade tests;
- single-node kernel integration;
- Clippy.

They all run on this PR, but auto-merge would fire as soon as the 11
required checks pass. It would not wait for them, or for any of the manual
checks below.

Recommendation:
1. Leave auto-merge off.
2. Run the manual checks below and add the missing tests rated 4–5.
3. Wait for the whole check list to be green, including E2E and upgrade.
4. Merge by hand with a **merge commit** (`gh pr merge 1682 --merge`), so a single `git revert -m 1` can undo it.

---

## Results of the local runs (2026-10-31 and 2026-11-01)

Run on a local instance. Upgrade path: main's build (`36b3f0b65`) created the
database with the legacy variables set, then the branch build started on the
same database with the variables still set.

| Check | Result |
|---|---|
| A1 migrations on main's database | **Pass.** 399 → 401 migrations; `response_body` dropped, both old deliveries kept |
| A2 legacy values migrated | **Pass.** Let's Encrypt `staging`, screenshots `noop`, backup concurrency 2, OTel rate limit 500, quota 7 |
| A3 one audit record per value | **Failed, fixed.** `TEMPS_SCREENSHOT_PROVIDER` had no audit record. Fixed in `fe7b5d216`, with a test |
| A4 admin change survives restart | **Failed, fixed.** Setting Let's Encrypt back to `production` (the default) was reverted to `staging` on the next restart, with a new audit record each time. Fixed in `fe7b5d216` with a migration ledger, and re-checked live: `production` and `local` survive restarts, no new records |
| A5 webhook deliveries | **Pass.** List and fetch work, no response body anywhere, redelivery works |
| A6 fresh-install defaults | Covered by `defaults_match_the_former_environment_defaults`; not run live |
| B1 IP block | **Pass.** 403 after blocking 127.0.0.1, 200 after unblocking. The blocklist refreshes every 30 s, the same as before this PR, so allow up to 30 s, not 10 |
| C2 two quick deploys | **Pass.** The environment ends on the newer deployment |
| C3 three environments at once | **Pass when started 1 s apart.** Three overlapping image deploys to three environments of one project all completed, each environment on its own deployment. **Started at the same instant, two of three failed**, from an existing bug on `main` (see below) |
| C4 rollback | **Pass.** A new deployment on the old image, served by the proxy |
| C5 stop / start / restart | **Pass.** Each works; the app returns 503 while stopped |
| C8 block I/O stats | **Pass.** `block_write_bytes` matches 64 MiB written with fsync |
| C10 route change | **Pass.** The rollback's new container is served without a restart |
| D1 manual backup | **Pass.** Managed Postgres 18 and a RustFS-backed S3 source. The preflight warned about the WAL-archiving restart, and the backup completed with a 5.8 MB WAL-G base in the bucket. All 1000 rows survived the container swap, with matching checksums |
| D2 scheduled backup | **Pass.** A schedule due at 07:31 ran at the 08:00 scheduler tick (see note), producing a control-plane backup (1.3 MB) and the Postgres service backup (5.8 MB), both completed. `last_run` was recorded and `next_run` moved to the next day |
| D3 lifecycle rules | **Pass (rules).** Creating a 1-day schedule put `temps-retention-1d` on the bucket (a 1-day expiry filtered on the `temps-retention-days` tag); changing it to 7 days replaced it with `temps-retention-7d`, with no reconcile failure recorded. The expiry itself takes a day and was not observed |
| D4 restore in place | **Pass.** Deleted 990 rows and added one after the backup. The plan listed 7 steps, and the restore completed in about 10 s: back to the 1000 backed-up rows with the same checksum, the added row gone, and the server promoted out of recovery |
| D5 major upgrade 17 → 18 | **Pass, end to end.** Pre-backup (through the reworked `BackupService` job queue), snapshot, dump, new container, restore and analyze, in about 20 s. Server version 170011 → 180002, 500 rows with the same checksum, both indexes present |
| G1 default log filter (static) | **Pass.** Every renamed crate that was in main's default filter is in the branch's under its new name (`temps_node_agent`, `temps_ai_agents`, `temps_deployment_logs`); no code moved into an unlisted crate |

Not run here: B2, B3 and B5 (need a public domain, a Cloudflare token, or
stopping a shared database), B4 (the host's preview gateway belongs to
another instance), C6 and C7 (need Linux microVMs), E, F, and G2.

Found along the way. These are existing behaviour on `main`, not changed by
this PR; follow-ups rather than blockers:
- **Concurrent deploys to different environments of one project can kill
  each other's container.** The image-deploy handler numbers a deployment
  as `count(project deployments) + 1` with no lock, so simultaneous
  requests get the same slug. The slug is the container name, and the
  Docker deployer removes any existing container with that name before
  creating its own: one environment's fresh container is deleted, and
  another fails with a 409 name conflict. The handler and the
  remove-by-name step are the same code on `main`. Fix in its own PR: a
  unique slug (a sequence or a unique index with retry), and never remove
  a container labelled with another deployment.
- The backup scheduler only wakes at the top of each hour, so a schedule
  whose cron says `0 31 7 * * *` runs at 08:00. The cron field accepts
  minutes, which suggests otherwise.
- `BackupScheduleResponse` returns `created_at`, `next_run` and `last_run` as
  epoch milliseconds instead of ISO 8601.
- The default log filter names `temps_sentry_ingester`, which is no crate,
  and omits about 37 crates (`temps_flags`, `temps_edge_rules`,
  `temps_mcp_server`, …), whose info logs are hidden unless `RUST_LOG` is
  set. A test (T7) would catch this.

---

## Part 1: manual checks

Run these on a Temps built from the branch (`cargo build --release --bin temps`)
with the web UI, against a database restored from a main-era instance where
the step says so. Tick each box; anything that fails blocks the merge.

### A. Upgrade from main (do this first, on a copy of a real-shaped database)

Start the **current main** build with these legacy variables set:
`LETSENCRYPT_MODE=staging`, `TEMPS_SCREENSHOT_PROVIDER=noop`,
`TEMPS_BACKUP_EXECUTOR_MAX_CONCURRENT=2` and one OTel ingest variable. Create
a webhook and let it record a few deliveries. Stop it, start the branch
build against the same database with the same variables, and check:

- [ ] A1. All migrations apply, including `m20261105_000002_drop_webhook_deliveries_response_body`; startup completes.
- [ ] A2. Settings shows the Let's Encrypt environment as `staging`, the screenshot provider as `noop`, backup concurrency 2 and the OTel value.
- [ ] A3. The audit log has one `SETTING_MIGRATED_FROM_ENV` entry per migrated value, with names only, never values.
- [ ] A4. Change one migrated value in the UI, restart with the variables still set → your change is kept, and no new migration audit entry appears.
- [ ] A5. Webhook deliveries list and redelivery work; no response body is shown anywhere.
- [ ] A6. Fresh install with no variables → Let's Encrypt `production`, screenshot provider `local`, backup concurrency 4.

### B. Proxy and certificates

- [ ] B1. Block an IP in the console → requests from it are refused within ~30 s (the blocklist refresh interval). Unblock → accepted again.
- [ ] B2. Add a custom domain that issues over HTTP-01 → certificate issued.
- [ ] B3. Issue a certificate over Cloudflare DNS-01 → issued; a wrong token shows a clear error rather than crashing.
- [ ] B4. Open a preview URL that needs the gateway token → it works. Rotate the token in settings → the new one works without a restart, and the old one is refused.
- [ ] B5. Stop Postgres for ~30 s while sending traffic → existing routes keep serving, the logs show a backed-off refresh rather than a query per connection, and it recovers when Postgres returns.

### C. Deployment lifecycle

- [ ] C1. Create a project from a Git repo and deploy → success, with download progress in the deployment log.
- [ ] C2. Push twice quickly to the same environment → both deploy and complete in order; the environment ends on the newer commit.
- [ ] C3. Deploy three environments at once → all complete.
- [ ] C4. Redeploy, roll back and promote → each works.
- [ ] C5. Stop, start and restart a Docker container from the deployment page → each works. **This path was re-applied by hand into the split files.**
- [ ] C6. On a host with the microVM runtime: stop, start and restart a microVM replica → refused with a message naming the replica and the operation.
- [ ] C7. Tear down an environment that has a workload-plane replica → the replica is retired and teardown completes.
- [ ] C8. Container stats panel → block read/write bytes are shown and grow under disk activity.
- [ ] C9. Run a compose service → starts, logs stream, stops.
- [ ] C10. Change a route or custom domain → the proxy serves the change without a restart.

### D. Backups and services

- [ ] D1. Create a managed Postgres → manual backup succeeds.
- [ ] D2. Schedule a backup a few minutes out → it runs.
- [ ] D3. S3 source with lifecycle rules → backup uploads and retention applies.
- [ ] D4. Restore a backup → data comes back.
- [ ] D5. Start the Postgres major-upgrade flow (or its dry run) → the job enqueues.

### E. Remote nodes and edge (skip a section if you have no such node)

- [ ] E1. Add a remote node over SSH (exercises russh 0.65) → it connects and becomes healthy.
- [ ] E2. A workload on the worker resolves another workload by name (node DNS).
- [ ] E3. Edge node: route a domain through it, then check SNI for two domains plus a wildcard.
- [ ] E4. Edge node: fill the cache past its limit → disk usage stops growing, and evicted files are gone from the cache directory. Existing edge nodes will free space on their first eviction after upgrading.

### F. Plugins, sandboxes and AI

- [ ] F1. Install and load an external plugin → its pages and API work, and its events arrive.
- [ ] F2. Create an AI sandbox and run a command in it → output comes back.
- [ ] F3. Run an AI agent CLI session end to end.
- [ ] F4. Import a project.

### G. Operator-visible changes

- [ ] G1. `RUST_LOG=temps_node_agent=debug` shows node-agent debug lines; the old `temps_agent=debug` shows nothing (expected; goes in the release notes).
- [ ] G2. Release-build dry run: push a `test-v*` tag (runs `test-release.yml`) or dispatch the release workflow with dry run, and check that the Linux and macOS binaries for both architectures build.

---

## Part 2: missing automated tests

Need: **5** = must add before merging · **4** = should add before merging
(covers a hand-ported or bug-fix path with no regression test) · **3** =
add soon after · **2** = nice to have · **1** = already covered, nothing to add.

| # | Area | What is missing | Need |
|---|---|---|---|
| T1 | Deployment container ops (hand-ported) | `refuse_micro_vm_lifecycle` is tested on its own (`plane_deployer.rs`), but nothing checks that `DeploymentService::{stop,start,restart}_container` actually call it. Those three call sites were re-applied by hand into `services/services/containers.rs`. Add a test that each refuses a plane replica and leaves Docker untouched. | **4** |
| T2 | Edge cache eviction | The fix is that eviction now deletes `<root>/cache/<key>` instead of `<root>/<key>`. `test_eviction` only checks the byte counter, and `test_invalidate_deletes_the_file_from_disk` covers invalidation, not eviction. Add a test that an evicted entry's file is gone from disk. | **4** |
| T3 | External plugin channel | No test for the full-channel path: an event `try_send` on a full 256-slot channel must return the error naming the plugin; a closed channel likewise. | 3 |
| T4 | Plugin SDK events | No test that events beyond 256 are dropped with a warning while requests still go through. | 3 |
| T5 | Settings shim end to end | Each variable's migration plan is unit-tested (backup, OTel, Let's Encrypt, screenshots, core), but nothing boots the plugins with a variable set and checks the stored row and the audit record together. Manual check A covers this before the merge; an integration test against `TestDatabase` would keep it covered. | 3 |
| T6 | Git archive progress | No test that progress updates beyond 32 are dropped without blocking the download. | 2 |
| T7 | Default log filter | No test that every crate named in the default `EnvFilter` in `temps-cli/src/lib.rs` exists in the workspace, so a future rename cannot silence logs. | 2 |
| T8 | unwrap/expect → errors (248 files) | No per-site tests. The fixed real panics each have one (digest send time, anomaly `sustained_minutes=0`, `RetryConfig` `max_attempts=0`); the rest are covered by E2E and manual checks B–F. | 2 |
| T9 | Proxy: connection filter, ACME cache, preview token | Covered: single-flight, backoff, timeout, keep-last-blocklist, late-stored challenge, rate-limited misses, settings-over-env. | 1 |
| T10 | Locks and registries | Covered: environment-lock eviction and poisoning, zone-store snapshot reads, scraper poisoned map, registry poisoning, retention-cache poisoning. | 1 |
| T11 | Backup job queue at construction | Covered: `require_service` fails before phase two (`missing_requirement_fails_before_phase_two`). | 1 |
| T12 | Webhook column drop | Covered: the migration test inserts a populated row before migrating up. | 1 |
| T13 | Route-listener payloads after DTO trimming | Covered: six payload-parsing tests, including minimal insert/delete payloads. | 1 |
| T14 | Main's ported changes | Covered: `teardown_retires_a_plane_replica_instead_of_stopping_it_in_docker`, `cleanup_never_tombstones_a_plane_replica_it_could_not_retire`, `block_io_*`, carried into the split test files and passing. | 1 |

Nothing scores 5: every behaviour change has at least partial coverage.
T1 and T2 are the two worth adding before merging: each guards a path that
is either hand-ported or the actual bug fix, with no test of its own.
