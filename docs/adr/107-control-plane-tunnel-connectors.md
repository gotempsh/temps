<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-107: The control plane runs tunnel connectors for the apps it serves

**Status:** Proposed
**Date:** 2026-11-19
**Amends:** [ADR-058](./058-ingress-modes-and-a-shared-app-domain.md) (D3 and D9: "only workers run connectors", "only what workers serve")

Related: ADR-070 (HTTPS-only tunnel addresses), ADR-080 (control-plane
enrollment and ingress nodes), ADR-064/065 (private and signed-in addresses,
Cloudflare Access).

## Context

ADR-058 made a tunnel a connector that runs on workers: the node agent
supervises a pinned `cloudflared` and forwards its traffic to a loopback
tunnel origin listener that trusts the provider's client-address header from
loopback only. The control plane runs no agent, so a connector could not be
placed on it. `validate_nodes` refused node `0` with "Node 0 does not exist.",
and the reachability planner offered tunnels only to environments that run on
workers.

The install that most needs a tunnel is a single machine behind NAT, carrier-
grade NAT or a home router: a control plane with no workers and no mesh. Its
only answers were "add a second machine as a worker" or "open 80/443 and point
DNS at your public address".

ADR-058 records why ADR-080 (then 048) rejected tunnels: the *console* must
not depend on a third party to be reachable. That objection stands and this
ADR keeps it. What changes is app traffic only: environments the control
plane's own proxy serves, on hostnames the operator chose to publish.

## Decision

### D1. The control plane is node `0` for connectors

Node `0` is the control plane everywhere else (`temps_core::CONTROL_PLANE_NODE_ID`,
re-exported by the workload scheduler). The ingress API accepts it in
`node_ids` on create and on `PUT /ingress/connectors/{id}/nodes`, and shows it
as **Control plane**.

The control plane has no `nodes` row, and `ingress_connector_nodes.node_id`
referenced `nodes(id)`. The foreign key is replaced by two triggers that keep
what it guaranteed for every other id: a non-zero `node_id` must name a
`nodes` row (locked `FOR KEY SHARE`, as the foreign key check does), and
deleting a node deletes its rows. `0` stays an ordinary integer rather than
`NULL`, so a binary rolled back after the upgrade still decodes every row;
the existing `UNIQUE (connector_id, node_id)` keeps one control-plane replica
per connector. The downgrade refuses while a connector is placed on node `0`.

Validation keeps its messages for workers and becomes precise for the rest:

- an unknown id: `Node {id} does not exist (use 0 for the control plane).`;
- a non-worker row and an inactive worker: unchanged;
- the delegated-ingress consent of ADR-058 D7 is a worker's: the control
  plane's operator is the one creating the tunnel, so node `0` has no consent
  step.

### D2. The control plane supervises the connector in-process

`temps serve` runs the node agent's `ConnectorSupervisor` in its own process,
with the same process driver: the same compiled-in list of trusted
`cloudflared` artifacts (URL, platform and SHA-256), the same 0600 secret
files, the same readiness probe, restart backoff and parent-death signal. No
second implementation exists, and the pinned-binary trust model is unchanged:
the control plane can only choose among artifacts the binary already trusts.

A loop in `temps-ingress` (`ControlPlaneConnectors`) does what the agent's
sync loop does over HTTP, against the service directly: every 15 seconds, and
right after a connector's placement changes, it reads
`node_specs(0)`, hands them to the supervisor and writes the supervisor's
statuses with `record_status(0, ..)`. Status lands in the same
`ingress_connector_nodes` row the console already reads for workers. The
specs are not cached on disk: the control plane is the source of truth.

The supervisor gains one mode. On a worker it binds its own tunnel origin
listener; on the control plane the origin is the proxy's (D3), already bound,
so the supervisor only checks that the connector's `edge_port` is the port the
proxy listens on and reports a clear `down` status when it is not. Why the
control plane cannot run a connector is a typed error
(`ControlPlaneTunnelsUnavailable`: the proxy is another process, or its
tunnel origin is not bound), turned into text only where it is shown.

`temps serve` starts this loop itself, next to the proxy and from what the
proxy already has (database, encryption, route table), not from the
console's startup: a console that fails to start leaves the proxy serving
apps, and a tunnel that is the only way in must keep carrying them. The
console's ingress service shares the loop's handle, so it accepts node `0`
and wakes the loop when a placement changes.

Only the combined `temps serve` (console and proxy in one process) runs
connectors. With `--role console` the proxy is another process, possibly on
another machine, and node `0` reports `down` with that reason.

### D3. The proxy's tunnel origin: loopback, plain HTTP, trusted headers from the connector only

At startup the proxy binds `127.0.0.1:7480` (the default `edge_port`) as a
second plain-HTTP listener, the **tunnel origin**. Pingora cannot add
listeners after it starts, so it is bound whether or not a connector exists.

