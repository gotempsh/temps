# Crates refactor plan: stability, less maintenance, less legacy, AI-legible code

**Status:** Draft for review
**Date:** 2026-10-31
**Scope:** the Rust workspace (`crates/`), plus the repository processes, CI and agent docs around it. The web console is covered only where it affects the crates (generated clients, shared specs).

---

## 1. Summary

The workspace has grown to **96 crates and about 1.6M lines of Rust** in roughly a year. That is 2,129 commits since May, **1,030 of them `fix`**. The codebase follows most of its own rules well. There are only 245 production `unwrap`/`expect` calls, every `From<Error> for Problem` impl is exhaustive, and there are no unbounded channels on the proxy path. The problems are structural, not stylistic:

1. **Bugs cluster in a few huge files.** Of the 15 files fixed most often, 14 are over 7,000 lines. `temps-providers/src/services.rs` (26,456 lines, with a single `impl` block of 14,200 lines) received 53 fix commits in six months. 44 files are over 5,000 lines, and 163 functions are over 300 lines. The largest are `request_filter` at 2,236 lines, `try_tool_loop_in_workspace` at 2,170 and `start_console_api` at 2,057.
2. **Every change rebuilds and touches too much.** `temps-core` has 76 transitive dependents, `temps-entities` 69 and `temps-database` 63. The database crate drags `temps-migrations`, the most-changed crate, into everything. 222 commits touched five or more crates at once. `temps-cli` depends on 64 internal crates.
3. **The rules in `CLAUDE.md` are not machine-enforced.** The workspace has no `[workspace.lints]`, `clippy.toml`, `deny.toml`, `rust-toolchain.toml`, cargo-machete or coverage. So rule drift only gets caught in review, which in practice means it often does not get caught.
4. **The same job is done in several places.** There are three container stacks (projects, sandboxes and databases; see ADR-052 and ADR-055), at least six Docker exec helpers, six S3 client builders, and 20 copies of `constant_time_eq`. Fixes have to be remembered several times.
5. **Agents work with a stale or missing map of the code.** `CLAUDE.md` loads about 15k tokens per session, says the workspace has "51 crates", and spends 17% of its size on an env-var table. Only 8 of 96 crates have a README and 14 have a Cargo `description`. ADR numbers collide across 23 files.

The plan has three phases. Section 5 lists the quick wins in detail.

1. **Weeks 1–2:** fix the confirmed hot-path stability bugs, add the guardrails, and delete the dead code. This is cheap, low-risk work that stops the drift.
2. **Weeks 2–8:** mechanically split the god files and cut the bad dependency edges. This gives the biggest drop in fix rate and build time.
3. **Quarter-scale:** the structural redesigns. These are the workload plane (ADR-055), splitting `temps-providers` and `temps-deployments`, and a migrations baseline at 1.0.

---

## 2. Current state in numbers

| Metric | Value |
|---|---|
| Crates / Rust LOC | 96 (+1 stub patch) / ~1.61M (≈545k of it inline tests) |
| Commits since 2026-05-01 / `fix` commits | 2,129 / 1,030 |
| Files >3k / >5k / >10k lines | 99 / 44 / 12 |
| Files whose *production* part is >3k lines | 50 |
| Functions >300 / >500 lines | 163 / 37 |
| Production `unwrap`/`expect` | 245 (tests: ~26k, fine) |
| `anyhow` lines in library crates / `.context()` | ~1,150 / 47 |
| Handler files querying the DB directly | 13 files, 112 query lines |
| Lazy error variants (`X(String)`, `Internal(String)`, `#[error("{0}")]`) | 446 of 2,865 (16%) |
| Runtime env-var reads outside bootstrap | 97 |
| `#[allow(dead_code)]`/`unused` attributes | 147 |
| Dependency lines still pinning their own version (not `workspace = true`) | 436 in 66 crates |
| Packages at more than one version in `Cargo.lock` | 145 |
| Migrations | 347 files (~43k lines), 81 in Sep and 51 in Oct |
| `#[ignore]` tests, against the CLAUDE.md rule | 78 |

**Where the fixes land.** The table counts fix commits since May. "Fix density" is fix commits per 1,000 lines.

| Crate | Fix commits | kLOC | Fix density | Tests / kLOC |
|---|---|---|---|---|
| temps-deployments | 147 | 152 | 0.97 | – |
| temps-providers | 119 | 133 | 0.89 | 10.3 |
| temps-cli | 116 | 53 | **2.17** | – |
| temps-core | 77 | 36 | **2.11** | – |
| temps-proxy | 76 | 59 | 1.28 | – |
| temps-backup | 74 | 52 | 1.42 | 8.1 |
| temps-migrations | 68 | 50 | 1.35 | **2.3** |
| temps-entities | 52 | 23 | **2.28** | 6.3 |
| temps-config | 40 | 17 | **2.41** | – |
| temps-cloud | 31 | 13 | **2.33** | – |
| temps-query-{mongodb,redis,postgres} | 36 | 6.5 | **4.8–7.4** | – |

