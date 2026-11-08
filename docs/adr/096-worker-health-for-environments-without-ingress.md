<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-096: Worker health for environments without ingress

**Status:** Proposed
**Amends:** [ADR-068](./068-environment-addresses-without-a-public-preview-domain.md)
**Builds on:** [ADR-053](./053-distributed-runtime-health-and-worker-uptime.md), [ADR-064](./064-making-a-project-reachable.md), [ADR-020](./020-multi-node-deployment-hardening.md)

## Context

A hosted control plane cannot check its worker's application through a generated
loopback hostname. A deployed container can therefore pass its runtime checks
while its project card reports an HTTP outage. ADR-068 D4 currently neutralizes
only DNS failure: connection refusal against a generated local address remains
an outage. That rule does not describe workload health on remote workers.

The repository already implements ADR-053's authenticated runtime-health
assignment, local scheduling, protected container creation receipts, and bounded
observation reporting. Reuse that protocol; do not introduce a second HTTP
prober or send caller-selected targets to workers.

## Decision

For an environment with worker placements and no configured reachable address,
use fresh runtime observations for its current deployment as the application
health signal. The existing worker engine checks its own container's declared
port and deployment-authorized relative health path through its runtime's
published endpoint. The control plane consumes observations from the existing
agent management channel. It does not issue periodic arbitrary URL requests.

For environments with public addresses, preserve external monitoring: failure
of the configured public route must remain visible even if runtime health is
green. Failed configured ingress is not equivalent to ingress never configured.
Keep explicitly configured monitors and single-host local monitoring semantics.
Address resolution uncertainty must not manufacture either a healthy result or
proof that ingress is absent.

Runtime health and reachability remain independent. A passing private workload
is healthy, without claiming visitors can reach it. Unknown, unsupported, expired
or missing worker observations are neutral. An older worker retains the existing
monitoring path where it can check the application; an inaccessible generated
address must not become evidence of a failed workload. Do not change the
reachability banner described by ADR-068 D5.

## Security and resource bounds

Reuse ADR-053's immutable deployment revision and worker-owned creation receipt:
container identity, project, environment, deployment and declared port must all
match. Probe URLs and hosts cannot be supplied by the control plane. Relative
paths are validated and redirects cannot escape the container. Reports contain
bounded outcomes and durations, never response bodies or credential-bearing
transport errors. Retain authenticated TLS and node identity checks.

Use the existing bounded scheduler, timeouts, per-container serialization,
lease expiry and replay protection. No new hot-path work, unbounded queue or
per-project poller is introduced. At saturation observations expire to unknown.
Results from replaced deployments, retired replicas, changed placements and
expired leases must not remain green. Existing containers without trustworthy
creation receipts remain unknown until an authorized deployment establishes
ownership; never reconstruct ownership from caller-supplied labels alone.

## Validation and rollout

Add focused tests for check selection, worker target restrictions, the existing
agent report round trip and project card presentation. Run touched-crate check
and clippy plus focused tests, and web type checking/formatting. Integration
acceptance is a later step on an internal canary; this change stays local until
that validation. No public API shape change is intended.
