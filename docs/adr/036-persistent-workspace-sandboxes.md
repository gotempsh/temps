# ADR-036: Persistent Workspace Sandboxes

**Status:** Proposed
**Date:** 2026-08-06
**Author:** David Viejo

## Context

Standalone sandboxes (`crates/temps-sandbox`, the `@vercel/sandbox`-compatible `/v1/sandboxes` API) are built for ephemeral work: create, seed from a repo or tarball, exec some commands, throw away. Every sandbox is created with a bounded `timeout_secs`, clamped to `[60, 86400]` (`services/sandbox_service.rs:212-213`), and a background sweeper stops any running sandbox whose `expires_at` has passed (`services/expiration_sweeper.rs`).

That shape is wrong for a use case we already have the parts for: **a long-lived development workspace on a git repo, with an AI CLI in it, that a user returns to over days or weeks.** The sandbox image already ships `claude`, `codex`, `opencode`, `gh`, `glab`, `bun`, `dtach`, `socat`, and the in-sandbox `temps-pty-agent` (`crates/temps-agents/src/sandbox/docker.rs`, ADR-008). Git cloning — including private repos via a stored provider connection (`git_connection_id`) — is already a create-time source (`services/sandbox_service.rs:38-59`). What's missing is the lifecycle: today the platform will stop that workspace out from under the user, and won't bring it back without an explicit API call.

Four specific gaps:

1. **No lifecycle class above ephemeral.** `timeout_secs` is hard-clamped to 24h. There is no way to express "this sandbox is a workspace; don't treat its idleness as abandonment." The sweeper already has a precedent for a second class — it skips agent-run sandboxes entirely via `agent_run_id IS NULL` (`expiration_sweeper.rs:73-76`) — but nothing equivalent exists for user-owned workspaces.

2. **`timeout_secs` is documented as idle, implemented as absolute.** `expires_at` is set once at create (`sandbox_service.rs:563`) and only moved by `resume_sandbox` and the explicit `extend_timeout` endpoint. `touch()` — called on every exec and filesystem operation — bumps `last_activity_at` and nothing else (`sandbox_service.rs:1395-1417`). The doc comment on the column says "timeout in seconds before the sandbox is considered **idle**"; the behaviour is a wall-clock deadline. A sandbox in continuous active use is stopped at its deadline anyway. This is a bug independent of anything else in this ADR.

3. **A stopped sandbox is a hard error, not a wake.** `resolve_id` — the single choke point every exec and filesystem handler passes through — returns `InvalidState` when `status == "stopped"` (`sandbox_service.rs:1584-1590`). Correct for an ephemeral sandbox whose lifetime the caller controls; hostile for a workspace, where "I came back the next morning" is the normal case.

4. **Nothing ties a sandbox to a project.** Creating a workspace on the repo you're already deploying means copying the clone URL by hand and re-picking the git connection, even though the project row already knows both.

The prize is that fixing lifecycle unlocks a workspace product from parts that already exist. The risk is resource exhaustion: the reference deployment is a Hetzner cpx22 (3 vCPU / 4 GB), and "permanent" read naively — always-running containers that never stop — puts a handful of idle workspaces in a position to evict everything else on the box.

## Decision

Introduce a second sandbox lifecycle class, **`workspace`**, whose *state* is permanent but whose *compute* is on-demand. Concretely: workspaces suspend on idle exactly like ephemeral sandboxes do today, and any access transparently wakes them.

