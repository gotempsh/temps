<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-115: A workload capability is written once; each kind keeps its own entities

**Status:** Accepted — 2026-11-25

**Amends:** [ADR-052](./052-common-workload-model.md)

Related: [ADR-055](./055-workload-plane-and-runtime-drivers.md) (§9, the
per-node egress proxy). The standing rule and the recipe are in
[`docs/architecture/workload-capabilities.md`](../architecture/workload-capabilities.md).

## Context

Recording a workload's outbound connections was first built for sandboxes:
an opt-in column on the sandbox row, an ingest that attributed records to
sandboxes, an API under `/v1/sandboxes/...`, a console card on the sandbox
page and a `sandbox_id` column on the stored records. The engine under it
(the per-node egress proxy, its policy table, its recording channel) already
identified a caller by `WorkloadRef { kind, id }` and knew nothing about
sandboxes.

The same capability is wanted for a project's environments: which
destinations does a deployed service call? Built the same way, it would have
been a second opt-in flow, a second ingest, a second query, a second set of
API types and a second console card, each drifting from the first.

A sandbox and a project environment are different products. They have their
own tables, lifecycle, permissions, URLs and pages, and nothing here changes
that. What they share is that both run workloads, and a capability of a
workload should not be written per product.

ADR-052 set the common workload model and its identity. This ADR states what
that model means for the code around a capability, and records the decisions
taken when recording was moved onto it.

## Decision

### D1. Entities stay separate; capabilities are shared

A workload kind keeps its own entities, data and authorization. A capability
of workloads (recording outbound connections, and after it telemetry,
activity views, network policy surfaces) is implemented once, against
`WorkloadRef`, in a workload-level crate. It is not implemented in a product
crate and it is not copied into a second one.

### D2. A kind provides one small adapter per capability

What genuinely differs by kind is answered by an adapter the kind
implements, and by nothing else:

| Question | Sandbox | Environment |
|---|---|---|
| Which row holds the opt-in? | `sandboxes.egress_recording` | `environments.egress_recording` |
| What identifies it as a workload? | `sandbox/<id>` | `deployment/<environment id>` |
| Which project owns it? | its project, or none | its project |
| Where does it run? | its one node | every node hosting one of its containers |
| What is it called? | `sandbox`, `temps.sandbox.id` | `environment`, `temps.environment.id` |

Configuration stays a column on the row it configures; the adapter knows
where that column is.

### D3. Authorization and URLs stay with the kind

Each kind keeps its own URLs where its users expect them and checks access
in its own terms (a sandbox's owner; a project's `EnvironmentsRead` and
`EnvironmentsWrite`). Its handlers are thin: resolve the caller's workload,
call the shared service. Request and response types, the audit entry and
every error are the shared ones.

### D4. Records are owned by a workload, and a project is optional

`otel_log_events` gains `workload_kind` and `workload_id`, with a partial
index, and `project_id` becomes nullable. A record the platform observes
about a workload is stored with that identity and read by it. It is also its
project's log record when the workload has a project.

This replaces the `sandbox_id` column, which was never released, and it
replaces the earlier decision to refuse recording for a sandbox without a
project: a projectless sandbox now records, and its records are read from
its own page. Records without a project are not archived to object storage,
whose layout is per project; they live in the queryable store for its
retention period.

### D5. One recorder per node, with a traffic source per kind

Node 0 runs one egress proxy with one table and one recording channel for
every kind. A kind whose workloads run on the control plane provides a
traffic source: where on the host its workloads reach the proxy, which
workloads record and under what policy, and what to do when the proxy starts
or stops serving it.

How traffic reaches the proxy is the one part that really differs:

- **Sandboxes** have no other way out. Their sidecar hands recorded traffic
  to the proxy, which applies the sandbox's own egress policy.
- **Environments** reach the network directly, so their containers are
  steered: a replica of an environment that records is started with the
  proxy as its HTTP(S) proxy. This is opt-in and cooperative, and its limits
  are accepted for now:
  - it applies to containers started after it is turned on, and turning it
    off stops recording at once without touching running containers;
  - only clients that honour the proxy variables are recorded;
  - only public destinations go through the proxy. Internal names and
    addresses stay direct, and what the bypass list misses is refused and
    shows as refused in the activity;
  - while it is on, the environment's outbound HTTP(S) depends on the node's
    proxy being up, including across a control-plane restart;
  - only replicas on the control plane are recorded until worker agents
    serve the proxy, and only replicas of image deployments: a Compose
    stack's containers are not steered;
  - the proxy knows a caller by the address Docker gave its container, so
    the deploy path announces a new replica to the recorder as soon as it is
    created. A connection made in the instant before that is refused.

  A steered container stays in the proxy's table for as long as it runs,
  whether or not its environment still records, so turning recording off
  never cuts a running container off. Only environments the database places
  on the node are recorded there.

  Redirecting at the network layer (ADR-055 §9) removes the first, second,
  fourth and last limits and is the intended replacement.

### D6. The console has one component per capability

A capability's console surface is one component that takes a workload
descriptor (its noun and its endpoints). The sandbox page and the
environment page render the same component.

## Consequences

- Recording for a third kind (a job, a function) is an adapter, a thin
  handler and, if it runs on the control plane, a traffic source. The flow,
  storage, API types and console need no change.
- `temps-workload-egress` depends on OpenTelemetry storage, so it is a
  domain crate, not a runtime one. The proxy and its record types stay in
  `temps-workload-netpol`, which no node-side crate needs the database for.
- `LogRecord.project_id` is optional in the API. Clients that read log
  records see `null` for a record of a projectless workload, which only a
  workload's own activity endpoint returns.
- A workload on several nodes records wherever a node can. Its status lists
  each node and why one cannot record.
- Review has something concrete to check: a new workload feature in a
  product crate that does not go through `WorkloadRef` is duplication in
  waiting, and is sent back.

## Not decided here

- The per-node egress gateway container that replaces the sandbox sidecar,
  and its status and ports in the console
  (`docs/plans/sandbox-egress-without-sidecars.md`).
- Sending a workload's own OpenTelemetry data through the platform for any
  kind. It follows the same shape: one relay, one adapter per kind, records
  owned by the workload.
- Recording on worker nodes, which waits for their agents to serve the
  proxy.
