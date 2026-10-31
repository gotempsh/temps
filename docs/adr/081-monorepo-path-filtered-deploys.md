# ADR-081: Monorepo path-filtered deploys for project group members

> **Renumbered:** this ADR was numbered 049 until 2026-10-31, when duplicate ADR numbers were removed. Older references to "ADR-049" may mean this document; the [ADR index](./README.md#renumbered-adrs) lists every renumbered ADR.

**Status:** Proposed
**Date:** 2026-09-28
**Related:** feat/project-groups branch; `crates/temps-project-groups`

---

## Context

A "project group" (`project_groups` table, crate `temps-project-groups`) gathers
several services (`projects` rows) into one parent. `project_group_members` maps
each project into at most one group (`project_id` is the PK). Services in a group
share a repository: the group holds the connection, owner, repo name, and branch;
those values are written through to each member's `projects` row so the existing
deploy pipeline continues to read `projects` unchanged. Each member keeps its own
`directory` (the subdirectory it lives in) and its own preset.

The current webhook fan-out in
`temps-git/src/services/git_provider_manager.rs::queue_push_event_for_projects`
(~line 4774) queues a `GitPushEventJob` for **every** project whose `repo_owner` +
`repo_name` matches the push, with no knowledge of which paths changed. In a
monorepo this means a one-line commit to `apps/web` triggers a full rebuild of
`services/worker`, `services/mailer`, and every other member, burning build
minutes, deploying unchanged images, and generating noise in deployment history.

The `GitPushEventJob` struct (`temps-core/src/jobs.rs`) currently carries no
information about changed paths or the pre-push commit (`before` SHA); only the
post-push commit (`after`, stored as `commit`) is passed through. The
`projects.directory` and `projects.pull_only_root_directory` columns exist and are
already read by the build pipeline, but nothing upstream uses them to filter which
projects a push should reach. The per-environment `automatic_deploy` gate exists
and is honoured in `job_processor.rs::is_automatic_deploy_enabled`, but that is an
all-or-nothing toggle, not a path predicate.

There is no existing mechanism in the git cache (`temps-git/src/services/cache.rs`)
or git ops layer (`services/git_ops.rs`) to compute a changed-file list from two
SHAs; that capability would need to be added. Provider webhook payloads carry
`before` and `after` SHAs for normal pushes; the GitHub handler (`handlers/github.rs`)
currently passes only `push_event.after` to `handle_push_event` and discards
`push_event.before`.

Building path-filtered deploys is the right point to address this, because phase 1
of project groups (monorepo members sharing a repo) makes the problem visible and
reproducible for the first time. Without filtering, the feature ships with a
regression: every push to a shared repo now costs as many builds as the group has
members.

---

## Decision

### 1. Watch-path model

Each service (`projects` row) gains an optional `watch_paths` column
(`TEXT[] DEFAULT NULL`, Postgres). When `NULL`, the service behaves exactly as
today (deploy on every push to its tracked branch). When set, the service is only
queued for deployment if at least one changed file matches at least one watch path
pattern.

A project group gains an optional `shared_watch_paths` column (`TEXT[] DEFAULT
NULL`). Paths matching any shared pattern trigger **every** member of the group.
This covers monorepo-wide inputs: lock files (`package-lock.json`,
`Cargo.lock`), workspace config (`turbo.json`, `pnpm-workspace.yaml`), and shared
packages (`packages/**`). Shared paths are ORed with per-service paths;
either match is sufficient.

Default watch path: `{directory}/**` is written into `projects.watch_paths`
when a service starts **following the group's repository**
(`project_group_members.inherits_repository` becomes true — the monorepo case),
and only if `watch_paths` is still `NULL`. Joining a group is not enough: a
group whose services each have their own repository gains nothing from path
filtering. Services not in a group, services that do not follow a group
repository, and services whose `directory` is the repository root (`/` or `.`)
are never given a default watch path — they retain `NULL` and keep today's
deploy-on-every-push behaviour.
Backwards compatibility is therefore structural, not conditional logic: `NULL`
means "no filter, deploy everything", which is exactly what every existing project
has.

Glob semantics: `**` matches any path segment sequence including zero segments,
`*` matches within one segment, `?` matches one character. Implemented with the
`glob` crate already in use elsewhere in the workspace. Case-sensitive on all
platforms (git paths are case-sensitive). Patterns are anchored to the repository
root (a pattern `apps/web/**` matches `apps/web/src/index.ts`).

Paths are stored as a Postgres `TEXT[]` column rather than a join table because
patterns belong to the service row conceptually, form a small bounded list
(practical limit: 50 patterns per field, enforced at the API), and a `TEXT[]` is
trivially queryable with `unnest` when needed. Editing watch paths goes through
the existing project settings update endpoint (`PUT /api/projects/{id}`) and the
group update endpoint (`PATCH /api/project-groups/{group_id}`). CLI parity is
`bunx @temps-sdk/cli projects update --watch-paths '...' <id>` and
`bunx @temps-sdk/cli project-groups update <group> --shared-watch-paths '...'`.

### 2. Source of changed-file lists

**Primary source: provider push payload.** The GitHub, GitLab, Gitea and
Bitbucket push payloads all include a `before` (pre-push SHA) and `after`
(post-push SHA). From those two the commit range of changed files can be fetched
via the provider's compare API (`GET /repos/{owner}/{repo}/compare/{before}...{after}`
on GitHub; the GitLab, Gitea, and Bitbucket equivalents follow the same pattern).
This is cheap (one API call), does not require a local clone, and gives an exact
list of added, modified, renamed, and deleted paths.

