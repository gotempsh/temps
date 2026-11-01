# #1682 retest map: what changed behaviour, and how to retest it

#1682 changes 937 files (+138,878 / −129,484). Classified by commit, the
non-generated changed lines split into four risk tiers:

| Tier | What | Lines | Share | Retest |
|---|---|---|---|---|
| 1 | Runtime behaviour changes (proxy, edge, settings, migration, locks, channels, wiring) | ~6,300 | 2.4% | **Yes, by hand on a running instance** |
| 2 | Wide mechanical edits that can still change behaviour (unwrap/expect → errors, dead code and DTO fields removed, deps/features, crate renames, toolchain) | ~9,700 | 3.6% | Targeted smoke tests + a release-build dry run |
| 3 | Pure moves and splits (backup, projects, deployments, deployer, providers, git) | ~244,000 | 91.3% | No dedicated retest: line-for-line verified, compiled, and covered by the existing tests |
| 4 | Docs, ADRs, CI scripts, rule drift, lint attributes, test-only allows | ~7,400 | 2.8% | No |

The main-branch merges add a fifth, small but important group: main's
changes written against the old single files were **ported by hand** into
the split modules (section 1.8).

---

## Tier 1: retest each of these by hand

### 1.1 Proxy hot path (`6f8ca86fe`, `crates/temps-proxy`)

Changed: the TCP connection filter (IP blocklist) now refreshes
single-flight into an `ArcSwap`, retries after 5 s and times out after 10 s.
ACME HTTP-01 answers come from a bounded snapshot with a single-flight
reload. The preview gateway token is read from cached settings, not from
the environment on every request.

Retest:
- Block an IP in the console → a new connection from it is refused within seconds; unblock → accepted again.
- Add a custom domain that issues via HTTP-01 → the certificate is issued (the challenge is served for a challenge stored *after* the proxy started).
- Open a preview URL that needs the gateway token → it authenticates; rotate the token → the new one works without a restart.
- Stop Postgres briefly → proxy keeps serving with the last blocklist and recovers when the database returns (no per-connection query storm).

### 1.2 Edge node (`f66d71da7`, `crates/temps-edge`)

Changed: route table and cert store are `ArcSwap` snapshots, the cache
index is 32 lock-free shards with sampled access times, byte accounting
is race-safe, and **eviction now deletes the right path** (before, it
deleted `<root>/<key>` while files live at `<root>/cache/<key>`, so
evicted files were never freed).

Retest (on an edge node):
- Route a domain through the edge → served; change the route → picked up by the sync loop.
- TLS SNI for two domains, plus a wildcard → correct certs.
- Fill the cache past its limit → disk usage stops growing and evicted files are gone from `cache/`. **This is new behaviour: expect existing edge nodes to free disk on first eviction.**

### 1.3 Settings that moved from environment variables (`795f065a5`)

Changed: OTel ingest limits, quota, anomaly and health toggles and the S3
archive (keys now encrypted at rest and masked); `backup_executor.max_concurrent`;
`letsencrypt.environment` (was `LETSENCRYPT_MODE`); `screenshots.provider`
(was `TEMPS_SCREENSHOT_PROVIDER`). A one-release shim reads a legacy
variable once at boot and persists it only while the stored value is
still the default, with a `SETTING_MIGRATED_FROM_ENV` audit record.