High fix density in `temps-cli`, `temps-core`, `temps-entities` and `temps-config` comes mostly from **hand-maintained registration lists** that every feature edits. These are `serve/console.rs` (121 commits), `core/lib.rs`, `entities/lib.rs` and `migration/mod.rs` (182 commits). They are where unrelated changes collide.

---

## 3. Root causes

These are the problems the plan has to remove. The other findings are symptoms of them.

| # | Root cause | Symptoms |
|---|---|---|
| R1 | **Accretion into one file or one `impl`.** New features are appended to the nearest service instead of getting a module. | God files, functions over 1,000 lines, fix hotspots, files AI tools cannot read in one pass |
| R2 | **Wrong dependency direction.** Foundation crates carry domain code: handlers in `temps-config`, the migrator in `temps-database`, `app_settings`/templates/workflow engine in `temps-core`. | Wide rebuilds, shotgun commits, critical build path 18 crates deep |
| R3 | **Rules exist only as prose.** | `unwrap`, `anyhow` in traits, env-var config, handler DB access and `#[ignore]` all creep back in |
| R4 | **Parallel stacks for the same concept.** | Three container stacks, duplicated helpers, drift between control-plane and worker paths (ADR-055 §Context) |
| R5 | **No map of the code.** | Stale CLAUDE.md, colliding ADR numbers, misleading crate names (`temps-agent` vs `temps-agents`, an "embeddings" crate that is a tokenizer, "telemetry" that is not OTel) |

---

## 4. Priorities

Each item has an effort (S < 1 day, M 1–3 days, L = a redesign or more than a week) and a risk level.

### P0: confirmed stability bugs (do first, small PRs)

All of these were checked by reading the code.

| # | Issue | Location | Fix | Effort |
|---|---|---|---|---|
| P0.1 | Edge cache takes a **global write lock on every cache hit** to update `last_accessed`, with `.unwrap()`. A panic while holding it poisons the lock and every later request panics. | `crates/temps-edge/src/cache.rs:93`, plus `.read().unwrap()` in `route_table.rs:93,109` and `tls.rs:68,143` | `ArcSwap` snapshot (the pattern `temps-routes` already uses) plus an atomic or sampled access time | S |
| P0.2 | TCP connection filter spawns a refresh task on **every accepted connection** while the cache is stale. On a DB error, `last_refresh` is deliberately left unchanged, so during an outage each connection spawns another DB query. | `crates/temps-proxy/src/service/connection_filter_service.rs:117-165` | Single-flight guard (`AtomicBool`) plus backoff timestamp on failure; move the sets to `ArcSwap` | S |
| P0.3 | Unauthenticated ACME HTTP-01 path runs a DB query per request | `crates/temps-proxy/src/proxy.rs` (`handle_acme_http_challenge`, ~L2493) | Bounded TTL cache of pending challenges | S |
| P0.4 | `std::env::var("PREVIEW_GATEWAY_SHARED_SECRET")` on every preview request: env lock on the hot path, and a secret kept in the environment | `crates/temps-proxy/src/proxy.rs:5615` | Load once into `ArcSwap`, rotate through the settings row (encrypted) | S |
| P0.5 | Locks whose poisoning stops a whole subsystem: global `ENVIRONMENT_LOCKS` (a map that never shrinks) stops all deployment completions; the DNS zone store `expect("rwlock poisoned")` runs on every DNS query | `temps-deployments/src/jobs/mark_deployment_complete.rs:290`, `temps-node-dns/src/zone_store.rs:162,174`, `docker_cleanup_service.rs:508-526`, `temps-metrics/src/scraper.rs:235` | `parking_lot` (no poisoning) or async mutex keyed with eviction; `ArcSwap` for the zone store | S |
| P0.6 | Plugin-ordering mistake becomes a runtime panic | `temps-backup/src/services/backup.rs:1733` (`expect("BackupService.queue not set")`), `temps-core/src/plugin.rs:723,731` | Constructor injection (`Arc<Queue>` required at construction); fail at startup | S |
| P0.7 | Runtime config and **a plaintext secret** read from env, against the CLAUDE.md rule | `temps-otel/src/plugin.rs:118-173` (incl. `TEMPS_OTEL_S3_SECRET_KEY`), `temps-domains/src/tls/providers.rs:289-361`, `temps-file-store`, `temps-screenshots`, `temps-backup/src/plugin.rs:177`, `temps-wireguard`, `temps-geo` | Move to settings rows; secrets via `EncryptionService`; keep a one-release deprecation shim that reads env once and persists | M |

### P1: guardrails (stop the drift before refactoring)

Without these, each cleanup decays. Every gate below uses a **committed baseline** and fails only when a count *rises*. That lets the gates land on day one without first fixing every existing violation.

1. **Pin the toolchain.** Add `rust-toolchain.toml` so `clippy -D warnings` stops breaking when stable updates. (S)
2. **Add `[workspace.lints]`** and give every crate `[lints] workspace = true`:
   - `clippy::unwrap_used = "deny"` and `expect_used = "warn"`, with `allow-unwrap-in-tests`/`allow-expect-in-tests` in `clippy.toml`;
   - `dbg_macro` and `todo`.

   Fix the 245 production sites in one PR, using `#[allow(..., reason = "...")]` for the truly infallible ones (static header parses, `LazyLock<Regex>`). (M)
