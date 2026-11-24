<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-113: Coordinating identical builds

**Status:** Accepted — 2026-11-23

## Context

Several projects, or several environments of one project, often build the
same image: same repository, same commit, same build context and Dockerfile,
same build-time values. A push then triggers one full build per project, all at
once. On the reference small server (3 vCPU, 4 GB) three concurrent `next
build`s are enough to swap or be OOM-killed, and every build writes its own
layers to disk. Other self-hosted platforms queue these: the first build
runs, the others wait and reuse its image.

Temps already has what reusing an image needs: a build job records where its
image lives (`image_tag`, `image_id`, `builder_node_id` and the per-platform
maps), and the deploy job hands an image to the node that runs it from
wherever it lives (`deploy_image.rs`, the same path rollbacks and promotions
use). Retention already keeps an image that another project's deployment
names (`image_references.rs`).

## Decision

### D1. A per-project setting, `parallel` by default

Two columns on `projects`, not environment variables (AGENTS.md
"Configuration is a column"):

| Column | Values | Default |
|---|---|---|
| `identical_build_mode` | `parallel`, `queue`, `share` | `parallel` |
| `identical_build_scope` | `source`, `fingerprint` | `source` |

- `parallel` is what every build did before. It is the default for existing
  and new projects, so nothing changes on upgrade.
- `queue`: builds of a group run one at a time, oldest first; each builds its
  own image.
- `share`: as `queue`, and a build whose full fingerprint matches a build that
  succeeded in another `share` project with the same access boundary (D4)
  deploys that image instead of building.
- The scope decides the group a build queues in: the repository and build
  path whatever the commit or values (`source`, the default), or only the full
  fingerprint (`fingerprint`). The default is `source` because builds of one
  repository and path almost always differ in some build-time value (D2), so a
  fingerprint group would rarely queue anything and a coordinating project
  would still run them side by side. **The scope only ever widens queueing.
  Reusing an image always requires the full fingerprint**, so under `source` a
  `share` build waits for the build of its source and then reuses its image
  only if the fingerprints match, building its own otherwise.

`GET/PUT /projects/{id}/identical-builds` reads and changes it (`projects:read`
/ `projects:write` on the project, audited as
`project.identical_builds.updated` with the old and new value), with a plain
explanation and an example per mode and the project's activity over the last
30 days (reused, built, waiting, building). The console shows it under
Project settings → Build, the CLI as `temps projects identical-builds`.

### D2. The fingerprint

A keyed hash (HMAC-SHA256 under a subkey of the install's encryption key) of a
length-prefixed encoding of:

- the repository: provider connection, git URL, owner, name, and whether only
  the project directory is checked out;
- the commit actually checked out (a full SHA; a build without one is not
  coordinated);
- the build context and Dockerfile path, normalised;
- the Dockerfile: the repository's own (its bytes), or "generated from the
  preset";
- the preset and its configuration; both `.temps.yaml` files a build reads;
- the target platforms (the builder's native platform when none is set); who
  builds (control plane, worker, CI), because a worker receives only public
  values; the registry mirror prefix; `cache_from`;
- the name and value of every build-time variable the build can read.

"Can read" errs towards *not* matching, because a wrong match deploys one
project's build-time values (an API key, a public DSN) inside another
project's image:

- a repository Dockerfile reads the `ARG`s it declares in any stage and the
  secrets its `RUN --mount=type=secret` steps name; Docker ignores the rest;
- a Dockerfile that cannot be parsed (`ARG $NAME`) counts every value;
- a Dockerfile generated from a preset declares every variable, so every
  value counts;
- Docker's predefined proxy arguments and `NPM_TOKEN`/`NPM_RC` (which become a
  generated `.npmrc` in the context) always count;
- `BUILDKIT_*` arguments are BuildKit settings, and count only when they can
  change the image: `BUILDKIT_SYNTAX` always (it picks the frontend that
  parses the Dockerfile), any other one when the repository Dockerfile
  declares it with `ARG`, and all of them when the Dockerfile cannot be
  analysed. The cache-mount namespace is one per project and environment, so
  counting it when nothing reads it would make every cross-project match
  impossible; an unanalysable Dockerfile therefore never shares across
  projects, which errs toward building.

Values never leave the planner's sealed envelope in clear: only the keyed
hash is stored, so a stored fingerprint cannot be brute-forced back into a
low-entropy secret without the install's key.

A consequence worth stating: preset builds declare every project variable,
including Temps' own per-project values (`SENTRY_PROJECT`, the deployment
token), so across projects they practically never share an image. That is the
correct answer for them, since those values can reach the build. The
default scope `source` is what serialises them; `share` pays off for repository
Dockerfiles that declare only the values they need.

### D3. Durable coordination: `identical_builds`

