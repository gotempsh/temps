# AGENTS.md

The rules for working on Temps, for people and for every coding agent. Claude
Code loads this file through `CLAUDE.md`; Codex, Cursor and others read it
directly. **Core philosophy: code that works safely, and when it fails,
explains why comprehensively.**

Read [`REVIEW.md`](./REVIEW.md) before opening a PR: the recurring review
findings (authorization scope, secrets in logs, stale writes, cancellation and
cleanup, resource limits, migrations) and a pre-PR checklist.

| Need | Go to |
|---|---|
| What each crate does, its layer and consumers | [`docs/architecture/crates.md`](./docs/architecture/crates.md) (generated) |
| A crate that follows the rules below end to end | [`crates/temps-flags`](./crates/temps-flags): `error.rs`, `services/flag_service.rs`, `handlers/{handler,types,audit}.rs`, `plugin.rs` |
| Step-by-step recipes (skills) | [`temps-add-migration-and-entity`](./.agents/skills/temps-add-migration-and-entity/SKILL.md), [`temps-add-handler`](./.agents/skills/temps-add-handler/SKILL.md), [`temps-add-plugin`](./.agents/skills/temps-add-plugin/SKILL.md), [`temps-split-module`](./.agents/skills/temps-split-module/SKILL.md), [`temps-design-system`](./.agents/skills/temps-design-system/SKILL.md), [`start-temps`](./.agents/skills/start-temps/SKILL.md) |
| Environment variables the binary reads, and why | [`docs/reference/environment-variables.md`](./docs/reference/environment-variables.md) |
| Architecture decisions | [`docs/adr/README.md`](./docs/adr/README.md); a new ADR takes `python3 scripts/adr_index.py --next` |

## Critical Rules

### NEVER

- **Put a real user's, customer's or third party's identity into anything that leaves this machine.** The repository is **public**. Company, product and account names, internal hostnames, contract/method names, real trace/span/request IDs, or any other detail that identifies whose system produced a payload never appear in code, comments, test fixtures, commit messages, branch or file names, PR titles, descriptions or comments, or issue text.
  - *Reproducing a reported bug*: keep the shape that makes the payload valuable (timings, ordering, precision, nesting, sizes, edge cases); rename services, operations and hosts to generic ones and regenerate every identifier. A fixture that identifies nobody is strictly better.
  - *Describing it*: say "a reported cross-project trace", never who reported it.
  - *A live task target is scratch input, not evidence*: a real URL, repo or account a user hands you ("deploy github.com/someone/their-repo") must not end up in tests, fixtures, commits or PRs as an example, even though the user supplied it. Write against a generic case ("a repo with no build manifest, just an `index.html`").
  - *Allowed*: public provider names and logos that identify supported integrations, and upstream URLs and copyright/license notices that attribute third-party assets; keep required attribution. This never covers customer accounts, private hostnames, credentials or captured payload identifiers.
  - *Before pushing*, grep the diff and any PR body for the reporter's names and identifiers. Once on GitHub it is effectively permanent: force-pushing does not remove a PR's recorded commits or diff, PRs cannot be deleted, forks keep the objects.