3. **Add `clippy.toml` `disallowed-methods`/`disallowed-types`:**
   - `anyhow::Context::{context, with_context}`;
   - `std::env::{var, var_os}`, allowed in bootstrap modules;
   - `tokio::sync::mpsc::unbounded_channel`;
   - `anyhow::{Result, Error}` in library crates.

   Give `temps-proxy`, `temps-edge` and `temps-routes` stricter configs that also ban `std::sync::{Mutex, RwLock}` and `parking_lot::{Mutex, RwLock}`. (S)
4. **Add a rule-drift CI script.** It counts sea-orm queries in `handlers/**`, `_ =>` in `From<…> for Problem`, `Json<Value>` returns, lazy error variants, `RequireAuth` functions with no guard, and `#[ignore]`. It compares the counts with `scripts/rule-baseline.json`. (S, and the audit script from this investigation can be reused)
5. **Add `cargo-deny`** for licences, bans and duplicate versions, with the 145 current duplicates as a baseline; **`cargo-machete`** for unused dependencies; and **coverage** (`cargo llvm-cov`, report only at first). (S)
6. **Move the heavy pre-commit hook to pre-push or CI.** It currently runs full-workspace clippy with `--all-features` on every commit and gets bypassed. Pre-commit should run `fmt` and clippy only on changed crates. (S)
7. **Finish centralising dependencies.** 436 dependency lines in 66 crates still pin versions; switch them to `workspace = true`. Remove the direct `sqlx = "0.9"` in `temps-proxy/Cargo.toml:59`, which pulls a second sqlx and contradicts the pin comment at `Cargo.toml:196`. (S–M)

### P2: split the god files (largest drop in fix rate)

Rust allows several `impl` blocks for one type across modules of the same crate. Most of these splits are therefore **pure code moves with no behaviour change**, which makes them cheap to review. Do one file per PR and move nothing else in that PR.

Ranked by size × churn:

| # | File | Prod / test lines | Proposed modules | Effort | Type |
|---|---|---|---|---|---|
| 1 | `temps-providers/src/services.rs` | 17.4k / 9.0k | `services/{types,errors}.rs`, `manager/{crud,remote,health,cluster_lifecycle,cluster_members,cluster_backup,readiness,project_links,env_vars,managed_s3,import,runtime_limits}.rs` | M | Mechanical |
| 2 | `temps-cli/src/commands/serve/console.rs` | 5.5k / 2.3k | Move `ai_read_allowlist`/`ai_write_allowlist` (~1.1k lines of string lists) to `temps-ai-api-tools`; split `start_console_api` (2,057 lines) into `validate_deps`, `register_core_services`, `register_plugins`, `build_router`, `serve` | S + M | Mechanical |
| 3 | `temps-deployer/src/docker.rs` | 5.1k / 4.0k | `docker/{log_tail,guards,build_limits,image_builder,deployer,runtime}.rs` (three trait impls, cleanest seams) | S | Mechanical |
| 4 | `temps-providers/src/externalsvc/postgres.rs` | 5.5k / 3.7k | `postgres/{config,lifecycle,walg,restore,service_impl}.rs` (the `postgres/` dir already exists) | S–M | Mechanical |
| 5 | `temps-backup/src/services/backup.rs` | 10.0k / 5.8k | `walg`, `postgres_backup`, `restore`, `s3_sources`, `schedules`, `retention`, `scheduler`, `access_scope`, `index` | M | Mechanical |
| 6 | `temps-deployer/src/compose.rs` | 10.2k / 8.3k | `compose/{support,secrets,files,resolve,security_policy,fs_confinement,exec,readiness,overrides}.rs` (the ~2.3k-line security policy is the first, cleanest target) | M | Mechanical |
| 7 | `temps-projects/src/services/project.rs` | 7.1k / 8.2k | `create`, `guards`, `settings`, `git_settings`, `webhooks/{gitlab,bitbucket,gitea}`, `queries`, `templates`, `pipeline` | M | Mechanical |
| 8 | `temps-deployments/src/services/services.rs` | 6.7k / 6.0k | `queries`, `containers`, `trigger`, `redeploy`, `reuse`, `rollback`, `promote`, `cancel`, `domains` (then dedupe rollback/promote) | M | Mechanical + dedupe |
| 9 | `temps-ai-chat/src/handlers.rs` | 10.9k / 2.5k | `handlers/{dto,errors,applications,workspace_files,workspace_terminal,conversations,attachments,permissions,pending_actions}.rs` | M | Mechanical |
| 10 | `temps-git/src/services/git_provider_manager.rs` | 7.3k / 2.7k | `providers`, `tokens`, `sync`, `oauth`, `presets`, `health`, `installations`, `push_events`, `repo_write` | M | Mechanical |
| 11 | `temps-sandbox/src/services/sandbox_service.rs` | 6.1k / 4.2k | `placement`, `app_workspace`, `data_network`, `links`, `git_seed`, `lifecycle`, `reconcile`, `preview` | M | Mechanical |
| 12 | `temps-ai-agents/src/sandbox/docker.rs` | 7.2k / 4.5k | `docker/{exec,image,network,create,snapshot,files}.rs` | M | Mechanical |
| 13 | `temps-ai-chat/src/service.rs` | 9.0k / 6.5k | Helpers first (`managed_process`, `titles`, `wire_events`, `persistence`, `prompt_budget`, `tool_dispatch`); the tool loop is item R3 below | M + L | Mixed |
| 14 | `temps-proxy/src/proxy.rs` | 7.4k / 4.4k (in 21 test mods) | `markdown`, `static_serving`, `acme`, `visitor`, `security_headers`, `timeouts`, `policy`; `request_filter` is item R2 below | M + L | Mixed |
| 15 | `temps-routes/src/route_table.rs::load_routes` | 1,663-line fn | Already commented as 7 numbered phases, so split it into 7 loader fns | S | Mechanical |

