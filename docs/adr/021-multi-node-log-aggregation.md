---
title: "ADR-021: Multi-node container log aggregation and shipping"
status: Accepted — history (pull-first) implemented; agent-push deferred
date: 2026-06-28
author: David Viejo
---

# ADR-021: Multi-node container log aggregation and shipping

> **Implementation status (v1, shipped):** searchable **history** now covers
> remote-node containers, implemented **pull-first** rather than agent-push (see
> "Phase B" below for why). A CP-side `RemoteLogCollectorService`
> (`temps-log-aggregator`) reconciles the set of running remote containers and
> opens the agent's existing mTLS `/logs/stream` for each, feeding lines into the
> **same** `ChunkWriterService` as local logs — so `/api/logs/search` is
> node-complete. `log_chunks`/`LogLine` gained `node_id`/`node_name`; search
> gained `container_ids`/`node_ids` filters and per-line `container_id`/`node_name`
> in results; the history UI gained Container + Node pickers ("show all" =
> default) and a per-line source column. The agent-push shipper + CP-pressure
> broadcast remain the scale-out evolution (deferred). Phase A (on-demand live
> "show all") is not yet built — the combined history view covers the immediate
> "see everything across replicas/nodes" need.

## Context

Container logs are node-local. Today the per-container **live** view already
routes by `deployment_containers.node_id` — local containers via the control
plane's Docker, remote containers via the agent's `/agent/containers/{id}/logs`
over mTLS (ADR-020) — and the teardown capture (`capture_container_logs`) dumps a
bounded snapshot to `deployment_container_logs` for both. Two gaps remain:

1. **Searchable history (`/api/logs/search`) under-represents remote nodes.** It
   reads control-plane-side JSONL (`structured_logs`/`file_logs`); a *running*
   remote container's stdout/stderr lives on the worker and is only fetched live
   or dumped at teardown, so it isn't continuously in the central store.
2. **No "show all" view.** The viewer streams one container at a time; there is
   no combined-across-replicas stream and no way to fan out across nodes.

The hard constraint is operational: a high-traffic app can emit tens of
thousands of log lines per second. The pipeline must never degrade the workload,
the worker node, or — above all — request **routing**. Logging is best-effort and
lossy-tolerant; under pressure we shed logs, never traffic.

## Decision

Two phases, both built on a single invariant: **logging never blocks the
workload or competes with the data plane; every hop is bounded and drops rather
than backs up.**

### Phase A — on-demand aggregate ("show all"), pull-based

A console endpoint `GET …/environments/{id}/logs` fans out to every container in
the existing node-aware list (local Docker + remote agents over mTLS),
interleaves by timestamp, and tags each line with `container_name` + `node_name`.
Cost is zero unless a human is watching. Bounds:

- **Tail-only by default** (last N + live), never a full replay.
- **Per-container ring buffer + WebSocket backpressure** — drop oldest toward a
  slow client; never buffer unbounded on the control plane.
- **Aggregate rate cap** — above X lines/s, sample 1-in-K and surface "sampled".
- **Max fan-out** — at most M containers in "all" mode; beyond that, require a
  filter (a single container is always unbounded-selectable).
- **Idle auto-disconnect.**

### Phase B — durable, searchable remote history

**Shipped as CP-side pull (v1).** The control plane is already the sink for the
entire *local* Docker log firehose, so the lowest-risk way to make remote logs
searchable is to extend that same model: a `RemoteLogCollectorService` reconciles
running remote containers and opens the agent's proven `/logs/stream` for each,
parsing and tagging lines (`node_id`/`node_name`) and writing them into the
**same** `ChunkWriterService` + `log_chunks` pipeline as local logs. Intake stays
CP-controlled (it can bound its own concurrency), reuses the existing chunk
buffering/flush caps, and adds no new ingest surface. Per-container streams back
off exponentially and give up after a bounded number of failures, identical to
the local collector.

**Agent-push (deferred, the scale-out form).** When the CP-as-sink saturates, the
target design is each worker agent tailing its containers and shipping to a
dedicated control-plane ingest endpoint, tagged with `node_id`/`container`/
`service`, with backpressure + drop-and-count at every hop:

- **Read rotated Docker json-file logs, never the live stdout pipe** — so a
  stalled downstream can never block the container's `write()` (the
  hang-the-app failure mode). Docker rotation (`ContainerLogConfig`) bounds
  on-disk size on the worker.
- **Worker:** non-blocking tail → bounded ring → batch (≈1 s / 64 KB) + gzip →
  ship over mTLS; a small **capped on-disk spool** for CP-unreachable windows
  (drop-oldest when full — never fill the worker disk). Per-container/per-node
  rate + byte caps; over-cap → sample or drop with counters. Exponential backoff
  + circuit breaker on CP 429/5xx.
- **Control plane:** bounded ingest queue → fixed worker pool; **reject (429)
  when full** so the *agent* sheds, not the CP. Writes to a time-partitioned
  store with **retention + rotation**; cold logs tier to object store (R2/S3).

### Isolation from routing

Per ADR-017 the proxy (Pingora) is a **separate process** from the console. Log
ingest and the log API live on the **console** side; the agent ships to a
dedicated mTLS ingest path — **not** through the public proxy and **not** through
the routes it serves. A log storm can saturate the console's bounded ingest pool
and Pingora keeps routing. On the worker, shipping is userspace in the agent and
never touches the kernel overlay path (vxlan/nftables), so cross-node request
routing is unaffected.

### Load-shedding priority (explicit)