- **Put the local build or verification environment into public text** (PRs, commits, code comments): machine paths, cache helpers or servers, Docker socket paths, local toolchain workarounds. Report verification as the generic commands and their results.
- **Commit `.env` files, credentials or secrets**, including local dev-instance artifacts (encryption keys, auth secrets, generated tokens, `temps_data`-style directories). Run `git status` before staging and scrutinize every path you did not intentionally edit; a broad `git add` after running a local instance is the usual cause. A committed secret is compromised: stop tracking it and tell the user whether history needs rewriting; never force-push without asking.
- **Sweep unrelated dirty files into a commit.** If the tree was dirty when you arrived, ask before staging those files.
- **Access the database from an HTTP handler.** Handlers call services.
- **Return untyped JSON** (`serde_json::Value`). Use typed structs.
- **Use anyhow's `.context()`/`.with_context()`.** Use `.map_err()` into a typed error. Enforced as `disallowed-methods` in `clippy.toml` (the `temps-cli` bootstrap binary is exempt).
- **Use `.unwrap()`/`.expect()` in production code.** Use `?` or explicit handling. `unwrap_used`, `expect_used`, `dbg_macro` and `todo` are denied workspace-wide (tests are exempt via `clippy.toml`). For a truly infallible call prefer restructuring; otherwise `#[allow(clippy::unwrap_used, reason = "why it cannot fail")]` on the narrowest item. Every lint `allow` needs a `reason`.
- **Use `anyhow::Result` in the service layer.** Use typed `thiserror` enums.
- **Expose stored sensitive data** (API keys, tokens, passwords) in responses; mask them. The one exception is a credential minted by the request itself (a new API key, an admin-reset temporary password): returned exactly once in plaintext with `Cache-Control: no-store`, never persisted in retrievable form, never logged or audited.
- **Write N+1 queries.** Use JOINs for related data.
- **Leave the project in a non-compilable state.**
- **Use `#[tokio::main]` when integrating with pingora.**
- **Log in plain text.** Use structured logging (JSONL for deployment logs).
- **Overwrite `apps/temps-cli/openapi.json` with the raw server response.** Use `bun run spec:update` ([OpenAPI clients](#openapi-clients)).
- **Create markdown documentation files** unless explicitly asked.
- **Mark Docker tests `#[ignore]`.** They skip at runtime when Docker is missing. `#[ignore = "reason"]` is allowed only on a test a dedicated CI job runs with `--ignored`, or a known-broken/manual test whose reason starts with `known broken:` / `manual:` and says what is wrong. A bare `#[ignore]` fails CI (`scripts/rule_drift.py`).
- **Write error types with generic messages.** Include IDs, names and the operation.
- **Expose internal dependencies through public accessors** (`service.db()`). Pass them via the constructor or `AppState`.
- **Use `Option<T>` for a required dependency.** Use `Arc<T>` and fail at startup if it is missing.
- **Use `get_service` for a required plugin dependency.** Use `require_service`, which fails fast with a clear error.
- **Add runtime configuration as an environment variable** ([next section](#configuration-is-a-column-not-an-environment-variable)).
- **Hand-edit `CHANGELOG.md`.** [git-cliff](https://git-cliff.org) generates it from commit subjects at release time (concurrent edits used to conflict constantly); preview with `scripts/changelog.sh --unreleased`.
- **Pass `--no-verify`** unless the user explicitly asks. If a hook is broken, fix the hook.
- **Mutate Docker resources you did not create.** A host may already run a live Temps instance or operator-owned resources: never stop, remove, prune, rebuild or retag existing containers, images, volumes or networks. Tests create uniquely named resources and clean up only their own.

### Configuration is a column, not an environment variable

Model new runtime configuration as a column on the entity row it configures
(`oidc_providers.trust_idp_email`, not `TEMPS_OIDC_SKIP_EMAIL_VERIFIED`), so the
admin can change it per record at runtime through the API/UI, it is audit-logged
by the normal write path, it survives upgrades, and nobody restarts the binary
to change one tenant's behaviour. An environment variable is process-wide:
changing it for one tenant changes it for everyone. A sensitive value
(credentials, tokens, private keys, a Vault CA bundle or auth token) is a column
**encrypted at rest with `EncryptionService`**, never plaintext, never an env var.

The only exceptions are bootstrap values needed before a database connection
exists (`DATABASE_URL`, `TEMPS_DATA_DIR`, `--license-path`), and one-shot
first-boot *inputs* consumed exactly once whose result is persisted and
audit-logged like any other write (`TEMPS_ADMIN_EMAIL`/`TEMPS_ADMIN_PASSWORD_FILE`,
`TEMPS_CLOUD_ENROLLMENT_CODE`). Nothing may read such a variable to decide runtime
behaviour, and it is never the durable home of the value. The bar for anything
else is "this can only ever have one value per running process"; almost nothing
meets it, so before adding `TEMPS_FOO_BAR` ask whether `entity.foo_bar` would do.
Enforced: `std::env::{var, var_os}` are `disallowed-methods`; a legitimate read
needs `#[allow(clippy::disallowed_methods, reason = "bootstrap: ...")]` on the
narrowest item. Existing variables and their justification:
[`docs/reference/environment-variables.md`](./docs/reference/environment-variables.md).

### ALWAYS

- **Add the SPDX header** to every new first-party source or commentable configuration file, in its comment syntax: `SPDX-FileCopyrightText: 2024-2026 Temps Contributors` and `SPDX-License-Identifier: MIT OR Apache-2.0`. Apply it with `python3 scripts/source_attribution.py annotate <file>`, and run `python3 scripts/source_attribution.py check` before every commit that adds or regenerates source files; failures block. Generated files get the header from their generator. Never replace or misattribute third-party notices.
- **Run `cargo check --lib`** (or `-p <crate>`) after every modification; only build when you are at least 99% sure it compiles. New code compiles **without warnings**; fix them all before calling work complete.
- **Opt every new crate into the workspace lints** (end its `Cargo.toml` with `[lints]` / `workspace = true`; levels live in `[workspace.lints]` in the root `Cargo.toml`, configuration in `clippy.toml`), give it a one-line `description`, and add it to `LAYERS` in `scripts/crate_map.py`.
- **Ship a test with every behaviour change, in the same PR**, and run it. It fails without the change; "verified manually" does not count, even for a one-line fix. When you change a default, message or response shape, update the existing tests that assert the old one.
- **Keep Rust unit tests next to the code**: in the same file, or, when inline tests push it past ~1,500 lines, in a sibling `tests.rs` declared `#[cfg(test)] mod tests;` (e.g. `backup/mod.rs` + `backup/tests.rs`). Never in a distant directory. Frontend tests are adjacent `name.test.ts`/`.test.tsx` files, so `bun:test` imports stay out of production modules.
- **Follow the three layers**: handlers (auth, permissions, DTOs, audit, OpenAPI) → services (business logic, typed errors, validation, orchestration) → Sea-ORM data access. All business logic lives in services.
- **Use typed errors with contextual messages** (IDs, resource names, paths) and propagate them with `?` ([Errors](#errors)).
- **Authorize every handler** with `permission_guard!` (plus the project-scope guards when a `project_id` is involved), and **audit-log every write** (CREATE, UPDATE, DELETE).
- **Paginate lists**: default 20, max 100, sorted `created_at` DESC.
- **Return dates in ISO 8601 with a `Z` suffix** (`DateTime<Utc>`, never `NaiveDateTime`).
- **Log with structure and an explicit level.**
- **Give each component its own `Arc` clone** of shared dependencies; never share them through accessor methods.
- **Use `require_service`** in plugins for dependencies the app cannot function without.
- **Let the user configure and control their setup**: show status and give instructions; never do things silently on their behalf.
- **Design new features for a small footprint** ([Scalability & Efficiency](#scalability--efficiency)) and **give every feature a visible surface that onboards when unconfigured** ([Feature Discoverability](#feature-discoverability)).
- **Use Conventional Commits and sign off every commit** ([Commits and pull requests](#commits-and-pull-requests)).

## Errors

The chain is `DbErr` / `io::Error` / external error → the crate's domain enum
(via `From` or `map_err`) → `Problem` (RFC 7807 `application/problem+json`, via
`From<DomainError> for Problem` in the handler module). Errors never travel as
strings or through anyhow. Reference: [`crates/temps-flags/src/error.rs`](./crates/temps-flags/src/error.rs)
and `impl From<FlagError> for Problem` in [`handlers/handler.rs`](./crates/temps-flags/src/handlers/handler.rs).

- Every crate owns a `thiserror` enum. Variants carry structured fields
  (`NotFound { project_id: i32, key: String }`), use `#[from]` for common
  sources (`sea_orm::DbErr`, `std::io::Error`), and have a grep-able
  `#[error("...")]` that says what failed, on what, and why. Every `NotFound`
  includes the ID searched for.
- No lazy variants: `#[error("Database error")]`, a bare `NotFound`,
  `#[error("Operation failed")]`, `Internal(String)` catch-alls.
- `From<sea_orm::DbErr>` maps semantically: `RecordNotFound` → the domain
  `NotFound`, duplicates → a validation/conflict variant, the rest →
  `Database(DbErr)`.
- `From<DomainError> for Problem` matches **every variant explicitly, no
  `_ =>`**: 404 not found, 400 validation, 409 conflict, 500 internal, each with
  `.with_title(..)` and `.with_detail(error.to_string())`. Errors built inline in
  a handler use `temps_core::error_builder` (`not_found()`, `bad_request()`, or
  `ErrorBuilder::new(status).type_(..).value(key, value)` for structured metadata).

## Scalability & Efficiency

Temps is one binary on small machines while its proxy may serve very high
request rates. Two targets are requirements: **2 vCPU / 4 GB with Temps and its
PostgreSQL/TimescaleDB on one host must run well** (the default experience, with
room left for workloads); **1 vCPU / 2 GB with an external database must run**,
every feature working, nothing OOM-killed. Design against the smaller one:
caches, pools, buffers, background tasks and per-workload sidecars share 2 GB
and one core with the OS and the user's workloads. Efficiency is a requirement
for new functionality, not an optimization to defer.

- **Hot path** (per request in `temps-proxy`/`temps-edge`, per event in
  analytics/error ingest, per sample in metrics), assume 100k+ ops/s:
  - no synchronous I/O and no per-operation network, DB or disk call: aggregate
    in memory (atomics, sharded counters) and flush on an interval, or push into
    a bounded channel drained by a background batcher;
  - no `Mutex`/`RwLock` on the request path: atomics, `ArcSwap` for
    rarely-updated config (the route-table pattern), thread-locals, sharding;
  - any channel fed by the hot path is bounded and sent with `try_send` under a
    documented drop/degrade policy, with drops counted in a metric.
    `tokio::sync::mpsc::unbounded_channel` is a `disallowed-method` everywhere;
    allow it only with the reason it cannot grow without bound;
  - bounded label/key cardinality (status class, not full URL; project ID only
    if project count is bounded);
  - avoid per-request allocation and serialization: reuse buffers, prefer
    `&str`/`Bytes`, no `serde_json::to_string` per request.
- **Control plane** (handlers, services, jobs): normal rules, bounded memory, no
  unbounded fan-out.
- **Everywhere**: stream or chunk anything unbounded (log tails, backups,
  exports), never load it into a `Vec`; batch high-volume writes to
  ClickHouse/TimescaleDB/Postgres with max-size and max-age flushes; prefer
  pull/scrape over per-event push (`MetricsScraper`); make background loops
  O(changes) (`updated_at` cursors, NOTIFY), not O(rows).
- A PR touching the hot path or a high-volume flow states the expected load, the
  memory bound and the behaviour at saturation (drop, degrade, backpressure).

## Feature Discoverability

A feature the user cannot find does not exist; self-hosted operators debug
alone, with no support channel to ask "does temps do X?".

- Every feature has a visible entry point where the task happens, not only in a
  settings page visited once. A keyboard shortcut is an accelerator, never the
  only way in. Name the outcome ("Ask a question about this data"), not the
  mechanism.
- **Never hide a feature because optional configuration is missing** (an AI
  provider, S3 credentials, SMTP, a DNS token). Render it in an onboarding state
  that shows what it would do with a concrete example, states exactly what is
  missing ("No AI provider is configured", never a bare disabled control),
  deep-links to the settings section that fixes it, and explains the gap if
  triggered instead of no-oping or spinning:
  `<AiQueryBar configured={aiConfigured} onboardingHref="/settings/ai" example="show me the users created last week" />`,
  never `{aiConfigured && <AiQueryBar />}`.
- Back it with a typed capability endpoint (`configured: bool`, `reason:
  Option<String>`, `setup_path: Option<String>`), so the client can tell "does not
  exist" from "not set up yet" without inferring it from a 404 or 500.

## Resilience Patterns

- **Retry** transient failures (external APIs such as GitHub/GitLab, database
  reconnects, temporarily unavailable container operations) with
  `temps_core::retry::RetryConfig` (exponential backoff, bounded delay). Never
  retry validation, authentication or not-found errors.
- **Time out** every external operation: `reqwest` client timeouts,
  `tokio::time::timeout` around connections, wait loops bounded by a deadline that
  return a typed timeout error.
- **Degrade, don't crash**: a failed audit-log write is logged and the operation
  still succeeds; a missing optional subsystem (GeoIP) yields `None`; Docker tests
  skip when Docker is absent.
- **Clean up on every path**: remove what you created when a later step fails,
  use `Drop` for guaranteed cleanup, abort the background tasks you own.
- **Use a transaction** for multi-step writes (`db.begin()` … `txn.commit()`;
  dropping it rolls back).

## Testing

Error paths matter as much as the happy path. For every service method test the
happy path, not found (the error carries the right ID), validation failures, a
database error and edge cases (empty strings, zero, boundaries). For every
handler test 401 without a token, 403 with the wrong role or scope, the success
status and body, and the Problem Details shape of errors. Real patterns:

- service tests on Sea-ORM `MockDatabase`: `crates/temps-flags/src/services/flag_service.rs`;
- handler guard tests: `mod tests` in `crates/temps-flags/src/handlers/handler.rs`;
  401s and audit assertions through the router with `oneshot`:
  `crates/temps-deployments/src/handlers/wireguard_mesh.rs`;
- trait-based mocks or `mockall` for external services; `TestDatabase`
  (`crates/temps-database/src/test_utils.rs`) for real-database tests, one
  isolated schema per test, cleaned up on drop;
- Docker tests check `docker.ping()` and return early with a message when Docker
  is unavailable.

Run what you wrote immediately: `cargo test --lib -p <crate> <test_name>`, then
the whole crate for regressions.

## Architecture

- **Services**: dependencies injected through `new(..)` as `Arc<T>` or
  `Arc<dyn Trait>`; methods validate input first, return
  `Result<T, DomainError>` and propagate with `?` through `From` impls.
- **Handlers** (recipe: [`temps-add-handler`](./.agents/skills/temps-add-handler/SKILL.md)):
  `RequireAuth` → guard → service call → audit → typed response, returning
  `Result<impl IntoResponse, Problem>`. Routes use `{param}`, never `:param`.
  Responses are DTOs built with `From`. Every handler and schema is registered in
  the crate's `ApiDoc` with all its response codes.
- **Permissions**: `permission_guard!(auth, BackupsCreate)` (equivalently
  `permission_check!(auth, Permission::BackupsCreate)`) returns a structured 403.
  Permission names are `{Domain}{Read|Write|Create|Delete}`.
- **Plugins** (recipe: [`temps-add-plugin`](./.agents/skills/temps-add-plugin/SKILL.md)):
  crates register services through `TempsPlugin` with type-safe DI. Every plugin
  registers its services first, then every plugin initialises, so services can
  cross-reference.
- **Large files** split by responsibility ([`temps-split-module`](./.agents/skills/temps-split-module/SKILL.md));
  new features get a module, not a section in an existing service file.

## Data, logging and secrets

- **Database**: Sea-ORM on PostgreSQL with TimescaleDB. Raw queries use
  `DatabaseBackend::Postgres` with `$1`, `$2` binding. Never cast `time_bucket()`
  at the same query level as `GROUP BY`; bucket in a subquery. Schema changes:
  [`temps-add-migration-and-entity`](./.agents/skills/temps-add-migration-and-entity/SKILL.md).
- **Logging**: `error!` for database, auth and unrecoverable failures; `warn!` for
  rate limits, retries and degraded functionality; `info!` for business events;
  `debug!` for initialisation and configuration. Always include IDs and context.
  Deployment and build logs are JSONL through `temps-deployment-logs`
  (`log_info`, `log_success`, `log_warning`, `log_error`).
- **Secrets**: stored keys, tokens and passwords are masked as `***` (or reported
  as a `has_secret` flag) in responses; encrypted at rest with `EncryptionService`
  (AES-256-GCM), S3 credentials included; session tokens use `CookieCrypto`.

## Platform specifics

- **Bollard 0.19+**: `bollard::query_parameters::*` (builders such as
  `CreateContainerOptionsBuilder`) and `bollard::models::*`, not
  `bollard::container::*`; `ContainerCreateBody` replaces `Config`; boolean
  option fields are plain `bool`, not `Option<bool>`.
- **Pingora**: command `execute()` methods are synchronous; create a local tokio
  runtime only for specific async work (DB connections); pingora manages the main
  runtime after startup.

## Frontend (`web/`)

The console's rules (design standard, React, state, UI, mobile, pagination, the
generated SDK) are in [`web/AGENTS.md`](./web/AGENTS.md), which tools load when
you work under `web/`. Read it before any frontend change.

## OpenAPI clients

| Client | Source of truth | Refresh with |
|---|---|---|
| `web/src/api/client/` | the live server | `cd web && bun run openapi-ts` |
| `apps/temps-cli/src/api/` | the committed `apps/temps-cli/openapi.json` | `cd apps/temps-cli && bun run spec:update && bun run generate:api` |

After any change to handlers, request/response shapes, schemas or routes:
restart `temps serve` ([`start-temps`](./.agents/skills/start-temps/SKILL.md)),
refresh both clients, and commit the regenerated files so reviewers see the API
delta. TypeScript errors like "has no exported member" mean a stale SDK. CLI docs
regenerate with `bun run scripts/generate-docs.ts --output docs/CLI.md` and
`--format mdx --output docs/CLI.mdx` in `apps/temps-cli`.

- `apps/temps-cli/openapi.json` stays canonical: keys sorted recursively,
  two-space indent, trailing newline. Only `bun run spec:update` writes it; the
  server serves it minified with unstable key order, so `curl ... >
  openapi.json` reports ~92,000 deletions. `bun run spec:check` verifies it
  offline (pre-commit hook and the **OpenAPI Spec Format** job); `--fix` only
  reformats, it never fetches. Against a non-default server:
  `TEMPS_API_KEY=tk_... bun run spec:update --url <server>/api/api-docs/openapi.json`. Sanity check: `git diff --numstat --
  apps/temps-cli/openapi.json` shows hundreds of lines, never tens of thousands.
- Conflicts in either client are build output. Never hand-merge them: take one
  side (`git checkout --ours -- web/src/api/client apps/temps-cli/src/api
  apps/temps-cli/openapi.json`), regenerate from a server built off the merged
  source, then typecheck both `web/` and `apps/temps-cli/`.
- Never add a plugin-only route or schema (served by a plugin crate outside this
  repository) to `apps/temps-cli/openapi.json`. For CLI parity hand-write local
  request/response interfaces and call the shared `client` directly (pattern:
  `apps/temps-cli/src/commands/otel-forward/index.ts`).

## Build, lint and CI

```bash
cargo check --lib -p <crate>                 # after every change
cargo test --lib -p <crate> [test_name] [-- --nocapture]
cargo build --bin temps                      # debug build, skips the web UI
cargo build --release --bin temps            # release build, includes the web UI
FORCE_WEB_BUILD=1 cargo build                # debug build with the web UI
```

- **Toolchain**: pinned in `rust-toolchain.toml`. Bump it there and in every
  workflow's `toolchain:` input in the same PR;
  `.github/scripts/check-toolchain-pin.py` fails CI on drift. An optional shared
  build cache is described in [`docs/testing/build-cache.md`](./docs/testing/build-cache.md).
- **Lints**: `[workspace.lints]` and `clippy.toml` enforce the unwrap/expect,
  `.context()`, env-var and unbounded-channel rules above. Prefer fixing the code
  (`?`, `map_err`, `let-else`, a bounded channel) over an `allow`.
- **Hooks** (`scripts/setup-hooks.sh`): at commit `cargo fmt`, clippy on the
  changed crates and the rule-drift check; at pre-push the full
  `cargo clippy --workspace --all-targets -- -D warnings`. If `cargo fmt` changes
  files during a commit, re-stage and commit again.
- **Rule drift**: `scripts/rule_drift.py` counts per crate the rule violations
  clippy cannot express (DB access in handlers, `_ =>` in
  `From<..> for Problem`, `serde_json::Value` responses, lazy error variants,
  unguarded `RequireAuth`, `anyhow` in library crates, files over 3,000 lines,
  functions over 300 lines, `#[ignore]` without a reason) against
  `scripts/rule-baseline.json`; CI and pre-commit fail when a count rises. After
  fixing violations, lower it with `python3 scripts/rule_drift.py
  --update-baseline` and commit the baseline; `--list <metric>` shows where the
  rest are.
- **Generated docs** are checked in CI: `python3 scripts/crate_map.py --check`
  (crate map and Cargo descriptions) and `python3 scripts/adr_index.py --check`
  (unique ADR numbers and the index).

## Commits and pull requests

- **Conventional Commits**: `type(scope): description`, where type is one of
  `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`,
  `chore`, `revert` and scope is the crate or area (`backup`, `web`,
  `deployments`). The subject becomes the changelog entry (a non-conventional
  commit is dropped from it), so make the user or operator impact clear. The
  `Changelog` workflow validates **every** non-merge commit in `base..HEAD` and
  fails the PR on one bad subject. `git revert` defaults to `Revert "..."`, which
  is not conventional: pass an explicit conventional `-m`, or amend right after.
- **DCO sign-off**: every commit carries a `Signed-off-by: Name <email>` trailer;
  the DCO check also validates every commit in `base..HEAD`. Always
  `git commit -s` (and `-s` on `--amend` and `revert`); never a plain
  `git commit`. Repair with `git commit --amend --no-edit -s`. Before opening or
  updating a PR, check every commit in the range
  (`git log origin/main..HEAD --format='%h%n%B%n---'`); the hooks do not add it.
- Read [`REVIEW.md`](./REVIEW.md) before opening the PR.