Some functions need real redesign, not code moves:

| # | Function | Problem | Target | Effort |
|---|---|---|---|---|
| R1 | `workflow_execution_service.rs::create_job_from_record` (1,944 lines) | A text match over ~20 job types | `fn from_record(ctx)` on each job in `jobs/*.rs`, plus a registry keyed by job type. Each arm moves mechanically. | M, **high value** |
| R2 | `proxy.rs::request_filter` (2,236 lines, hot path) | One function for every request phase | Ordered phase functions returning `Continue`/`Responded` (preview gateway, on-demand, path routes, ACME, HTTPS redirect, header rewrite). Benchmark before and after. | L |
| R3 | `ai-chat service.rs::try_tool_loop_in_workspace` (2,170 lines) | Not testable | An explicit per-round state machine | L |
| R4 | `deploy_image.rs::deploy_single_replica` (871 lines) | Mixes the readiness, transfer and rollout steps | Split into readiness, transfer and rollout steps | M |
| R5 | `temps-ai-agent-cli` sandbox chat turns | Two near-duplicate functions (476 and 342 lines). One of them, `sandbox_chat_stream_turn`, is dead. | Delete the dead one in P4 | – |

### P3: fix the dependency graph (build times, blast radius)

Ordered by impact over effort:

| # | Change | Why | Effort / risk |
|---|---|---|---|
| T1 | **Cut cheap wrong edges.** (a) `temps-error-tracking` and `temps-analytics-events` depend on `temps-proxy` (pingora) only for `CachedPeerTable`, which proxy re-exports from `temps-routes`; depend on `temps-routes` instead. (b) `temps-deployer → temps-dns` (41k LOC) for 2 symbols (`probe_control_plane_resolver`, `OverlayDnsSlot`); move them to `temps-node-dns` or core. (c) Remove the **22 declared but unused `temps-*` deps**, e.g. proxy→migrations, proxy→audit, otel→deployer, git→queue, backup→notifications. | Shortens the critical path immediately | S / low |
| T2 | **Decouple `temps-database` from `temps-migrations` and `testcontainers`.** Both are *production* dependencies today, because `pub mod test_utils` and `schema_guard.rs` live in the crate. Move the test utilities to a dev-only **`temps-test-utils`** crate (which also absorbs the second `TestDatabase` in `temps-analytics`, 10+ local `setup_test_db` helpers, 6 `docker_available()` helpers and the 12 hand-written `ContainerDeployer` mocks). The migrator call moves to the binary. | Migrations are the most-churned crate. A migration change would rebuild ~2 crates instead of 63. | M / med |
| T3 | **Split `temps-authz` from `temps-auth`.** 48 crates use only `AuthContext`, `Role`, `Permission`, `permission_guard!`, `RequireAuth`, but pay for OIDC, TOTP, qrcode, image and 4.7k lines of handlers. | OIDC and user changes (221 since Aug) stop rebuilding 48 crates | M / low |
| T4 | **Split `temps-config`** into the model and service (`ConfigService`, `ServerConfig`) and the handlers (6.8k lines, which pull auth and ClickHouse into 37 dependents). | Smaller blast radius | M / low |
| T5 | **Split alarms out of `temps-monitoring`.** providers, git, backup and otel only need `alarm_service`, but they inherit `monitoring → deployer`. | Breaks a domain-to-domain chain | M / low |
| T6 | **Slim `temps-core`** (76 transitive dependents). Move out `app_settings.rs` (4k lines, 47 changes) → `temps-config`; `templates.rs` + `include_dir!` → `temps-presets`; `workflow`/`workflow_executor` → `temps-deployments`; `external_plugin/` → a new **`temps-plugin-protocol`** (so `temps-plugin-sdk` and the 4 example plugins stop pulling all of core); `workflow_memory` + `temps-memory` → `temps-ai-agents`. Also replace `anyhow` in the core traits `AuditLogger`, `AuditOperation`, `EncryptionService` and `ServiceError::Internal(anyhow::Error)` with typed errors. This one change removes `anyhow` from ~20 crates' `audit.rs`. | Every core change today rebuilds everything | M / med |
| T7 | **Slim `temps-entities`.** Move the non-entity logic out (`deployment_config.rs` 1.7k, `preset.rs` 1.7k, `compose_security`, an unused copy of the tokenizer). Consider per-module registration so `lib.rs` stops being a merge-conflict hotspot. | 153 commits co-change entities and migrations | M / low |
| T8 | **Extract from `temps-ai-agents` (69k).** Consumers use only `sandbox` (27k) and `ai_cli` (9.4k). Making those standalone crates removes `temps-deployments` from the dependency chains of ai-gateway, ai-chat, sandbox, ai-agent-cli and the node agent. Invert the 3 `cluster_ca`/`DeploymentTokenService` refs with a trait. | Removes the 833k-LOC serial build chain | M–L / med |
| T9 | **Slim the node worker (`temps-node-agent`).** It pulls 53 crates, including `temps-deployments`, only for sandbox protocol and remote-service DTOs. Move those into protocol crates. | A smaller, safer worker binary | M / med |
| T10 | **Split `temps-providers` per engine.** A core crate holds `ExternalServiceManager`, DTOs and `managed_names`, plus engine crates (postgres family ~20k, mariadb ~8k, mongodb ~6k, redis ~6k, object storage ~14k). Fold `temps-query` and the 4 `temps-query-*` crates into their engines. Their only consumer is providers, and they have the **highest fix density in the repo** (4.8–7.4 per kLOC). Change the `ExternalService` trait from `anyhow::Result` to a typed error. That trait accounts for 560 `anyhow` lines and is why these errors all become 500s. | Biggest crate, second most fixes | L / med |
| T11 | **Split `temps-deployments` (152k).** First extract `temps-nodes` (~35k: node service, scheduler, SSH, pairing, WireGuard mesh, remote deployments and their handlers). Then traefik discovery (3.4k), CI tokens (5k) and docker housekeeping (4.6k). Move the 60 handler DB queries in `handlers/nodes.rs` and `handlers/remote_deployments.rs` into services as part of the move. | Largest crate, most fixes | L / high |