The `before` SHA is already present in the octocrab `PushWebhookEventPayload`
struct (`push_event.before` in the GitHub handler) but is currently discarded.
Pass it through to `handle_push_event` and into `GitPushEventJob` as
`before_sha: Option<String>` (`#[serde(default)]` for backwards compatibility
with jobs queued before this field was added). Existing handlers that do not pass
`before` (GitLab HMAC, Gitea, Bitbucket) set it to `None`.

**No local-clone fallback at fan-out.** The filter runs when the webhook
arrives, before any deployment exists; the repository is cloned later, inside
each deployment's pipeline workspace, and `temps-git`'s cache
(`crates/temps-git/src/services/cache.rs`) only caches provider API responses,
not clones. So when `before_sha` is `None` or the compare API call fails (rate
limit, API error, network error), there is nothing to diff against: fall
through to the fail-open rule. A lightweight shallow fetch of the two commits
to compute the diff is a possible later improvement, not part of this
decision.

**Fail-open rule (mandatory):** If changed files cannot be determined — `before_sha`
is `None`, the provider API returned an error, or the push is a **force push** — treat the changed-file set as
*unknown* and deploy every service with a watch path set. A force push rewrites
history; the `before..after` range is not a valid ancestry walk. Detect force
pushes from the `forced` field in the GitHub payload (and its equivalents in other
providers); pass it through as `force_push: bool` on `GitPushEventJob`
(`#[serde(default)]` → `false`). A force push triggers full fan-out regardless of
paths.

Edge cases that always deploy the service:
- `before_sha` is the all-zeros null SHA (new branch, first push to the repo):
  unknown base; fail open.
- `before_sha` == `after` (empty push, already handled by the null-SHA deletion
  guard but stated explicitly): no changes; do not deploy.
- Tag push (`tag` is `Some`, `branch` is `None`): deploy normally; no path filter
  is applied to tag-triggered deploys.
- Deleted branch (after == null SHA): already dropped in the existing null-SHA
  guard; this ADR does not change that.

The compare API call is made **once per push event** (at the fan-out site in
`queue_push_event_for_projects`) for all services sharing the same repo, not once
per service. The resulting `Vec<String>` of changed paths is passed to the
per-service filter alongside the watch paths.

### 3. Where the filter runs

The filter runs at the **webhook fan-out site**:
`queue_push_event_for_projects` in `git_provider_manager.rs`. This is the right
chokepoint because it already has the project list, the commit SHA, and the
provider context needed to call the compare API. Moving filtering later (into the
job processor) would require serialising the full changed-file list into the job
payload, which grows it unnecessarily, and would still result in a job being
queued and dequeued for every service only to be immediately dropped — wasting
queue throughput for no benefit.

The implementation order within the function:
1. Fetch the changed-file list (compare API → nil on failure, which fails open).
2. For each project: if `watch_paths IS NULL`, pass through. If `watch_paths IS
   NOT NULL`, evaluate: changed files vs `watch_paths` (per-service) plus
   `shared_watch_paths` from the group (if any). Skip if no match.