When a node or the CP is under pressure, shed in this order — serving traffic is
never the thing that gives way:

1. Route user requests (proxy) — **never dropped.**
2. App → Docker stdout — never blocked (we read files, not the pipe).
3. Ship logs to the CP — **first to be sampled/dropped.**
4. Search/index ingest on the CP — dropped before it can slow queries or Pingora.

Shedding is driven by an explicit CP-pressure signal — not by each worker
guessing in isolation (see below).

### Backpressure signalling — how the CP knows, and how workers react

Two layers, and the event is the upper one:

- **Hard guarantee (always on, no event):** the CP ingest is a bounded queue;
  full → reject (429) / drop at the boundary. This is what actually protects
  routing — it holds even if the event system is broken, the worker never got the
  message, or the CP just died. Routing safety must never depend on a message
  being delivered.
- **Proactive signal (the event):** the CP detects pressure *early* and tells
  workers to back off **before** they hit the 429 wall, turning a lossy cliff
  (workers blasting bytes that only get rejected) into a coordinated, graceful
  degrade. The event makes the system efficient and smooth; the bounded queue
  makes it safe.

**How the CP decides it is "under pressure"** — trigger on the resource being
protected, never on log volume. Pressure is asserted when *any* of:

- **Data-plane health** — proxy in-flight count / accept-queue backlog / p99
  routing latency over watermark (the most honest signal: if routing is slowing,
  that *is* pressure).
- **Ingest queue high-watermark** — the bounded ingest channel's fill ratio
  (self-calibrating; no magic threshold).
- **Host headroom** — CPU %, load average, memory, and especially **disk free**
  (the slow-motion killer for a log store). The CP-as-node already samples these
  via the resource-alert sampler (`node_cpu_alert_percent`, disk-space alerts).

**The event, designed so it cannot misbehave:**

- **A level, not a boolean** — broadcast a target: `normal → sample(1-in-N) →
  errors-only → pause`. Never go fully dark: a stressed CP is exactly when logs
  matter, so the first steps are severity-aware (keep ERROR/WARN, shed
  INFO/DEBUG) and rate-capping; full pause is the extreme only.
- **Hysteresis + dwell** — enter at a high watermark, clear only at a lower one,
  hold a minimum dwell before clearing. Without this the state flaps and every
  worker oscillates in lockstep, re-overloading the CP each cycle.
- **Level-triggered + edge-triggered** — send an immediate event on transition
  (fast reaction) *and* carry the current level in every heartbeat, so a dropped
  transition event self-heals on the next tick. The pressure channel runs at a
  1–5 s cadence, faster than the ~60 s health loop.
- **Soft-state lease → fail-open** — the signal is "shed at level N for the next
  30 s," re-asserted each tick. A worker that stops hearing the CP fails *open*
  to normal — safe only because the hard 429 still catches any excess.
- **Recovery without a stampede** (the dangerous half) — on clear, workers ramp
  back with **jittered AIMD** (multiplicative decrease on assert, additive
  increase on release) so the CP sees a slope not a step; any bounded local spool
  is **backfilled rate-limited and jittered** so post-incident catch-up doesn't
  re-overload the CP.

**Transport** — reuse the agent's mTLS control connection (`cp_ws_client_config`)
and heartbeat as the carrier; the `Job::ForceRouteReload` + PG NOTIFY broadcast
is the precedent. The pressure message must be tiny and prioritised so it never
queues behind the ingest backlog it is trying to relieve.

**Fairness (later)** — a global level sheds every worker equally, penalising a
quiet worker for a chatty neighbour. A v2 can combine the global level with
per-worker rate quotas so the top-talker sheds most; global is acceptable for v1
because CP CPU/disk is genuinely a shared resource.

## Consequences

- **Positive:** remote containers become first-class in both live ("show all")
  and searchable history; logs survive node/agent restarts; the data plane and
  the workload are structurally insulated from log volume.
- **Negative / accepted:** logs are **lossy under sustained overload** (by
  design — completeness yields before the app or routing). The pipeline must
  expose its own health (`dropped_lines`, ship lag, ingest-queue depth) and the
  viewer must show a "sampled/dropped (high volume)" banner so operators see
  shedding rather than silently trusting a complete history.
- New surface to operate: a continuous agent-side shipper, a central ingest path
  with retention, and a **CP-pressure detector + control-channel broadcast** (the
  shed-level signal), each needing its own resource bounds. The broadcast is
  advisory: it can be wrong or absent without compromising routing, because the
  bounded-queue 429 is the actual guarantee.

## Alternatives considered

- **Pull-only (no shipper):** aggregate on demand from agents every time. Simple
  and zero-baseline, but no durable searchable history and re-reads the firehose
  per viewer. Adopted as Phase A; insufficient alone — hence Phase B.
- **Ship through the public proxy / same process:** rejected — couples log
  volume to request routing, violating the core invariant (ADR-017).
- **Unbounded buffering / block-on-full for completeness:** rejected — the
  hang-the-app and OOM-the-node failure modes are worse than dropping lines.
- **Per-connection 429 backpressure only (no proactive signal):** kept as the
  safety floor, but insufficient alone — workers ship bytes only to have them
  rejected, and loss happens abruptly at the cliff. The CP-pressure broadcast is
  layered on top to shed early and gracefully.
- **Each worker self-detects CP pressure:** rejected — no worker can observe
  CP-global state (proxy latency, total ingest-queue depth, CP disk); independent
  guesses oscillate and disagree. Pressure detection is CP-authoritative and
  broadcast.