**Merges and renames (S each, low risk).**

- **Merges:**
  - `temps-backup-core` → `temps-backup` (single consumer);
  - `temps-embeddings` → its only consumer (it is a 299-line tokenizer, not embeddings);
  - `temps-memory` → `temps-ai-agents`;
  - `temps-static-files` (380 lines) → its consumer.
- **Renames:**
  - `temps-agent` → `temps-node-agent`, and `temps-agents` → `temps-ai-agents`. The current names differ by one letter and are the most confusing pair in the repo.
  - `temps-telemetry` → `temps-product-telemetry` (it is not OTel).
  - `temps-logs` → `temps-deployment-logs`.
  - `temps-dns-resolver` → `temps-node-dns`.

**Leave as they are.** These clusters are well-shaped: cloud (protocol → client → cloud), analytics plugins (after fixing their edges), import adapters, networking (network, wireguard, edge, routes), and the separate-binary crates (`captcha-wasm`, `git-credential`, `preview-gateway`, `pty-agent`, `vm-agent`, `agents-mcp-proxy`). Do not delete `crates/patches/opentelemetry-proto-unused`: it is a deliberate lockfile security patch.

### P4: delete dead and legacy code

**Safe now, in one PR, all verified as having no callers:**

| Item | Location | Lines |
|---|---|---|
| `sandbox_chat_stream_turn` (dead; its `retained_` sibling is used) | `temps-ai-agent-cli/src/service.rs:3062` | ~476 |
| `update_deployment_from_context`, `update_environment_current_deployment` ("DEPRECATED") | `temps-deployments/src/services/workflow_execution_service.rs:3747,3838` | ~122 |
| `update_connection_token_with_provider_validation` ("legacy") | `temps-git/src/services/git_provider_manager.rs:4798` | ~66 |
| `is_page_visit`, `delete_bucket`, `list_databases`, `NONCE_LENGTH` | proxy, s3, mongodb, providers | ~60 |
| Deprecated `TestDatabase::with_config` | `temps-database/src/test_utils.rs:686` | ~12 |
| Stale `#[allow(dead_code)]` on items that *are* used; unused serde fields in git and DNS provider DTOs | temps-git (32 attributes), temps-dns, temps-cli | ~100 |
| Never-enabled features: `temps-embeddings` `huggingface`/`openai`/`all` (dead `tokenizers`/`tiktoken-rs` deps), `temps-import-types/validation`, `temps-deployments/integration-tests` | Cargo.toml | ~12 |
| Unused deps, ~200 heuristic hits. Confirm with `cargo machete` first. Densest: temps-kv (9), temps-query-s3 (7), temps-blob (7), temps-projects (7), temps-proxy (7); `axum-macros` in 11 crates, `serde_derive` in 8. | Cargo.toml | ~150 |
| Unreferenced evidence folders: `e2e-evidence/`, `docs/pr-evidence/`, `docs/screenshots/` | repo | ~3 MB |

