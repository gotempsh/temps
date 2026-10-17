# ADR-052: Pluggable Health Probes for Deployments Without an HTTP Port

**Status:** Proposed
**Date:** 2026-10-17
**Author:** David Viejo
**Builds on:** ADR-045 (host Docker socket grant: first non-web workload class), the container health loop in `temps-monitoring`

---

## Context

### Every deployment is assumed to be a web server

A deployment is only considered ready once it answers HTTP. `DeployImageJob`
(`crates/temps-deployments/src/jobs/deploy_image.rs`) has two phases:

1. **Phase 1**: wait for the container to be `Running`, failing fast on
   `Exited`/`Dead` and on restart loops.
2. **Phase 2**: poll `GET {container}:{port}{health_path}` until it returns an
   acceptable status, bounded by `health_check_timeout_secs` (default 300,
   exposed as "Startup timeout" in project settings).

Traffic is switched, and the previous deployment torn down, only after
Phase 2 passes. That gate is what lets a bad build fail without replacing a
working one.

A process that never opens a port (a queue consumer, a pub/sub subscriber, a
chat bot, a scheduler daemon) can never pass Phase 2. It runs for the full
timeout and the deployment is marked failed, although the container is
healthy. A reported request asked for exactly this case. The only workaround
today is to bolt a dummy HTTP listener onto the worker so the probe has
something to hit.

### The obvious fix removes the safety net

The request proposed disabling the startup timeout and marking the deployment
done once the container runs. The code already half-supports this:
`effective_health_path == None` skips Phase 2 (`deploy_image.rs`, "Phase 2"
comment), and it is used by rollbacks. But nothing user-facing can set it.

Exposing it as-is is a regression in safety:

- **Broken deploys replace working ones.** A worker that starts, then crashes
  on its first Redis call, passes "container is running" at the instant it is
  checked. The previous good deployment is removed.
- **No health signal exists afterwards.** The only runtime health data is what
  `container_health.rs` derives from Docker (restarts, OOM, exits, resource
  use). HTTP readiness is never re-evaluated after deploy, so a no-check mode
  does not lose a runtime signal today. But it permanently forecloses one:
  any liveness-driven remediation added later (restart a wedged process, roll
  back on sustained failure) would work for HTTP apps only, and workers would
  be a second-class citizen forever.
- **It is unobservable.** "Skipped" is not a state the UI can render
  meaningfully; the operator cannot tell a healthy worker from one that is
  silently broken.

### What already works without a port

The container health loop (`crates/temps-monitoring/src/container_health.rs`)
iterates every non-deleted `deployment_containers` row and uses Docker
inspect and stats, not HTTP. Restart-count and crash-loop alarms, OOM and
unexpected-exit alarms, CPU/memory thresholds and the container metrics all
work for a portless container. Restart policy is applied by Docker itself
(`crates/temps-deployer/src/docker.rs`, `map_restart_policy`).

What does assume a port:

- `deployment_containers.container_port` is a non-nullable `i32`
  (`crates/temps-entities/src/deployment_containers.rs`).
- `persist_runtime_info` in `container_health.rs` looks up the published TCP
  binding matching `container_port` on every poll, clears `host_port` when it
  is absent, and notifies the route table. A portless container is
  permanently in that "absent" state.
- Proxy metrics, HTTP uptime checks and the status page have no traffic or URL
  to measure.

### Constraint: no exec primitive on `ContainerDeployer`

`ContainerDeployer` (`crates/temps-deployer/src/lib.rs`) has no
run-a-command-in-a-container method, locally or for worker nodes. Any probe
that runs inside the container needs one.

---

## Decision

Replace "is the HTTP endpoint answering" with a **health probe**, of which HTTP
is one kind. Introduce an explicit, observable **health state** that does not
care which probe produced it. Workers choose a probe that fits them; they never
opt out of health.

### 1. Probe kinds

Configured under `health:` in `.temps.yaml`, and as a project/environment
setting with the same shape. `http` remains the default, so existing
projects are unchanged.

| `type` | Meaning | Needs a port | Needs app cooperation |
|---|---|---|---|
| `http` (default) | `GET path`, existing semantics and precedence | yes | yes |
| `tcp` | Connect to a port | yes | no |
| `process` | Container stays `Running` with no restart and no exit for a stability window | no | no |
| `exec` | Run `command` inside the container; exit code 0 is healthy | no | yes (a command) |

```yaml
health:
  type: process        # default for a deployment that declares no port
  stable_for: 15       # seconds the container must stay up (process only)

# or
health:
  type: exec
  command: ["redis-cli", "-h", "redis", "ping"]
  timeout: 5
```

`process` is the default when no port is declared, so a worker with no
configuration gets a real gate instead of nothing. A heartbeat probe (worker
pings Temps; a missed beat means unhealthy) is the natural fourth kind, but it
needs a token and an endpoint and is out of scope for this ADR.

### 2. Health state is first-class

Add `health_state` to `deployment_containers`: `healthy`, `unhealthy`,
`unknown`, plus `health_checked_at` and a short `health_detail` (the failing
probe output or reason). Operators, the UI, alerts and any future remediation
read this value; none of them read probe-specific data.

`unknown` is a real state, shown as such: a probe that has not run yet, or a
kind that cannot be evaluated. It is never rendered as healthy.