Retest:
- **Upgrade path**: an instance on main's latest build with `LETSENCRYPT_MODE=staging` and `TEMPS_SCREENSHOT_PROVIDER` set → upgrade → the settings page shows the migrated values, one audit record each, and a restart does not re-migrate or overwrite an admin change.
- Fresh install with no variables → defaults (production Let's Encrypt, `local` screenshot provider, 4 concurrent backups).
- OTel S3 archive: save keys → response masks them; archive still uploads.
- Change `backup_executor.max_concurrent` → saved and audit-logged; it applies at the next restart (by design), so restart and confirm the new limit.

### 1.4 Migration that drops a column (`db4cb0144`)

`m20261105_000002_drop_webhook_deliveries_response_body` drops
`webhook_deliveries.response_body`. Old values are deleted, not migrated:
the column held receiver response bodies that could leak internal
addresses. `down` re-adds it empty.

Retest:
- Upgrade a database at main's latest migration that has webhook deliveries → migration succeeds; the deliveries list and redelivery still work.
- Migration order: it runs after main's `m20261105_000001_project_data_purges`.
- Release note: irreversible data removal.

### 1.5 Locks and registries (`4f40148d0`, `6d9a3e8b9`)

Changed:
- `mark_deployment_complete`'s per-environment lock map now holds `Weak` references and prunes itself.
- The node DNS zone store is `ArcSwap`.
- The Docker cleanup retention cache and the metrics scraper recover poisoned locks.
- `BackupService` receives its job queue at construction (a missing queue fails at startup instead of panicking on the first backup).
- `ServiceRegistry` and `PluginStateRegistry` recover poisoned locks.

Retest:
- Deploy the same environment twice quickly, and several environments in parallel → each completes once, in order per environment.
- Node DNS: workloads resolve each other by name on a worker node.
- Metrics: container metrics keep updating over a few scrape cycles.
- Backups: manual backup, scheduled backup, and the Postgres upgrade flow each enqueue and run.

### 1.6 Bounded channels (`bbcb332d0`)

Changed:
- External-plugin host channel: 256 messages. Responses wait for space; events use `try_send` and fail with an error when the channel is full.
- Plugin SDK: 64 requests (they wait for space) and 256 events (dropped with a warning when full).
- Git archive download progress: 32 updates, best-effort.

Retest: load an external plugin and exercise its pages and API; deploy
from a large repository and watch download progress in the deployment log.

### 1.7 Main merges ported by hand into split modules

Main changed files that this branch split. Those hunks were re-applied by
hand, so they are the least mechanically verified code in the PR:

- `temps-deployments/src/services/services/containers.rs`: stop/start/restart refuse microVM replicas (`refuse_micro_vm_lifecycle`) and container operations get the workload plane.
- `.../services/lifecycle.rs`: environment teardown retires plane replicas through the plane (`stop_replica_for_teardown`), and a retire failure logs and continues.
- `.../services/error.rs`: `DeploymentError::ReplicaRetire`.
- `temps-deployer/src/docker/deployer.rs`: block I/O bytes in container stats.
- Workspace `russh` 0.65 (security update from main), now in the workspace table.

Retest:
- Stop, start and restart a Docker container deployment → works. Same on a microVM replica → refused with a clear error.
- Tear down an environment that has a plane replica → replica retired, teardown completes.
- Container stats → block read/write bytes shown.
- Add or connect a remote node over SSH (exercises russh 0.65).

Main's own tests for these (`teardown_retires_a_plane_replica_instead_of_stopping_it_in_docker`, `block_io_*`) were carried into the split test files and pass.

---

## Tier 2: targeted smoke tests

### 2.1 unwrap/expect/`.context()` replaced across 248 files (`bd12c160f`)

Panics became typed errors, or the code was restructured so the panic
cannot happen. Most edits are in these crates (lines changed): temps-git
797, temps-ai-agents 325, temps-providers 239, temps-cli 142,
temps-deployments 131, temps-proxy 126, temps-config 118, temps-otel 105,
temps-backup 104, temps-core 103.

A failure that used to crash a task now comes back as an error, so check
that the error reaches the user:
- Git: connect GitHub and GitLab, list repos and branches, push → webhook → deploy.
- TLS: issue a certificate through Cloudflare DNS-01.
- Backups: S3 source with lifecycle rules → backup and retention.
- Sandboxes: create and run an AI sandbox (sandbox routing, executor spawn).
- Fixed real panics (each has a test): the digest email with an out-of-range send time, the anomaly detector with `sustained_minutes=0`, and `RetryConfig` with `max_attempts=0`.

### 2.2 Dead code and DTO fields removed (`d535bf86f`, `dac74d324`, `51c18aa09`)

Unused serde fields were removed from git, DNS, CLI, deployer,
git-credential and route-listener DTOs. None of these structs rejects
unknown fields, so parsing only gets more lenient. The route-listener
payload still tells its variants apart by `environment_id`.

Retest:
- Change a route or domain → the proxy picks it up through NOTIFY.
- An AI agent CLI session works end to end (839 lines of unreachable helpers were removed from `temps-ai-agent-cli`).
- Import a project (orchestrator mocks were removed).

### 2.3 Dependencies, features and the toolchain

- Unused dependencies and never-enabled features were removed, and member crates now inherit dependency versions from the workspace. `Cargo.lock` was unchanged by the inheritance step.
- The toolchain is pinned to Rust 1.99.1, with only the channel in `rust-toolchain.toml`. Release jobs install their own targets and components.

Retest: a release workflow dry run on Linux for both architectures, the
sandbox and daemon image builds, and the full release binary built with
the web UI.

### 2.4 Crate renames (`9a305baf6`, `c1df7d723`, `01e6cff1f`, `7abaa8c52`, `c0d57e46c`)

| Old | New |
|---|---|
| `temps-agent` | `temps-node-agent` |
| `temps-agents` | `temps-ai-agents` |
| `temps-telemetry` | `temps-product-telemetry` |
| `temps-logs` | `temps-deployment-logs` |
| `temps-dns-resolver` | `temps-node-dns` |

These are library crates inside the `temps` binary, so no binary, systemd
unit or install path changes. Operators see two things:
- **`RUST_LOG` targets change** (`temps_agent=debug` becomes `temps_node_agent=debug`), and old filters match nothing. The built-in default filter is already updated.
- Log lines carry the new module paths, which matters for anyone filtering logs by target.

Retest: `RUST_LOG=temps_node_agent=debug temps serve` shows node-agent debug lines.

---

## Tier 3: why the moves don't need a dedicated retest

The `refactor(<crate>): move/split …` commits:
- `temps-providers`: `services.rs` and `externalsvc/postgres.rs`.
- `temps-deployer`: `docker` and `compose`.
- `temps-projects`, `temps-backup` and `temps-deployments`: the service files.
- `temps-git`: `git_provider_manager`.

Each was checked by:
- a line-multiset comparison of the old file against the new modules: every line accounted for, the only differences being imports, `pub(super)` visibility and `super::` paths;
- compiling with the workspace lints and the existing tests, which moved with the code;
- an independent PR review per lane (#1683–#1698).

A one-path smoke test per area is enough:
- create a project;
- deploy, redeploy, roll back and promote;
- run a compose service;
- create and back up a managed Postgres;
- connect a git provider.

---

## Suggested retest order

1. Upgrade a copy of a real-shaped database from main's latest build: 1.3, 1.4.
2. Proxy and certificates: 1.1, plus Cloudflare DNS-01 from 2.1.
3. Deploy lifecycle, including concurrency and microVM refusal: 1.5, 1.7, Tier 3 smoke.
4. Backups: 1.5, the S3 lifecycle rules from 2.1.
5. Edge node, if any: 1.2.
6. Plugins and sandboxes: 1.6, 2.1, 2.2.
7. Release workflow dry run: 2.3.