This is deliberately **not** "a sandbox with no TTL". Always-running is the design that kills a small host; suspend-and-wake gives the user the property they actually want (my work is still there, and I don't have to think about it) at near-zero idle cost.

### 1. Suspension is already non-destructive — build on that

The sweeper stops, it does not destroy. Volumes, the bind-mounted `/workspace`, and home-directory state all survive, by explicit design (`expiration_sweeper.rs:8-12`). So data permanence needs no new mechanism. Everything below is about availability and ergonomics.

### 2. `lifecycle` column on `sandboxes`

```
lifecycle VARCHAR NOT NULL DEFAULT 'ephemeral'   -- 'ephemeral' | 'workspace'
```

Modelled as a column rather than a config flag, per the project's no-env-var-configuration rule: it is a per-row property the owner sets at create time and the API/UI can read back.

Behavioural differences, and only these:

| | `ephemeral` | `workspace` |
|---|---|---|
| Sweeper on idle | stop | stop (identical) |
| Access while stopped | `409 InvalidState` | transparent wake, then proceed |
| Auto-destroy | never (today) | never, and this is now a contract |
| Surfaced in UI | Sandboxes list | Sandboxes list, workspace-flagged |

Note what is *not* different: workspaces are still swept, still bounded by the same idle window, still hold the same resource limits. A workspace is not a licence to pin RAM.

### 3. Fix `touch()` to make the idle window real

`touch()` gains `expires_at = now + timeout_secs` alongside its existing `last_activity_at = now`. This makes `timeout_secs` behave as its name and documentation already claim, for every sandbox class.

Deliberately implemented by moving `expires_at` rather than by teaching the sweeper to compute `last_activity_at + timeout_secs`: the partial index the sweeper relies on is `(expires_at) WHERE status = 'running'` (migration `m20260414_000001_create_sandboxes`), and keeping the deadline materialised in the indexed column keeps that sweep query cheap. A predicate over a computed expression would not use the index.

This is a behaviour change for existing ephemeral sandboxes — an actively-used one now survives past its original deadline. That is the documented intent of the field, and the 24h clamp still bounds a fully idle sandbox.

### 4. Wake-on-access at `resolve_id`

`resolve_id` is the single choke point for exec, filesystem, and job operations. For `lifecycle = 'workspace'` rows found in `stopped`, it starts the container, transitions the row to `running`, records a `woken` event, and returns normally instead of erroring.

Ephemeral behaviour is unchanged — a stopped ephemeral sandbox still returns `InvalidState`, which is what `@vercel/sandbox` consumers expect.

Wake latency is a container start, not a create: the image is local, the volumes exist, the work dir is populated. Callers see it as a slow first call, not a failure.

### 5. Create a workspace from a project

`POST /v1/sandboxes` accepts `lifecycle` and an optional `project_id`. When `project_id` is present and no explicit `source` was given, the repo URL and git connection are resolved from the project row, so "give me a workspace on this project's code" needs no copy-pasted clone URL. `project_id` is persisted nullable, with **no foreign key** — deliberately. The column is only ever read as a list filter inside an already `user_id`-scoped query, and at create time behind the project access guard, so a dangling id grants nothing; leaving the FK off keeps project deletion from taking a lock on `sandboxes` and avoids a cascade that would silently rewrite sandbox rows.

Explicit `source` always wins over the project-derived one — the project is a convenience default, not an override.

The project-derived path clones with **the caller's own git connection**. A private repo therefore resolves only for the user who owns the connection the project was set up with — lending one user's token to another is exactly the vulnerability we are not going to build. A teammate with project access but no connection of their own must pass an explicit `source`, or connect their own provider.

### 6. Interactive terminal, and why it must heartbeat

`GET /v1/sandboxes/{id}/terminal` upgrades to a WebSocket bridged to the in-sandbox PTY agent (ADR-008), via a new `SandboxProvider::attach_pty` — a trait method rather than direct Docker access, because `temps-sandbox` must not depend on `bollard` (ADR-010, enforced by `scripts/check-provider-boundary.sh`). `temps sandbox shell <id>` is the client.

The agent keeps a tab alive with zero subscribers, so disconnecting does not kill the program in it. Reattaching to the same tab returns `existed: true` for the same PID with recent scrollback replayed — which is what makes `--cmd claude` viable across a closed laptop.

**Suspension is the exception, and it forces a design constraint.** `/run/temps-pty` is tmpfs and the agent dies with the container, so a suspend destroys every tab. Combined with §3, that creates a trap: activity is otherwise recorded only by exec and filesystem calls, and someone sitting in a terminal talking to an AI CLI makes neither. Without intervention the sweeper would stop the container out from under a live session.

So an attached terminal is itself activity: the handler calls `touch` on connect, every 20s while attached, and once more on detach. The interval is bounded below the 60s minimum `timeout_secs` so no sandbox can be swept between two beats.

### 7. Out of scope for this ADR

- **AI CLI credential injection at create.** The CLIs are installed in the image; the credential catalog and injector (`crates/temps-agents/src/ai_cli/`, `services/sandbox_injector.rs`) are currently wired only to agent runs. Until that is extended, users supply their own key via the existing create-time `env` map.
- **A browser terminal.** The WebSocket protocol is deliberately the same shape `temps-deployments`' container terminal already speaks (binary frames for PTY bytes, JSON text frames for control), so an xterm.js client can be added against the same endpoint without server changes.
- **Terminals on non-Docker backends.** `attach_pty` defaults to an explicit "not supported" error; Firecracker and the local dev provider return it. A sandbox on those backends is still fully usable through `exec`.

## Consequences

**Positive**

- A workspace survives indefinitely without operator intervention, and costs nothing but disk while idle.
- `timeout_secs` stops lying, for every caller.
- Creating a workspace on a project's repo is one call.
- No new background machinery: the existing sweeper, registry recovery, and preview-gateway paths are reused unchanged.

**Negative / risks**

- **Disk is now the unbounded resource.** Suspended workspaces hold their work dir and home volume forever. Nothing in this ADR reclaims them; `destroy` remains the only path, and it is user-initiated. A per-owner workspace count or disk quota is the natural follow-up, and should land before this is offered on Temps Cloud.
- **Wake adds latency on a cold call**, on an endpoint that previously either worked immediately or failed immediately. Clients with tight timeouts on `exec` may see a timeout where they used to see a clean 409.
- **The `touch()` change extends real lifetimes.** An ephemeral sandbox polled by a health check every minute now never expires. That is the intended reading of an idle timeout, but it is a change to observable behaviour and is called out in the changelog.
- **Wake-on-access does not cover preview URLs.** A suspended workspace's preview URL still fails until something calls the API. Waking from the gateway path is a larger change (it crosses the proxy/console split, ADR-017) and is not attempted here.
- **A terminal left attached indefinitely keeps a sandbox alive indefinitely.** That is the intended reading of "attached is active", but it does mean a forgotten terminal pins a container. The disk/quota follow-up above should account for it.

## Alternatives considered

**No TTL at all — workspaces simply never stop.** Simplest to implement and closest to the literal request. Rejected: on a 3 vCPU / 4 GB reference host, a handful of idle workspaces holding memory is a worse outcome than any TTL, and the failure mode (deployments evicted by idle dev containers) is invisible until it is catastrophic. Suspend-and-wake delivers the same user-visible property.

**A very large `timeout_secs` (e.g. 1 year) instead of a lifecycle class.** Rejected: it conflates "how long may this idle before suspending" with "may this be resumed automatically", which are genuinely different questions. It also leaves the stopped-sandbox error path unchanged, so the user still hits a 409 the morning after.

**A separate `workspaces` table and API surface.** Rejected: workspaces and sandboxes share every operation — exec, filesystem, jobs, preview URLs, resize, preview passwords. A parallel surface would duplicate `handlers/sandboxes.rs` almost entirely to express one lifecycle difference. A column is proportionate.

**Sweeper computes `last_activity_at + timeout_secs`.** Rejected on indexing grounds — see §3.

## References

- ADR-008 — in-sandbox PTY agent (the terminal follow-up builds on this)
- ADR-009 — sandbox API versioning
- ADR-010 — provider boundary traits (why the terminal needs a trait method)
- ADR-013 — sandbox egress credential proxy (threat model for long-lived sandboxes)
- ADR-029 — Firecracker sandbox backend (workspaces inherit backend selection unchanged)
