# ADR 057: Removing a worker node that is gone

## Status

Accepted — 2026-10-20

Related: ADR-048 (sandboxes on worker nodes), ADR 088 (outbound join),
ADR 056 (compute pool; a node that never reported its overlay locks the pool).

## Context

A node is removed after a drain has moved its deployments elsewhere. The
removal refuses while any container of a live deployment (the environment's
current one, one in progress, or a rollout's fallback) is recorded on the
node, and `force=true` only covered containers Temps could not confirm were
removed. The drain-first rule exists so a node is never pulled out from
under its traffic.

That left a node with no way out once its agent was gone:

- A drain needs the agent to move containers, so it never finishes.
- The health check only marks `active` nodes offline, so a node that was
  draining when its agent stopped stays `draining` forever. The console
  offered **Remove** only for `drained` or `offline` nodes.
- A control plane with no other worker (a hosted control plane runs no
  workloads) has nowhere to drain to.

On the lab this blocked a hosted control plane: the stuck node's allocation
locked the compute pool (ADR 056), so the pool could not be changed either.

## Decision

1. **Unreachable is a state the control plane can see.** A node is
   unreachable when it has never sent a heartbeat, or has sent none for
   longer than the health check's threshold (90 s,
   `NODE_HEARTBEAT_STALE_SECS`, shared with the health check). The heartbeat
   time is the control plane's own receive time. The status is not used: a
   draining node keeps its status after its agent stops, and the health
   check can write `offline` just after a heartbeat brought a node back.
2. **`force=true` on an unreachable node also lets go of what it serves.**
   Containers of live deployments on it are recorded as orphaned, like any
   other container whose removal was not confirmed. They keep their rows
   with a note naming the node, so cleanup never touches another daemon.
   The response's `lost_environments`, the audit record and the log name the
   environments that were serving from it. Those with no replicas on
   another node serve nothing until they are redeployed. Deleted projects
   are not listed.
3. **A reachable node keeps the drain-first rule.** Force does not override
   it. Reachability is checked again under the node's row lock, so a
   heartbeat that lands between the check and the delete keeps the refusal,
   and a refused forced removal never answers "retry with force".
4. **The refusal names the way out.** Removing an unreachable node that
   still serves returns `409 Unreachable Node Still Serves Deployments`.
   The response lists the environments and sets `force_removes_serving`.
   The console shows them in the remove dialog with **Remove anyway**.
   **Remove** is offered for any unreachable node, whatever its status.
5. **A stuck drain says so.** The drain status of an unreachable draining
   node says the drain cannot finish and points at removing the node
   instead of reporting progress.

Live sandboxes still refuse removal; they are destroyed from the node's
Sandboxes tab as before.

## Security

- Removal keeps its gates: `SettingsWrite`, sensitive-action step-up, and an
  audit record. The audit record now includes `lost_environments` and
  `node_unreachable`. Step-up is a recent-verification window shared by
  every sensitive action, so it is not tied to `force`.
- A node cannot make itself removable. Unreachability comes from the
  control plane's own record of heartbeats. A node that stops heartbeating
  only lets an operator remove it, which it could already do with drain plus
  removal.
- If the host comes back, its agent is no longer a member: a removed node's
  token and identity are gone with its row. The containers it kept running
  are recorded as orphaned on it and must be removed there by hand, as
  with the existing force removal.

## Consequences

- An operator can always remove a node whose host is gone. The cost of doing
  so is explicit: the dialog and the response name every environment that
  stops serving.
- Automatic failover is unchanged. It still redeploys a silent node's
  environments after the grace period when another node can take them.
  Removal is the operator's decision when it cannot.