**Needs a deprecation cycle or a check first:**

- `apply_sandbox_egress_filter` (iptables, ~280 lines incl. tests), `temps-ai-agents/src/sandbox/docker.rs:6908`. It was superseded by the managed egress proxy. Security-adjacent: confirm the replacement covers it, then delete.
- Legacy endpoint `GET /projects/{project_id}/environments/{environment_id}/container-logs`. No first-party caller, but it is exposed by the public Node SDK, so it needs a deprecation notice and then removal.
- Removals that need a migration or a notice:
  - **`ServiceType::Minio`** (`#[deprecated]`): needs a data migration of existing rows.
  - **`webhook_deliveries.response_body`**: needs a drop-column migration.
  - **`temps upgrade --stable`**: a hidden alias.
  - **The `DEPLOYMENT_MODE` legacy env var.**
- Stale spec copies. Only `apps/temps-cli/openapi.json` is canonical. `packages/api/openapi.json` (stale since 2026-09-22, used by `apps/temps-e2e`) and `sdks/node/packages/node-sdk/scripts/openapi.json` (stale since 2026-09-09) should be generated from it or removed. That is ~180k lines of drifting generated code.
- `crates/temps-cli/GeoLite2-City.mmdb` (**58 MB in git history**). A runtime downloader already exists. Fetch the file at image build, or move it to LFS, keeping the licence notice in `NOTICE`.
- `tools/keycloak-dev`, `tools/backup-stress`, `acceptance-tests/` have no references. Ask their owners, then delete or document them.

### P5: consolidate duplicates

| Duplicate | Target | Effort |
|---|---|---|
| `constant_time_eq` (20 definitions) | `temps_core::crypto` on top of `subtle` (preview-gateway keeps its own; it is standalone) | S |
| `sanitize_slug` (7 byte-identical copies in the import adapters) | `temps-import-types` | S |
| `slugify` ×4, `escape_html` ×3, `escape_like` ×3, `sha256_hash` ×4, `hash_token` ×4, `encrypt_value`/`decrypt_value` ×3, `is_valid_domain` ×3, `is_loopback_host` ×3 | `temps-core` utility modules | S–M |
| S3 client builders ×6 | `temps-file-store::s3_client` (already canonical) | M |
| `clickhouse::Client::default()` built in 8 files despite `temps-clickhouse` existing | `temps-clickhouse` | S |
| Retry helpers (`with_retry` in status-page, `retry_with_backoff` in log-aggregator) | `temps_core::retry::RetryConfig` | S |
| Docker exec/pull/network/tar helpers (≥6 exec implementations; `Docker::connect_with*` called in 140 places in providers alone) | a small `temps-docker-util` crate: capped exec capture, pull with progress, ensure-network, tar upload | M |
| Same-named error types in several crates: `GitProviderManagerError` (defined twice in temps-git), `ClusterCaError` (config + deployments), `EnvVarError` (environments + projects) | One definition each | S |
| Audit-event boilerplate written by hand in 37 crates | `#[derive(AuditEvent)]` | M |
| Container lifecycle across engines. Postgres, mariadb, mongodb and redis share ~60 function names; `create_container` bodies are 0.87 similar. | Shared `ContainerLifecycle` helpers or default trait methods; backup and restore stay per engine. **Do this as part of ADR-055**, not separately. | M–L |

### P6: make the code legible to humans and AI agents

1. **Slim `CLAUDE.md`** from ~15k tokens to ~5k. (S)
   - Move the env-var table (17% of the file) to `docs/reference/environment-variables.md`.
   - Replace generic Rust examples with links to one real reference crate.
   - Fix the stale "51 crates" table.
   - Deduplicate against `AGENTS.md`: one is canonical and the other links to it.
2. **Add a generated crate map.** Require `description` in every `Cargo.toml` (14 of 96 have one today), then generate `docs/architecture/crates.md` from `cargo metadata`, with owner, layer, consumers and binaries. Check it in CI. (S)
3. **Give each large crate an `AGENTS.md`** (or README) of 30–80 lines: what it owns, its entry points, invariants, "do not" rules, and how to test it. Start with deployments, providers, proxy, backup, ai-chat, agents, core, entities and migrations. These are the files agents edit most and get wrong most. (M)
4. **Fix the ADRs.** Renumber the 23 files that share 10 numbers. Add `docs/adr/README.md` as an index with status, and supersede links for 052 → 055. Add a CI check that refuses duplicate numbers. (S)
5. **Decide the test-file convention** (see §7). Tests are about 38% of the lines in files over 3k lines, and they are what pushes most files out of an agent's single-read window.
6. **Add contributor skills** in `.agents/skills`. Only 3 exist today. Good first ones: "add a migration and entity", "add a handler with audit and permission", "add a plugin", "split a module". Each encodes the repo's patterns so agents do not reinvent them. (M)
7. **Remove the hand-maintained registration lists** where possible. `migration/mod.rs` (182 commits) and `entities/lib.rs` cannot become fully automatic in Rust without a build script, but a `build.rs` that generates the migration list from file names removes the most common merge conflict. The plugin list in `console.rs` becomes declarative per process profile (console, proxy, agent, stateless). Today proxy, agent and stateless wire their services ad hoc, outside the plugin system. (M)