One row per coordinated build (`deployment_id`, `job_id` unique), with the
group (`queue_key`, the scope-prefixed fingerprint or source key), the
fingerprint, the access boundary, a state (`waiting`, `building`,
`succeeded`, `failed`, `cancelled`, `reused`, `timed_out`, `abandoned`), a
heartbeat, and for a succeeded share build the image it produced.

- **Exactly one builder per group**: a partial unique index on `queue_key`
  where `state = 'building'`. Claiming is one `UPDATE … SET state =
  'building'` guarded by "nothing builds in the group and no older live build
  waits"; two processes racing for the same group cannot both win, the loser
  hits the index and keeps waiting. No advisory lock is held for the length of
  a build and no pool connection is pinned.
- **Reuse is checked again after the claim**: a share build that wins its
  group looks for a reusable image once more before building, because the
  build it waited for can succeed between its last check and its claim. If
  one is found it records the reuse, which releases the group.
- **Line order**: oldest waiting row first; a waiting row whose heartbeat is
  stale (its process died) loses its place.
- **Waiting holds nothing**: the wait happens in the build job before the
  build is offered to a worker, so it holds no worker build slot
  (`build_admission` admits a build when it is sent) and no connection
  between polls.
- **Restarts**: a retried build job takes its own row over (`ON CONFLICT
  (deployment_id, job_id)`), so it never waits for itself.

### D4. Who may reuse whose image

An image is reused only when, at the same time:

1. the reusing project and the building project are both in `share` mode
   (opt-in on both sides);
2. the full fingerprint matches (D2), whatever the scope;
3. the access boundary matches: the sorted set of teams granted the project
   (`project_team_access`), or "unrestricted" for both. Whoever can see the
   reusing deployment can therefore already see the build it reused, which is
   why its log may name it ("Reused image from deployment #N, built from
   owner/repo@sha");
4. the build succeeded within the reuse window (30 minutes).

A build in a different access boundary can still share a queue (scope
`source`); its log then says "a build of another project" and never names it.

### D5. Failure handling

| Event | What happens |
|---|---|
| The builder's build fails | Its row becomes `failed`; the next waiting build builds itself (take-over), and its log says the awaited build failed. |
| The builder's deployment is cancelled (for instance its project got a newer commit) | The build future is dropped; the lease's `Drop` marks the row `cancelled`, and the next waiting build takes over. Followers are never stranded. |
| The builder's process crashes or restarts | Its heartbeat (every 10 s) stops; after 60 s the row is marked `abandoned` and a waiting build takes over. |
| A waiting deployment is cancelled | Its wait future is dropped; a guard marks the row `cancelled`, so it no longer holds its place. |
| A build waits longer than 90 minutes in total | It fails with a reason that says to redeploy or switch the project to `parallel`; its row is `timed_out`. |
| The coordination table cannot be reached | The build degrades to building on its own and says so in its log. |
| The reused image is on a worker | The reusing deployment records the same `builder_node_id` and per-platform maps, and the deploy job moves the image to its nodes through the existing image distribution path. |

### D6. Visible surface

- Project settings → Build → "Identical builds": the mode as three cards with
  an explanation and an example ("3 projects building the same repository and
  path at the same commit build once"), the scope, and the last 30 days.
- The build log: "Identical builds (share, grouped by fingerprint): checking
  for an identical build of owner/repo@sha (N build-time values in the
  fingerprint)", "⏳ Waiting for an identical build (deployment #N)…", "The
  build this deployment was waiting for (deployment #N) failed; this
  deployment builds the image itself", "♻️ Reused image from deployment #N,
  built from owner/repo@sha…". The build job also records the
  `reused_image_from_deployment_id` output.
- `temps projects identical-builds [get|set]` in the CLI.

## Load, memory and saturation

- **Expected load**: control plane only, per build, never per request. A
  waiting build runs about three indexed queries every 3 seconds; a building
  build one indexed update every 10 seconds. Fifty builds waiting at once is
  about 50 queries per second against a table of at most a few thousand rows.
- **Memory bound**: nothing per group in process memory. Each waiting build is
  one sleeping task and a few small strings; a building build adds one
  heartbeat task. The table holds one small row per coordinated build (no
  build inputs) and is pruned seven days after a row finishes.
- **At saturation**: builds of a group wait in line instead of running at
  once, which is the point: CPU, RAM and disk stay at one build per group.
  The wait is bounded (90 minutes); past it the deployment fails with a clear
  reason instead of waiting forever. If the database is unreachable, builds
  degrade to `parallel` behaviour.

## Consequences

- The default changes nothing. Operators opt projects in one at a time.
- A reused image keeps the labels of the build that produced it
  (`sh.temps.image.project`); retention and the orphan sweep already keep an
  image another project's deployment names.
- Source maps uploaded by a framework plugin during the build are uploaded
  once, by the builder. Builds that upload per project already differ in
  `SENTRY_PROJECT`, so they never share.
- Not covered: sharing images between installs, and reusing an image produced
  by a build that was not coordinated (`parallel` projects).
