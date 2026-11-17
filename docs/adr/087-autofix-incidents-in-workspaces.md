# ADR-087: Production crashes are fixed as incidents in a workspace

> **Renumbered:** this ADR was numbered 054 until 2026-10-31, when duplicate ADR numbers were removed. Older references to "ADR-054" may mean this document; the [ADR index](./README.md#renumbered-adrs) lists every renumbered ADR.

**Status:** Proposed
**Date:** 2026-10-20
**Author:** David Viejo
**Builds on:** ADR-074 (sandbox snapshots), ADR-084 (sandbox service access), ADR-086 (workspaces hold the chats)
**Supersedes:** the standalone Autofixer run flow (`crates/temps-agents/src/services/autofixer.rs`)

---

## Context

When a new crash reaches production, an agent should reproduce it, fix it,
prove the fix, and hand an engineer something to ship, or ship it itself when
the evidence is strong enough. Temps already has most of the parts, spread
across three systems that do not talk to each other.

| Part | Where | What it does today |
|---|---|---|
| Error trigger | `crates/temps-error-tracking/src/plugin.rs` | New issues and regressions enqueue `Job::AutopilotTrigger` with the error group id |
| Autopilot config | `crates/temps-core/src/repo_config.rs` (`AgentYamlConfig`) | `on.error.new_issue`, daily budget, cooldown, branch prefix, `deliverable: pull_request` |
| Autofixer | `crates/temps-agents/src/services/autofixer.rs` | Analyze, fix and PR as `agent_runs` rows, in a private sandbox keyed by run id (`SandboxRegistry`), on its own screen |
| Workspaces | `crates/temps-ai-chat/src/workspace_chats.rs` | `ai_applications` holding many chats, linked projects and a blueprint (ADR-053) |
| Chat sandbox | `crates/temps-ai-chat/src/sandbox_link.rs` | One sandbox linked to a chat, re-checked on every tool call |
| Goal watcher | `crates/temps-ai-chat/src/goals.rs` | Starts bounded fix turns in a chat with nobody watching, as the goal's owner |
| PR tools | assistant tools `commit_repository_files`, `open_repository_pull_request` | Commit and open a PR from a chat |
| Approvals | `crates/temps-ai-chat/src/pending_actions.rs` | Inline human approval of a prepared write |
| Operations tray | `crates/temps-projects/src/services/operations.rs` | Lists `agent_runs` autofix runs next to deployments and backups |

The Autofixer works, but:

1. **It lives on its own screen.** The engineer cannot continue the
   conversation in the place they already talk to the assistant, and the fix
   never sees the workspace's blueprint or linked services.
2. **Nothing it reports is checked.** "The test passes now" is the model's own
   claim. There is no basis for merging without a human.
3. **It only understands Git projects.** A project deployed from an uploaded
   archive, a Docker image or static files gets nothing, and the button does
   not say why.
4. **What it did is hard to see.** The run stores the model's output, not a
   record of the commands that ran, the files that changed or what was refused.

## Decision

### 1. An incident is the unit, and it runs as a chat in a workspace

A crash opens an **incident**: one row per error group being worked on. The
incident runs as a new chat in the project's workspace, so the engineer
reviews and steers it where they already talk to the assistant, and the agent
sees the accepted blueprint.

- The workspace is the `ai_application` linked to the project through
  `ai_application_projects` (primary first). A project with none gets an
  "Incidents · *project*" workspace created on the first incident.
- At most one open incident per error group, enforced by a partial unique
  index. A regression reopens the existing incident instead of opening another.
- The incident chat runs as the **owner**: the user who turned autofix on for
  the project. Their permissions are re-checked on every turn, as
  `GoalWatcher` does with `authorization_for_goal`. If the owner loses access,
  the incident pauses as "needs you".

### 2. Each incident gets its own sandbox, built from the crashing deployment

The sandbox is built from the **deployment the crash came from**, not forked
from the workspace's sandbox.

- The workspace sandbox is usually on another branch, holds uncommitted work
  and has different packages installed. A test that goes red to green there
  proves nothing about production. A sandbox built from the deployment's exact
  source does.
- Two incidents, or an incident and the user's own work, never share a
  checkout, ports or processes.
- The prompt carries crash data an attacker can shape (section 7). If the agent
  is subverted, the damage lands in a throwaway sandbox, not in the user's.

The sandbox is created through `CreateSandboxRequest` with the project's
`project_id` and the source resolved in section 3. The image comes from the
project's build preset. Production env vars and secrets are **not** copied in;
services are reached through the sandbox's own namespaces (ADR-051). The
sandbox exists only while the incident is open (section 10).

To keep cold starts short, the server snapshots the sandbox after its first
dependency install (ADR-037), keyed by project and lockfile hash. A later
incident with the same lockfile starts from that snapshot.

### 3. What an incident can do depends on how the crashing deployment was built

Autofix is available on every project, but what it can deliver depends on the
source of the deployment that crashed, read from the deployment row
(`commit_sha`, `source_bundle_id`, image, static bundle), not from
`projects.source_type`. A `manual` project, or one with
`allow_alternate_sources`, can have deployments of several kinds.

| Crashing deployment | Sandbox source | Deliverable | Auto-apply |
|---|---|---|---|
| Git: `commit_sha` set and the project has a repository | Repository at `commit_sha`, cloned through the stored Git connection | PR on `autofix/<group id>` | Auto-merge, when the policy allows it (section 6) |
| Uploaded source: `source_bundle_id` set (includes `temps deploy drop` and workspace sandbox deploys) | The deployment's `source_bundles` archive, extracted | A patched source bundle, a downloadable patch, and a preview deployment of it | Never. Approval deploys the patched bundle |
| Docker image whose labels carry `org.opencontainers.image.source` and `org.opencontainers.image.revision`, pointing at a connected repository | That repository at that revision | PR, as for Git | As for Git |
| Docker image with no usable source labels | None | Root-cause report | Not applicable |
| Static files | None (the bundle is build output) | Root-cause report, using source-map frames | Not applicable |
| External (telemetry only) with a connected repository and a release that maps to a commit | That repository at that commit | PR, as for Git | As for Git, but no post-deploy watch, since Temps does not deploy it |
| External with no repository | None | Root-cause report | Not applicable |

Two rules apply to every row:

- **The feature never disappears.** Every project shows "Fix this crash". When
  only a report is possible, the incident says why ("This deployment was built
  from a Docker image with no source labels") and how to get a full fix: link
  a repository, or add the two OCI labels to the image build.
- **Uploaded-source fixes are never applied automatically.** There is no CI
  and no reviewer history for these projects. There is also a trap: the next
  `temps deploy drop` from the user's own copy silently overwrites the fix. The
  incident therefore delivers the patch as a download alongside the deploy, and
  the approval dialog says the local copy must be patched too.

### 4. Reproduce, fix and verify are goals the server checks

The three phases are goals on the incident chat, so they reuse the watcher's
attempt cap, deadline and "needs you" handling.

Goals today check a read-only API operation. This ADR adds a second check kind,
`sandbox_command`: the server runs a fixed command in the incident sandbox and
records its exit code and an output excerpt. The model may propose the command;
it never reports the result.

| Goal | Check |
|---|---|
| Reproduce | The repro test **fails** at the deployment's source |
| Fix | The same test **passes** at the agent's changes |
| Verify | The project's test suite and build pass at the agent's changes |

A failed Verify sends the agent back to Fix until its attempts or deadline run
out. The incident then ends as `failed`, and the reviewer still gets the root
cause and whatever was learned. Flaky tests are handled by running each side
three times and requiring all three runs to agree.

### 5. Confidence is computed from evidence the server collected

The score is a level from 0 to 5. Each level is a statement about what the
server proved, and a fix reaches a level only when it meets that level's
checks and every level below it. A sum of weighted points would let a passing
test suite make up for a crash that was never reproduced; a ladder cannot, and
"4/5" always means the same thing. The scale matches what reviewers already
read in code-review tools, and a coarse scale does not claim precision the
evidence does not have.

| Level | Means | Checks (server-collected) |
|---|---|---|
| 0 | Nothing verified | The fix does not pass its own reproduction, or nothing changed |
| 1 | The fix passes the agent's reproduction | Same repro passes after the fix, every run |
| 2 | The crash reproduces at the deployed source and the fix makes it pass | Repro fails at the deployed source |
| 3 | The fix is small and changes a file from the stack trace | Diff touches a symbolicated frame; up to 50 lines in under 4 files |
| 4 | The project's test command passes | Suite and build pass; CI green on the PR |
| 5 | Replaying the crashing request is clean | Replay against a sandbox preview no longer errors |

Two things are shown but never move the score: the model's self-assessment
(1-5), which is not evidence, and protected paths, which block auto-merge at
any score.

Every check is stored with its result, the command or source it came from,
and an output excerpt, and the assessment says what the next level needs ("To
reach 4/5: the project's test command passes. No test command is configured"),
so the reviewer sees why the score is what it is and how to raise it. The
levels are a starting point, tuned against reviewer decisions in phase 2. The
default merge threshold is 4: a verified, focused fix with the project's tests
green.

### 6. The project decides what happens to a fix

One row per project in `project_autofix_settings`, changed through the API and
audit-logged like any other project setting (not environment variables). A
separate table rather than columns on `projects`, so the project model and its
many constructors stay as they are; a missing row reads as `off`.

| Column | Values | Default |
|---|---|---|
| `mode` | `off`, `propose`, `auto_merge` | `off` |
| `min_confidence` | Level 0 to 5, used by `auto_merge` only | 4 |
| `protected_paths` | Glob list that blocks auto-merge, such as `migrations/**` | Empty |
| `test_command` | Command the server runs after the fix, besides the agent's reproduction | None |
| `paused` | Pause switch for all autofix work of the project | `false` |
| `owner_user_id` | User automatic incidents act as: whoever last changed the settings | None |
| `application_id` | Workspace the incident chats go to; remembered after the first incident | None |
| `ai_provider`, `ai_model` | Model the incident chats run on | The assistant's default |

- `propose`: the PR (or patched bundle) is offered as a pending action in the
  incident chat, and a notification links to it. The engineer approves,
  rejects, or replies in the chat to have the agent change the fix.
- `auto_merge`: the **policy engine**, not the model, merges as the owner when
  the score meets the threshold, CI is green, the diff is small and no
  protected path is touched. Any gate failing falls back to `propose`. It
  applies only to rows of section 3 that allow it.

After a merge, a goal watches the error group from the deployment that carries
the fix. If the crash comes back, the incident reopens as `recurred` and opens
a revert PR.

### 7. Trust boundaries for chats nobody is watching

Error messages, breadcrumbs and request bodies are attacker-controlled and go
straight into the prompt. The design assumes the model can be talked into
anything, so nothing it says decides a merge or a deploy.

- **`autofix` permission mode.** Only three actions are pre-approved: commits to
  `autofix/*`, opening a PR, and commands in the incident sandbox. Deploys,
  env var changes, deletes and other projects' resources are refused, not
  queued, and every refusal is recorded (section 8).
- **Merge is not a tool.** The model can never call merge or deploy; the policy
  engine or a human does.
- **Credentials.** The sandbox gets a short-lived push token scoped to one
  repository and the `autofix/*` branches. No platform API key, no production
  secrets.
- **Untrusted data is labelled.** The seed wraps crash data as untrusted input,
  separate from instructions.

### 8. Every action is visible as it happens

An append-only timeline per incident, written by the **server** at each action,
including every tool call the autofix mode runs or refuses. Nothing in it comes
from the model's own account of its work.

| Action | Recorded |
|---|---|
| Incident opened, reopened or dropped | Error group, trigger, and why it was queued, deduped or dropped |
| Sandbox built | Source kind, image, repository and SHA or bundle id, node, snapshot reused or cold |
| Command run | Command, exit code, duration, output excerpt (max 4 KB, secrets removed) |
| Files changed | Path, lines added and removed |
| Test run | Test, source revision, pass or fail, attempt |
| Commit pushed, PR opened | Branch, SHA, URL, number |
| Confidence computed | Score and every signal |
| Decision | Auto-merged, approved or rejected, and by whom: a user or "policy" |
| Merge, deploy, watch | Merge SHA, deployment id, recurrence or all clear |
| Refused action | What the agent tried that the mode blocked: a likely sign of prompt injection |
| Cost | Tokens and sandbox minutes per phase |
| Incident closed | Final status; sandbox, preview, token and branch deleted, or the deletion being retried |

It shows in a live timeline panel in the incident chat, on the error group
page, in the operations tray (whose `autofix` entries move from `agent_runs` to
`autofix_incidents`), in a project "Autofix activity" list, in the PR body and
in notifications. Opened, merged, approved, rejected and reverted are also
written to the audit log.

A running incident has a **Stop** button, and a project-level **Pause** halts
all autofix work at once; both are audited.

### 9. Cost on a small host

Sandboxes are the expensive part, so the limit is how many run at once.

- Error ingest does no extra work: it already enqueues one job per new issue or
  regression.
- One incident sandbox at a time by default. Incidents beyond the cap wait as
  `open`; past a queue of 20 they are dropped, counted in a metric, and shown
  in the console.
- The existing per-project daily budget and cooldown apply.
- While an incident waits for review, the idle sweeper stops its sandbox's
  compute (`expiration_sweeper.rs`); a reply in the chat resumes it. The
  sandbox is deleted when the incident closes (section 10).
- Post-merge watching rides the goal watcher's existing indexed query and needs
  no sandbox.

### 10. When an incident closes, only its transcript and decisions remain

An incident closes when its fix is merged or applied, or when it is rejected,
fails or is stopped. The server then deletes everything that did the work and
keeps only the record of it.

| Deleted on close | Kept |
|---|---|
| The sandbox: container, volumes, work directory, and its disk while stopped | The chat transcript: messages and tool calls, with secrets already removed |
| Full command logs | The timeline (section 8), with its output excerpts |
| The fix's preview deployment and URL | Evidence and confidence |
| The scoped push token (revoked) | The decision, who made it and when |
| The `autofix/*` branch, once the PR is merged or closed (the PR keeps its diff) | PR link, merge SHA or applied bundle id, and cost |

- **Deletion is part of closing.** The incident moves to its final status, then
  the server deletes the sandbox and records a `sandbox_deleted` event.
  `sandbox_id` is cleared only after the provider confirms the deletion.
- **A failed deletion is retried, never forgotten.** If the provider or its node
  is unreachable, the incident keeps its `sandbox_id`. A reconcile loop picks up
  closed incidents that still hold a sandbox (a partial index, so the query is
  O(leftovers)) and retries with backoff. The idle sweeper is not a substitute:
  it only stops compute and keeps the disk.
- **Nothing reopens the old sandbox.** If the crash recurs, the incident reopens
  with a new sandbox built from the new crashing deployment.
- **The transcript lives as long as the chat.** It is deleted with the incident
  chat or its workspace.
- **The per-project lockfile snapshot (section 2) is a dependency cache, not
  incident data.** It is taken after installing dependencies and before the
  agent edits anything, so it holds only what the deployment already shipped.
  It is replaced when the lockfile changes and deleted with the project.

### Data model

`autofix_incidents`

| Column | Type | Notes |
|---|---|---|
| `id`, `public_id` | int, text | |
| `project_id` | int, FK | |
| `error_group_id` | int, FK | Partial unique index where status is `open`, `working` or `awaiting_review` |
| `deployment_id` | int, FK | The crashing deployment; decides the source kind |
| `source_kind` | text | `git`, `uploaded_source`, `image_labels`, `report_only` |
| `application_id`, `conversation_id` | bigint, FK | Workspace and incident chat |
| `sandbox_id` | int, FK, nullable | Cleared only after the sandbox is confirmed deleted. Partial index on closed incidents where it is still set, for the reconcile loop |
| `owner_user_id` | int, FK | |
| `status` | text | Open: `open`, `working`, `awaiting_review`. Closed: `merged`, `applied`, `rejected`, `failed`, `stopped`. `recurred` reopens |
| `phase` | text | `reproduce`, `fix`, `verify`, `deliver`, `watch` |
| `base_revision`, `head_revision` | text | SHA, or bundle checksum for uploaded source |
| `branch`, `pr_url`, `pr_number`, `patched_bundle_id` | text, int | The deliverable |
| `confidence` | smallint, nullable | Level 0 to 5 |
| `evidence` | jsonb | Every signal of section 5 |
| `decision`, `decided_by_user_id`, `decided_at` | | `null` decider means "policy" |
| `cost_cents`, `attempts` | int | |
| `created_at`, `updated_at` | timestamptz | |

`autofix_incident_events`: `id`, `incident_id`, `at`, `actor` (agent, server,
policy or user id), `kind`, `summary`, `details` (jsonb), `cost_cents`; indexed
on `(incident_id, at)`.

### API

| Method | Path | Permission | Purpose |
|---|---|---|---|
| GET | `/projects/{project_id}/autofix` | `ProjectsRead` | `configured`, `reason`, `setup_path`, the source kind and delivery of the latest production deployment, and the settings |
| PATCH | `/projects/{project_id}/autofix/settings` | `ProjectsWrite` | Mode, threshold, protected paths, test command, model; sets the owner |
| POST | `/projects/{project_id}/autofix/pause` | `ProjectsWrite` | Pause or resume all autofix work |
| GET | `/projects/{project_id}/autofix/incidents` | `ProjectsRead` | Paginated, newest first, filter by status (`open`, `closed` or exact) and error group |
| POST | `/projects/{project_id}/autofix/incidents` | `ProjectsWrite` | "Fix this crash" for `{error_group_id}`: open the incident or return the open one |
| GET | `/projects/{project_id}/autofix/incidents/{incident_id}` | `ProjectsRead` | Incident, evidence, deliverable |
| GET | `/projects/{project_id}/autofix/incidents/{incident_id}/events` | `ProjectsRead` | Timeline, polled with an `after_id` cursor; the response carries the incident status so a poller knows when to stop |
| GET | `/projects/{project_id}/autofix/incidents/{incident_id}/patch` | `ProjectsRead` | The fix as a unified diff, for uploaded source or a fallback from a PR |
| POST | `/projects/{project_id}/autofix/incidents/{incident_id}/stop` | `ProjectsWrite` | Stop a queued, running or waiting incident |
| POST | `/projects/{project_id}/autofix/incidents/{incident_id}/close` | `ProjectsWrite` | Record the decision on a fix waiting for review: `merged`, `applied` or `rejected` |

The decision is recorded through `close` rather than the chat's pending
actions: the deliverable already exists (the PR or the patch) when review
starts, so there is nothing left for the chat to execute on approval.

## Consequences

- One place to review and steer a fix, with the workspace's context, and no
  separate Autofixer screen to maintain.
- Merging without a human becomes defensible: it rests on observed red-to-green
  evidence, CI and policy, never on the model's word.
- Every project type gets something. Git and labelled images get PRs, uploaded
  source gets a reviewable patch and preview, and the rest get a root-cause
  report that says what would unlock a full fix.
- Cost: one extra sandbox per running incident, model tokens per phase, and two
  new tables. Bounded by the concurrency cap, the queue limit and budgets.
- `agent_runs` keeps its history for past autofix runs; new autofix work no
  longer writes there.
- Mobile and native crashes need toolchains the sandbox images do not have;
  they will end as report-only until such images exist.

## Alternatives considered

- **Use the workspace's shared sandbox.** Cheapest, but incidents collide with
  each other and with the user's work, and red-to-green on a drifted checkout
  proves nothing.
- **A git worktree inside the workspace sandbox.** Solves the checkout clash
  only; processes, ports, installed packages and exposure to untrusted input
  stay shared.
- **Fork the workspace sandbox snapshot.** Starts warm, but inherits whatever
  state the workspace is in. The per-project lockfile snapshot of section 2
  gets the speed without the drift.
- **Keep the Autofixer and add confidence to it.** Leaves the fix on a screen
  the engineer does not otherwise use, and keeps the private sandbox path.
- **Let the model report its own test results.** Simpler, but turns a prompt
  injection into a merge.
- **Gate autofix on `projects.source_type = 'git'`.** Hides the feature from
  every other project and mis-handles `manual` projects whose crashing
  deployment came from Git.

## Plan

**Phase 1: propose.** Gate to phase 2: at least 20 incidents delivered, and
reviewers accepted at least half with few edits.

1. Migrations: `autofix_incidents`, `autofix_incident_events`, `project_autofix_settings`.
2. Incident service: open and dedup from `Job::ErrorGroupOpened`, which error ingest sends for every new or regressed group;
   workspace resolution; budget, cooldown, concurrency cap and queue limit.
3. Source resolution from the crashing deployment (section 3), Git and
   uploaded source first; report-only for the rest.
4. Sandbox build from that source, link to the incident chat, lockfile
   snapshot cache.
5. `autofix` permission mode and the scoped push token.
6. PR delivery for Git; patched bundle, patch download and preview deployment
   for uploaded source; approvals through pending actions.
7. Timeline writes for every action and refusal; audit entries.
8. Close-out: delete the sandbox, preview, token and branch when an incident
   closes, and the reconcile loop for deletions that fail.
8. UI: "Fix this crash" on the error group page, settings card, incident header
   and timeline panel, Stop and Pause, onboarding states, capability endpoint.
   The existing Autofix button opens the incident flow.

**Phase 2: evidence.** Gate to phase 3: on 50 or more reviewed incidents,
deliverables scoring 4/5 or more were accepted unchanged at least 90% of the
time.

1. `sandbox_command` goal check kind; reproduce, fix and verify as goals.
2. Diff analysis: stack-frame overlap, size, protected paths.
3. CI check status from the Git provider (new `temps-git` method).
4. Optional replay against a sandbox preview.
5. Confidence and evidence in the incident header and the PR body.
6. Docker image source labels as a PR source.

**Phase 3: auto-merge and watch.**

1. `merge_pull_request` in `temps-git` for GitHub and GitLab.
2. Policy engine for `auto_merge`.
3. Post-deploy goal watching the error group; `recurred` and revert PR.
4. Retire `SandboxRegistry`, the Autofixer run screen and its handlers.

## Open questions

- Should a project with no workspace get one created automatically (proposed),
  or show an onboarding state until the user links one?
- How long are `source_bundles` archives kept? Uploaded-source autofix needs the
  crashing deployment's bundle to still exist.
- Is a short video explainer of the root cause and fix in scope, or is the
  timeline plus red-to-green output enough for review?