### P7: structural, quarter-scale

1. **Workload plane (ADR-052 and ADR-055).** These are already proposed and are the right fix for R4. Projects, sandboxes and databases each create containers through their own bollard code. The two database-creation paths have already drifted on labels, restart policy, hardening and healthchecks, and backups do not run for databases on worker nodes. This plan does not redesign that work. It only asks for two things: accept the two ADRs together, and sequence them after T2, T8 and P5's `temps-docker-util`, which make the move much smaller.
2. **Split `temps-providers` (T10) and `temps-deployments` (T11)**, in that order, after their god files are split in P2.
3. **Migrations baseline at 1.0.** Collapse everything before a cut-off release into one baseline migration. Add a startup guard that refuses to migrate when `seaql_migrations` lacks the last pre-baseline name, with an "upgrade to X first" message. This removes ~35–40k lines and the fragile manual squash bookkeeping (`mod.rs:94-98`, `:860-930`). It needs a stated minimum supported upgrade version. Raise test coverage first: the crate has 2.3 tests per kLOC and 68 fixes.
4. **Typed errors end to end.** After T6 and T10, run a pass that turns the 446 lazy variants (`X(String)`, `Internal(String)`, the 9 `Database(String)` that stringify `DbErr`) into structured variants. Start with core (37), deployments (39), git (34) and auth (28).

---

## 5. Quick wins: the first two weeks

Each line is one PR, small, low-risk and independently reviewable.