3. For skipped projects: write a **skip record** (see §5) and do not enqueue a
   `GitPushEventJob`.

The group's `shared_watch_paths` is fetched once via a single join on
`project_group_members` for all projects in the affected repo that are in a
group; ungrouped projects only consult their own `watch_paths`.

### 4. Observability of skips

A skipped service must be visible. Silent non-deployment is indistinguishable from
a bug; an operator debugging alone cannot tell whether the deploy was correctly
skipped or silently dropped.

**Recommendation: a `deployment_skips` table.** One row per skipped
(project, push, environment) tuple:

```
deployment_skips
  id              BIGSERIAL PK
  project_id      INT  FK → projects.id
  environment_id  INT  FK → environments.id (NULL if env resolution was skipped too)
  commit_sha      TEXT NOT NULL
  before_sha      TEXT
  branch          TEXT
  reason          TEXT NOT NULL        -- e.g. "no changes in services/worker since abc123"
  watch_paths     TEXT[]               -- the patterns that were evaluated
  changed_paths   TEXT[]               -- the paths that were tested against them
  skipped_at      TIMESTAMPTZ NOT NULL DEFAULT now()
```

A hypertable on `skipped_at` keeps the table bounded with the same retention
policy as deployments. The `reason` field stores a human-readable sentence: "Push
abc123..def456 changed 3 files; none matched watch paths [services/worker/**]."
When the changed-file list was unavailable the reason says so: "Changed files
could not be determined (force push); deploying all services." — in that case no
skip row is written, because the service was deployed.

Why not a `skipped` status on `deployments`? A deployment row implies a deployment
was planned and work was queued. A skip happens before the deployment row exists;
retroactively writing a deployment row for a skipped service would pollute
deployment history with phantom records and break deployment count metrics. A
separate table is cleaner and additive.

The `deployment_skips` rows are surfaced:
- In the project's deployment list via a dedicated `GET
  /api/projects/{id}/deployment-skips` endpoint (paginated, filterable by commit).
- In the Project-level grouped commit view (§6).
- In the CLI: `bunx @temps-sdk/cli deployments list --skipped <project_id>`.

### 5. Manual triggers and other cases that bypass filtering

| Trigger type | Path filter applied? |
|---|---|
| Webhook push (automatic_deploy = true) | Yes, per §2–3 |
| Webhook push (automatic_deploy = false) | No job queued at all (existing gate) |
| "Deploy" button in console | No — manual intent overrides all filtering |
| `bunx @temps-sdk/cli deployments create` | No — same as manual trigger |
| API `POST /api/projects/{id}/deploy` | No |
| Preview / PR environment deploy | No — PR environments are per-service opt-in; not affected by shared repo logic |
| Rollback | No — rollbacks recreate a specific past deployment regardless of current code state |
| Failover / recovery job | No — `DeployCaller::Platform` is infrastructure recovery, not a code push |

Manual triggers set `manual_trigger: true` on `GitPushEventJob`; the filter
checks that flag and skips evaluation when it is set.

### 6. Project-level grouped commit view

The group responds to `GET /api/project-groups/{group_id}/pushes?commit={sha}` with:

```json
{
  "commit": "def456",
  "before": "abc123",
  "branch": "main",
  "services": [
    { "project_id": 42, "slug": "api", "status": "deployed", "deployment_id": 9001 },
    { "project_id": 43, "slug": "worker", "status": "skipped", "reason": "no changes in services/worker" },
    { "project_id": 44, "slug": "web", "status": "deployed", "deployment_id": 9002 }
  ]
}
```

This is read-only aggregation over `deployments` and `deployment_skips` joined on
`commit_sha` and filtered by group membership. No new write path. UI rendering of
this view is out of scope for this ADR.

A "deploy all services for this commit" action at the group level triggers a
`GitPushEventJob` per member with `manual_trigger: true` and the specific commit
SHA; path filtering is bypassed as per §5.

---

## Alternatives considered

### Option A: Turborepo / Nx affected detection

Run `turbo run build --filter=...[<before>...<after>]` or the Nx equivalent to
determine the affected packages before queueing. This requires the build tool to
be present in the control plane, knowledge of the project's specific build
toolchain, and a full clone. It is brittle (depends on correct turbo.json / nx.json
config), adds a new runtime dependency, and couples the platform to specific
JavaScript build tools. Rejected: too narrow and too fragile.

### Option B: Build-time no-op detection via image digest

Deploy every service; in the build pipeline, compare the resulting image digest to
the current deployment's image and skip the rollout if they match. This is
conceptually clean but still burns build minutes, build workers, and cache space
for every push — the goal is to avoid the build, not to detect after the fact that
nothing changed. Rejected: solves the wrong half of the problem.

### Option C: Always deploy all members

Keep today's behaviour and document it. Acceptable for groups of 2–3 services;
unacceptable for a monorepo with 10+ services. Rejected as a goal state but
remains the correct fallback (fail-open rule, §2).

### Option D: Per-commit changed-path list in `GitPushEventJob`

Compute the changed-file list before enqueueing and embed it in the job. The job
processor then does the filtering. Rejected: it grows the job payload with a
potentially large list (hundreds of files), requires the fan-out site to already
have the list (same work as filtering there), and adds no benefit — the fan-out
site is the natural chokepoint.

### Option E: Watch paths as a separate join table

Separate rows in a `project_watch_paths` table instead of a `TEXT[]` column.
Adds join complexity for every deployment decision for no gain; a bounded list of
patterns is not a candidate for relational normalisation. Rejected.

---

## Security considerations

**Glob DoS.** Patterns like `{a,b,c,...}**` with many alternatives, deeply nested
`**/**/**`, or catastrophically-backtracking patterns could cause the glob
evaluation to take unbounded time against a large changed-file list. Mitigations:
cap patterns at 50 per field (API validation); cap individual pattern length at
200 characters; cap the changed-file list evaluated per push at 10,000 paths (any
push with more paths falls through to fail-open, which deploys everything — no
path is silently dropped). Glob evaluation happens off the hot path (fan-out, not
per-request), so a short per-evaluation timeout (100 ms) is also acceptable.

**Path traversal.** Patterns are matched against paths as supplied by the provider
API or git diff. Provider APIs return repository-relative paths without `..`
segments; validate that each path in the changed-file list is relative (no leading
`/`, no `..` component) before evaluation. Reject any pattern containing `..`.

**Untrusted payload data.** The `before` SHA comes from the provider webhook
payload, which is attacker-controlled for providers that do not sign their payloads
(e.g. before a webhook secret is configured). A crafted `before` SHA could in
theory cause the compare API to return a misleading changed-file list, causing
services to be skipped that should have deployed. This is already a problem for
deployment correctness without path filtering (a crafted `after` SHA could point
to an attacker-controlled commit). The existing webhook signature verification
(GitHub HMAC, GitLab token, Bitbucket secret-in-path, Gitea HMAC) guards this;
for providers where verification is optional, the risk is unchanged from today. The
fail-open rule ensures that a returned empty changed-file list (plausible for a
crafted `before` == `after`) would still trigger all services because the empty
set never matches any watch path — wait, that would skip everything. Counter that:
if the changed-file list is returned as empty (zero files changed) AND `before` !=
`after` AND this is not a force push, treat it as suspicious and fall open. A
non-force push that advances the commit pointer must have changed something.

---

## Consequences

### Positive

- Monorepo pushes no longer trigger rebuilds of services whose code did not
  change; build minutes drop proportionally to the group size and commit locality.
- Deployment history is cleaner: only deployments that correspond to real code
  changes appear per service.
- Fail-open rule ensures no legitimate deploy is ever silently dropped; the
  worst case is a spurious build, not a missed one.
- Backwards compatible by construction: `watch_paths IS NULL` = today's behaviour.

### Negative

- New `deployment_skips` table to manage, retain, and back up.
- The compare API call adds one provider API round-trip per push event (already
  cached if `before`/`after` are known at the fan-out site; not on the
  critical path because fan-out is async).
- Watch-path configuration is one more concept users must understand; poorly
  configured patterns silently skip deploys — mitigated by the skip log and the
  fail-open default for ungrouped services.
- `TEXT[]` columns on `projects` and `project_groups` widen those rows slightly.

### Risks

- GitHub commit compare API has a 250-commit cap on commits included in the
  response; with `?per_page=1` and only requesting the `files` array it returns
  all changed files up to GitHub's internal 300-file limit. Pushes that change
  more than 300 files fall through to fail-open (fail-open cap in §2). The
  300-file limit is per-API-call; a workaround (paginate the file list from the
  git diff) is tracked as follow-up. This risk is bounded: fail-open deploys
  everything, no deploy is skipped.
- With no local-clone fallback, any compare API failure (rate limit, outage)
  means a full fan-out for that push. That costs builds, never correctness.
- Glob evaluation performance with large changed-file lists and many watch patterns
  is bounded by the caps above; this should be measured on real monorepos before
  GA.

### Open question: clone cost per service

Path filtering reduces how many services build per push, but each one that
does build still clones the whole repository on its own. Measured on
`Infisical/infisical` (two services imported from one Project, public repo):
each deployment's checkout was 3.2 GB, so a four-service push would move
roughly 13 GB. The existing per-service `pull_only_root_directory` sparse
checkout avoids this, but it is off by default because many monorepo apps build
against shared packages at the repository root (`packages/*`, workspace lock
files). Options, not decided here: a shallow clone (`--depth 1`) when the
deployment targets a branch head rather than an older SHA; a clone cache shared
by the services of one Project for the same commit; or proposing sparse
checkout at import time for apps whose scan shows no root-level workspace.

---

## Migration / rollout plan

1. **Schema migration (non-breaking):** Add `watch_paths TEXT[] DEFAULT NULL` to
   `projects` and `shared_watch_paths TEXT[] DEFAULT NULL` to `project_groups`.
   Add `deployment_skips` table with hypertable on `skipped_at`. No existing
   behaviour changes; all existing `watch_paths` are `NULL`.

2. **`GitPushEventJob` extension:** Add `before_sha: Option<String>`,
   `force_push: bool`, both `#[serde(default)]`. Existing queued jobs deserialise
   cleanly; `before_sha = None` triggers the fallback / fail-open path.

3. **Fan-out update:** Update `queue_push_event_for_projects` to: accept
   `before_sha` and `force_push`; fetch changed files; evaluate watch paths; write
   skip rows; enqueue selectively. No separate feature flag: filtering only
   ever applies to services with non-`NULL` `watch_paths`, which are only
   populated for services following a group repository — a capability that
   ships with the Experimental `project-groups` feature. No existing project
   changes behaviour on upgrade.

4. **Handler updates:** Update the GitHub, GitLab, Gitea, Bitbucket, and Generic
   push webhook handlers to extract and pass `before_sha` and `force_push` from
   their respective payloads.

5. **API and CLI:** Add `PATCH /api/projects/{id}` support for `watch_paths` field.
   Add `PATCH /api/project-groups/{group_id}` support for `shared_watch_paths`.
   Add `GET /api/projects/{id}/deployment-skips`. Add
   `GET /api/project-groups/{group_id}/pushes`. Ship `bunx @temps-sdk/cli`
   commands for the same endpoints.

6. **Enable by default for services following a group repository:** when a
   member starts following its group's repository, write `{directory}/**` into
   `projects.watch_paths` if it is currently `NULL` and `directory` is not the
   root. This is the only automatic population; existing projects are never
   changed.

7. **Promote with the feature:** path filtering leaves Experimental together
   with `project-groups`, once validated on at least one real monorepo with the
   skip log confirmed correct.

---

## Testing plan

- Unit tests: glob matching logic against edge-case paths (`..`, absolute paths,
  root directory, empty pattern list, empty changed-file list, renamed files with
  both old and new names present).
- Unit tests: fail-open rule for each trigger condition (force push, null `before`,
  compare API error, empty-but-nonequal before/after).
- Unit tests: `GitPushEventJob` deserialisation with missing `before_sha` and
  `force_push` fields (backwards compat).
- Integration test: a real Git repository (Gitea, using the existing testcontainer
  harness) with two subdirectory services; assert that a commit touching only
  `apps/web/` produces a deployment for `web` and a skip record for `worker`.
- Integration test: a force push triggers both services regardless of paths.
- Integration test: a push to a service with `watch_paths IS NULL` always deploys.
- E2E: a Playwright spec under `web/e2e/authenticated/` — create a group with a
  repository and two services in different directories, push to one directory,
  verify the other shows a skip entry with its reason in the console.
