# ADR 045: Host Docker socket grant for operator-owned infrastructure services

**Status:** Proposed
**Date:** 2026-09-21
**Related:** ADR 040 (build/node decoupling), ADR 042 (multi-node join)

## Context

Temps never hands a deployed container the host's Docker engine. The compose
executor rejects `privileged`, `use_api_socket`, `cap_add`, `devices` and any
absolute host-path bind; the image deployer builds a `HostConfig` with
`cap_drop: ALL`, `no-new-privileges` and exactly one bind (the secrets tmpfs).
That is the right default: a container holding `/var/run/docker.sock` is root
on the host and can read every other tenant's volumes.

There is one class of workload that legitimately needs the socket and that an
operator wants to run *through* Temps rather than beside it: a per-host agent
whose job is to manage containers on that host on behalf of an external
control plane (a node daemon, a runner, a local registry mirror). Running it as
a Temps project gives it what every other project gets for free -- image
builds, blue/green rollout with health gating and rollback, routing, logs,
metrics -- and none of that is available to a hand-installed systemd unit.
Compose is not an answer here: a compose deploy recreates in place and takes
the service down for the duration.

The question is how to allow that for a handful of operator-owned services
without weakening the default for everything else.

## Decision

A **host-level, operator-set grant**, keyed by project slug, evaluated by the
process that actually creates the container.

### The grant is host policy, not project configuration

`TEMPS_DOCKER_SOCKET_PROJECTS=<slug>[,<slug>...]` is read once at startup by
each process that can run a deployment: `temps serve` for local placement,
and `temps agent` on every worker. Unset means the current behaviour, on every
host, unconditionally.

This is deliberately an environment variable rather than a column, following
`TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES` and `TEMPS_TRAEFIK_DISCOVERY_ENABLED`:
which projects may hold root on *this machine* is a decision for whoever has a
shell on this machine. It must not be reachable through the API, because any
account or token that could flip it would be one step from the host. It is
also naturally per host -- a worker that never opted in cannot be granted by
the control plane, and vice versa.

### The bind is added where the container is built

`DeployRequest` carries the project slug. The image deployer's single
`HostConfig` build site compares that slug against its own process's parsed
grant set and, on a match, adds exactly one bind:
`/var/run/docker.sock:/var/run/docker.sock`. Nothing else changes:
`cap_drop: ALL`, `no-new-privileges`, the PID limit, the read-only secrets
tmpfs all stay. The container is not privileged; it holds one file descriptor
that happens to be root-equivalent, which is the minimum the workload needs.

Because `DeployRequest` is serialised verbatim to the worker agent and the
agent runs the same deployer code, the decision is made by the executing
process against its *own* environment in both the local and the remote case.
The control plane never tells a worker "mount the socket"; it tells it "this
is project X", and the worker answers from its own policy.

### Placement is gated up front, never discovered mid-deploy

A worker advertises its grant set in its heartbeat. When a granted project is
scheduled, the node scheduler keeps only nodes (or the control plane itself,
by its own environment) that advertise the grant for that slug, and records
the exclusion reason per node the way the architecture filter does. If no
node qualifies the deployment fails before anything is built, with the same
actionable worker-node problem the platform already uses, carrying the reason
"project X is not granted host Docker access on any node". A granted project
is never silently placed on a host that would quietly deploy it without the
socket.

The companion control is the address the granted workload is *published* on.
A service that holds the socket is root-equivalent on its host, so its own
API must not be reachable from anywhere that host is. `temps agent` already
binds every published container port to the node's registered
`nodes.private_address` -- the WireGuard tunnel IP in relay mode, or the
operator-managed address in direct mode -- and never to `0.0.0.0`; the value
is set with `--private-address` / `TEMPS_AGENT_PRIVATE_ADDRESS`, validated as
an IP literal with reserved ranges (including `0.0.0.0`) rejected at startup,
and logged on every boot. Granting the socket to a project on a node whose
private address is a public IP puts a host-root-equivalent API on the public
internet; that combination is the one an operator must not ship.

### It is visible, and it is audited

The project response carries a capability object -- `granted`, `reason`,
`setup_path`, plus the nodes that advertise the grant -- so the console can
show a "Host Docker access" badge on the project header and an onboarding
state explaining what to set and where when it is not granted. Every
deployment that mounts the socket records an audit event naming the project
and the node. The CLI surfaces the same capability.

Compose deployments are unchanged: the deny-list stays as it is. The grant
applies only to the image deployment path.

## Consequences

- A granted project is host-root-equivalent on the hosts that grant it, and
  its published ports are only as private as that node's
  `private_address`. Pin that to the overlay/WireGuard address the control
  plane reaches before granting anything.
  This ADR exists so that sentence is written down: the grant is for
  operator-owned infrastructure services, and an operator who grants it to a
  tenant application has removed the isolation boundary on that host.
- Self-hosted instances that never set the variable see no behaviour change
  and no new API-writable surface.
- Operators get zero-downtime rollouts for the one kind of service that used
  to require a hand-maintained unit file, without a general "privileged
  deploy" switch existing anywhere in the product.
- Multi-node works by construction: the grant travels with each host's
  environment, and scheduling reads it from heartbeats rather than trusting
  the control plane's view.