- [ ] **QW1** `fix(edge)`: `ArcSwap` cache index and route table, no write lock on hits (P0.1)
- [ ] **QW2** `fix(proxy)`: single-flight and backoff for the connection-filter refresh (P0.2)
- [ ] **QW3** `fix(proxy)`: cache ACME challenges; load the preview secret once (P0.3, P0.4)
- [ ] **QW4** `fix(deployments,dns-resolver)`: non-poisoning locks, evicting environment-lock map (P0.5)
- [ ] **QW5** `fix(backup)`: inject the queue at construction, no `expect` (P0.6)
- [ ] **QW6** `build`: `rust-toolchain.toml`, `[workspace.lints]`, `clippy.toml`, rule-baseline script (P1.1–P1.4)
- [ ] **QW7** `ci`: cargo-deny with baseline, cargo-machete, coverage report (P1.5)
- [ ] **QW8** `chore`: delete the verified dead functions, stale `allow`s, unused features and evidence folders (P4, "safe now")
- [ ] **QW9** `build`: remove unused deps confirmed by machete; remove `sqlx` from temps-proxy; remove the 22 unused `temps-*` deps (T1c)
- [ ] **QW10** `refactor`: error-tracking/analytics-events depend on `temps-routes`, not `temps-proxy`; move the two deployer→dns symbols (T1a, T1b)
- [ ] **QW11** `refactor(deployer)`: split `docker.rs` into modules (P2 #3)
- [ ] **QW12** `refactor(cli)`: move the AI allowlists out of `console.rs` (P2 #2, first half)
- [ ] **QW13** `refactor`: `constant_time_eq`, `sanitize_slug`, ClickHouse client and retry consolidation (P5 rows 1, 2, 5, 6)
- [ ] **QW14** `docs`: slim CLAUDE.md, move the env-var table, fix the crate list, deduplicate with AGENTS.md (P6.1)
- [ ] **QW15** `docs`: renumber the ADRs, add an index and a duplicate-number CI check (P6.4)
- [ ] **QW16** `refactor(routes)`: split `load_routes` along its 7 commented phases (P2 #15)

## 6. Roadmap

| Phase | Weeks | Content | Exit criteria |
|---|---|---|---|
| 0: Stop the bleeding | 1–2 | P0 + P1 + quick wins | Hot-path lock and fan-out bugs fixed; lints and baselines in CI; dead code gone |
| 1: Mechanical splits | 2–6 | P2 #1, #4–#14 (one file per PR); T2 (`temps-test-utils`); T3 authz; T4 config | No production file over 5k lines among the top 15; migration changes rebuild under 5 crates |
| 2: Seams | 4–8 | R1 job registry; T5 alarms; T6 core slim (incl. typed errors in core traits); T7 entities; P5 duplicates + `temps-docker-util`; P6.2–P6.3, P6.7 | `temps-core` under 25k lines; crate map generated; registration lists generated or declarative |
| 3: Structure | 8–16 | T8 agents extract; T9 node worker; T10 providers per engine (with typed `ExternalService`); R2 `request_filter` phases (benchmarked); R3 tool loop | Providers and deployments under 60k each; node agent under 20 crates |
| 4: Platform | quarter | P7.1 workload plane (ADR-055); T11 deployments split; P7.3 migrations baseline at 1.0; P7.4 typed errors | One container-creation path; migration count reset |

**How to run it.** Mechanical splits are ideal agent tasks: one file per agent, worktree-isolated, "move only, no behaviour change, `cargo check` and tests must pass". Run them in parallel only across files in **different** crates, to avoid conflicts in `mod.rs`/`lib.rs`. Hold redesigns (R1–R4, T8–T11) to one at a time, with a human-reviewed design note first.

## 7. Decisions (made 2026-10-31)

1. **Test file convention: (b).** A file whose inline tests push it past ~1.5k lines may move them to a sibling `foo/tests.rs`, declared as `#[cfg(test)] mod tests;`. Tests stay beside the code, and this removes ~225k lines from the files agents read. CLAUDE.md is amended to match (P6 lane).
2. **Size limits: warn first.** The rule-drift script reports production files over 3k lines and functions over 300 lines against a committed baseline. It fails only when the count rises. Hard limits come after Phase 2.
3. **ADR-052 and ADR-055 are the accepted direction** for the duplicated container stacks (R4). This pass delivers the enabling steps: `temps-docker-util`, shared engine lifecycle helpers, and parity between the worker and control-plane database-creation paths (labels, restart policy, hardening, healthcheck). The full workload plane stays a separate, ADR-driven project.
4. **The migrations baseline is deferred to the 1.0 cut.** There is no 1.0 release yet and no minimum-supported-upgrade policy, and squashing without them risks broken upgrades. This pass generates the migration list instead of hand-maintaining it, and raises migration test coverage.
5. **`#[ignore]`.** Tests that only need Docker or the network become runtime skips, as CLAUDE.md requires. CLAUDE.md is amended to allow `#[ignore = "reason"]` only for tests that a dedicated CI job runs with `--ignored`.
6. **Renames happen**, as a solo step between waves 1 and 2 so they conflict with nothing.

## 8. Keeping it clean afterwards

- Baseline-gated lint and rule counts in CI, as in P1, and ratchet the baselines down as each phase lands.
- New features get a module, not a section in an existing service file. The file-size gate enforces this.
- Every new crate needs a `description`, an entry in the generated crate map, and an `AGENTS.md` if it is over 5k lines.
- Every ADR gets a unique number from the index, and the index is updated in the same PR.
- A monthly 30-minute review of the fix-density table (§2), produced by a script, to catch new hotspots early.

---

## 9. Execution and progress

**Model.** `refactor/crates-refactor-30-oct` is the integration branch, with one draft PR into `main` so the full CI runs on the combined state. Each lane runs as an agent in its own worktree and opens a PR into the integration branch. The orchestrator verifies each lane and merges it, and the next wave starts from the merged result. Lanes are assigned by **file ownership**, not by priority, so parallel lanes never edit the same files. At most 3 lanes run at once.

| Wave | Lane | Content | Status |
|---|---|---|---|
| 1 | `p0-stability` | P0.1–P0.6 (proxy, edge, dns-resolver, deployments locks, backup queue, plugin registry) | merged (#1683) |
| 1 | `p1-guardrails` | P1.1–P1.6, P0.7 (env config to settings), production unwrap/expect fixes outside the P0 files | in review (#1686) |
| 1 | `p4-cargo-hygiene` | P4 "safe now", P1.7 dependency centralisation, T1 edge cuts, unused deps (cargo machete) | merged (#1687) |
| 1b | `p6-docs` | P6.1, P6.2, P6.4, P6.6, CLAUDE.md amendments for decisions 1 and 5 | merged (#1689) |
| 1.5 | `renames` | Crate renames (solo) | merged (#1688) |
| 2 | `split-*` (one per crate) | P2 mechanical splits + R5; one lane per crate, 3 at a time | merged: deployer (#1690, #1692), providers (#1691, #1694), projects (#1695), backup (#1696), git (#1697), deployments (#1698); in progress: ai-chat, sandbox |
| 3 | `p3-*`, `p5-*` | T2–T10, merges, P5 consolidation; mostly sequential | pending |
| 4 | `redesign-*` | R1–R4, typed errors (P7.4), docker-util + lifecycle parity (P7.1 enabling steps), generated migration list (P6.7) | pending |

## Appendix: method

The findings come from five read-only investigations run on 2026-10-30 and 2026-10-31 against `refactor/crates-refactor-30-oct` (at `281ff0582`):

1. **Crate topology** from `Cargo.toml` and actual imports.
2. **Large-file structure**, with brace-depth function-length measurement that skips strings and comments.
3. **Rule-compliance audit** over production code only. Test modules were removed by `#[cfg(test)] mod` block, not cut at the first `#[cfg(test)]`, because 104 files have production code after a test module.
4. **Dead and duplicated code**, from a grep caller analysis, function-body similarity (difflib) and `Cargo.lock` duplicates.
5. **Process and churn**, from `git log --since=2026-05-01 --no-merges`, test counts, CI workflows and docs.

The P0 items, the database→migrations/testcontainers dependency, the proxy-only `CachedPeerTable` edges, the dead `sandbox_chat_stream_turn` and the deployer→dns symbol usage were checked by reading the code. Counts for unused dependencies are heuristic until `cargo machete` confirms them.