A worker agent on the same machine (a control plane also enrolled as a
worker) already holds `7480` for its own tunnel origin, so a taken port is
not the end: the proxy takes the first free port up to `7489` and logs which,
skipping the ports its own listeners (HTTP, TLS, the console's) are
configured on, which are not bound yet when it chooses.
The port is part of the provider's tunnel configuration
(`http://127.0.0.1:{edge_port}`), so a tunnel placed **only** on the control
plane follows it: a new one is created on that port, and the connector loop
moves an existing one, telling the provider first and keeping the new port
only once the provider accepted it (a failure leaves the old port and the
error on the tunnel, and is not retried every pass). Only one control-plane process
owns a database, so a per-connector lock in that process, shared by the
console's and the connector loop's services, orders every configuration of a
tunnel: a run reads the row, calls the provider and saves the outcome before
the next one starts, holding no database connection while it waits. Only
another configuration of the same tunnel waits on it; placement changes,
status reports and token-holder updates never wait for the provider. A move
re-checks right before the provider call that the tunnel still runs only on
the control plane, and saves the port only once the provider accepted it,
so the next pass simply makes an interrupted move again; a worker added
during the call gets the moved port, which it binds like any other. A tunnel also placed on
workers keeps its port, since every worker binds it itself; its control-plane
replica reports why it does not run. With every port taken, the listener is
skipped and a connector placed on node `0` reports why it cannot start.

A request that arrived on the tunnel origin:

- is **secure**: the visitor reached Cloudflare over HTTPS (ADR-070), so no
  HTTPS redirect (an app's, the production default's, a status page's or an
  on-demand certificate's), `Secure` cookies, Shield challenges instead of
  "use HTTPS", `X-Forwarded-Proto: https` upstream. The listener decides
  this; the edge's `X-Forwarded-Proto` can only *downgrade* it: a hostname
  whose edge policy lets plain HTTP through forwards `http`, and that
  visitor gets the app's HTTPS redirect and no `Secure` cookie. A missing
  header or any other value stays secure, so a forged header can cause a
  redirect but never skip one, and no other listener reads it (SEC-12).
  Only SNI validation stays TLS-only;
- is **via a tunnel**: Cloudflare Access addresses (ADR-065 D9) are verified
  on it like on a worker's tunnel origin, and still refused on every other
  listener;
- takes its client address from the connector's client-address header
  (`CF-Connecting-IP`), which the supervisor announces from the connector
  spec. No header announced, or an unparsable one: the TCP peer.

The header is trusted on that listener only, which listens on loopback only:
the same boundary as a worker's tunnel origin (ADR-058 D2, `tunnel` mode). A
`CF-Connecting-IP` sent to the public listeners is still trusted only from
Cloudflare's published ranges, as before. Every request on the tunnel origin
still goes through the full control-plane pipeline: routing, the gate,
IP access rules, Shield, edge rules, logging.

The console is not reachable through a control-plane tunnel unless the
operator publishes its hostname on purpose: a tunnel publishes route-table
hostnames (D4), and the console's own host is not one.

### D4. What a control-plane tunnel publishes

ADR-058 D9 publishes "only what workers serve" so a tunnel never sends a
visitor to a replica that answers 404. Cloudflare spreads a tunnel's
requests over every replica, so a hostname is publishable when **every**
replica serves it:

| Placement | Publishes |
|---|---|
| workers only | hostnames workers serve (unchanged) |
| control plane only | every public hostname in the control plane's route table, published status pages' custom hostnames, and those of sleeping on-demand environments, which have no route until a visit through this proxy wakes them (read from the same route snapshot, so an app waking up or falling asleep is never in neither): the control-plane proxy serves them all, including those on workers, and applies every policy that keeps a host off workers (redirects, wake-up, attack mode, path routing) |
| control plane and workers | hostnames workers serve: the control plane serves those too, so both replicas answer |

A mixed placement is allowed. It behaves like the workers-only placement for
publishing, and gains a replica that keeps the tunnel up while the workers
are away; the hostname view says so for a host only the control plane serves.

### D5. The planner offers the tunnel to environments the control plane serves

`get_reachability_choices` and the environment address warning offer
`new_tunnel` (and ready tunnels that run on the control plane) to an
environment that runs on the control plane, placing the new tunnel on node
`0`. The `not_offered` reason for an environment no tunnel can serve says
which placement would serve it. The assistant's playbooks stop saying a
tunnel needs a worker.

### D6. Not in this change

- **JA4 through a control-plane tunnel.** A worker's tunnel origin takes
  Cloudflare's `cf-ja4` header; the control-plane proxy computes JA4 from the
  TLS handshake only, so tunnel requests reach Shield without one.
- **An immediate Access key refresh on the control plane.** A worker that
  sees an Access JWT signed with a key it does not know asks the control
  plane to refetch the team's keys at once. The control-plane proxy has no
  such request yet: it picks up a rotated key at the hourly refresh, so for
  up to an hour after Cloudflare starts signing with a new key, Access
  addresses on a control-plane tunnel answer `unknown_key`.
- **Container and library drivers on the control plane.** The supervisor
  offers only the process driver there, as on workers today.
- **A chosen tunnel origin port.** The control plane takes the first free
  port from `7480` to `7489` (D3); an operator cannot pick one.

## Consequences

- A single machine behind NAT publishes its apps through Cloudflare with no
  open port and no worker.
- The control-plane process spawns a long-running child. It is tied to the
  process with `PR_SET_PDEATHSIG` like the agent's, so it ends with Temps.
- One more loopback listener on every combined install, idle unless a
  connector runs on node `0`.
- `ingress_connector_nodes` checks node references with triggers instead of
  a foreign key; the migration test covers both triggers and the downgrade.