### 3. Deploy gate semantics

- Phase 1 (container running, fail fast on exit and restart loops) is
  unchanged and applies to every probe kind.
- Phase 2 becomes "run the configured probe until `healthy` or the timeout".
  - `http`/`tcp`: existing behaviour.
  - `process`: pass after the container has been `Running` with an unchanged
    `RestartCount` for `stable_for` seconds (default 15). Failure is reported
    with the existing exit and restart-loop messages and log excerpt.
  - `exec`: run the command at an interval until it exits 0.
- **Unknown fails the gate.** A mistyped exec command or an unsupported kind
  fails the deploy with an actionable message, never passes it.
- The startup timeout continues to bound the gate for every kind. It is not
  made optional.

### 4. Runtime probing

The existing container health loop evaluates the probe after deploy and
writes `health_state`. It already walks every container and resolves worker
runtimes, so this adds no loop and no new per-container task. Probing is
interval-driven (not per request), with a bounded per-cycle concurrency, the
way stats collection works today.

Phase one only records state and alarms on sustained `unhealthy`. No
automatic restart or rollback is added by this ADR; the point is that the
state exists and is uniform, so remediation can be built once for all
workloads.

### 5. Portless containers are an explicit state

Make `container_port` nullable (or add an explicit `exposes_http` flag) rather
than storing a sentinel. When a container has no port:

- `persist_runtime_info` skips binding lookup and route notification entirely.
- No route-table entry is created for the deployment, and domains,
  environment subdomains and the screenshot job are skipped.
- The UI shows "background worker" and replaces uptime/URL surfaces with the
  health state and container metrics.

### 6. Exec primitive on `ContainerDeployer`

Add `exec_in_container(container_id, command, timeout) -> ExecResult` to the
trait, implemented for the local Docker deployer (Bollard exec create/start/
inspect) and for the remote worker deployer. The call is time-bounded and its
captured output is truncated to a fixed size before it is stored or logged.
The same constraint that applies to the Docker socket grant applies here: the
command comes from project configuration the user already controls and runs
only inside that project's own container, with no new host access.

### 7. Discoverability

Per the feature-discoverability rule, the probe choice is a visible control in
project settings, next to the existing "Startup timeout" field, with a
concrete example for each kind. The deploy-failure message for a container
that never opened a port points to the probe setting ("this container did not
open port N; if it is a background worker, set `health.type: process`").
That message is the onboarding path for the exact case that motivated this
ADR.

---

## Consequences

### Positive

- Background workers deploy without a dummy HTTP server, and a broken worker
  still fails its deploy instead of replacing a good one.
- Health is one concept across HTTP apps and workers, so future self-healing
  (restart on N failed probes, rollback, alerting) is written once.
- `exec` gives any workload a way to report real health, including ones that
  are not servers at all.
- Existing projects are unaffected: `http` is the default and precedence rules
  are untouched.

### Negative / costs

- Touches deploy, monitoring, entities and the UI, so it is a multi-step
  change, not a flag.
- `process` is a weak signal: it catches crashes and restart loops, not a
  hung process. Operators who need more must supply an `exec` probe.
- New exec primitive on the deployer trait, for local and worker nodes, is
  new attack surface and needs the bounds in section 6.
- Schema migration: nullable `container_port` (or a flag) and three health
  columns.

### Scalability

Runtime probing is control-plane work on the existing health loop, not hot
path. Memory is bounded by one small state row per container; exec output is
truncated; per-cycle concurrency is capped so a fleet of workers cannot
create unbounded simultaneous execs. At saturation the loop falls behind and
`health_checked_at` ages, which the UI surfaces as `unknown` after a staleness
bound rather than showing a stale `healthy`.

---

## Alternatives considered

1. **Allow disabling the startup timeout / health check.** Smallest change,
   and what was requested. Rejected: it deletes the deploy gate and the
   future remediation hook, and "skipped" cannot be observed. It stays
   available internally for rollbacks.
2. **Require every worker to expose a port.** The status quo. Rejected: it
   pushes a dummy listener into every worker, which also hides real health.
3. **Heartbeat only.** Strongest signal for workers, but requires SDK/token
   plumbing and an unconfigured worker would have no gate at all. Deferred as
   an additional probe kind.
4. **Rely on Docker `HEALTHCHECK`.** Free for images that define one, but
   many do not, the Dockerfile is not always the user's, and autopack presets
   can set `HEALTHCHECK NONE`. It can be honoured as an input to `exec` later;
   it is not a sufficient mechanism on its own.

---

## Rollout

1. **Phase 1**: `process` and `exec` at deploy time; `health_state` recorded;
   portless container state; route and monitoring paths skip portless
   containers; settings UI and failure message.
2. **Phase 2**: runtime probing on the container health loop; alarm on
   sustained `unhealthy`.
3. **Phase 3** (separate ADR): remediation actions, and the heartbeat probe.

## Open questions

- Nullable `container_port` versus an explicit `exposes_http` flag. A flag is
  a smaller migration; nullable is more honest. Lean nullable.
- Whether `stable_for` should default higher for slow-booting workers, or be
  derived from the startup timeout.
- Compose deployments (`deploy_compose.rs`) have per-service healthchecks
  already; whether they map onto this model in phase 1 or are left alone.
