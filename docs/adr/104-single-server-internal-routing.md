<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-104: A single-server control plane serves its own containers' `*.temps.local` names

**Status:** Proposed — 2026-11-12

Related: ADR-024 (the control-plane resolver on the app network's gateway),
ADR-098 (the worker's internal proxy on its own port), ADR-101 (internal access
grants), ADR-095 (DNS observability). Issue #1591, point 1.

## Context

With **Enable cluster DNS** on, every environment gets an internal address,
`<environment>.<project>.temps.local`, that other containers call over plain
HTTP on port 80. The deployment DNS publisher points each name at every worker
node's bridge address (`compute_cidr` .1). There, the worker's internal proxy
(ADR-098) routes the call by `Host`. It admits the caller's own project, and
another project only through an internal access grant (ADR-101). Everything
else gets a 403.

A single-server install has no worker nodes. Its control plane runs the
cluster resolver on the `temps-app-network` gateway (ADR-024) and points its
local containers at it, but:

- the publisher finds no worker bridge, so it publishes no environment name,
  and every `*.temps.local` lookup from a local container is NXDOMAIN;
- nothing would serve a call even if the name resolved. The public proxy
  (pingora) holds the route table, but it serves whatever reaches it and
  applies neither the caller check nor the grants;
- the DNS page shows the control plane's internal routing as "Main proxy",
  and **Test resolution**, asked as a project, fails with "no container of
  this project". The console says nothing true about where these names go.

So cross-project calls, and same-project calls by internal name, do not work
on the most common install.

## Decision

### D1. The control plane runs the worker's internal routing on its gateway

While cluster DNS is on, the control-plane resolver answers on the app
network's gateway, and **no worker node has a bridge address**, the `temps
serve` process runs `temps_node_agent::internal_routing::supervise` on that
gateway: the same function a worker runs on its bridge. Nothing is forked.
That means:

- the proxy listens on port 591 of the gateway, never on `0.0.0.0` or any
  other address;
- one nftables rule in the table `ip temps_internal_proxy` redirects
  containers' calls to `<gateway>:80` to it, with the 90 s liveness set
  (ADR-098 D2, D3);
- the caller is identified from its source container through the local
  Docker daemon, re-checked against Docker behind the per-project token
  bucket, and decided by ADR-101's `internal_access::authorize`. Same
  project: allowed. Another project: allowed only through a grant. Anything
  else, including a container the route table does not know (a database, the
  console): the existing 403.

The DNS bootstrap loop (`serve/proxy.rs`) owns it: it starts the routing when
the resolver first answers on a gateway, restarts it when the gateway changes,
and stops it when cluster DNS is turned off, the resolver fails, or a worker
node appears. It stays off while any worker has a bridge, because the names
then point at the workers. It also never competes for the redirect table with
a `temps agent` that runs on the same host.

### D2. Routes come from the route table, in process

A worker receives its routes over the long poll (`RouteSnapshot.routes`). The
control plane builds the same entries from its own route table with
`temps_routes::route_sync::internal_route_entries`. That is the function the
worker snapshot now uses too: the `*.temps.local` routes with every live
grant attached (ADR-101 D5). It applies them to a `RouteStore`. A feeder
rebuilds the store:

- on every route-table generation bump: deployments, and grant changes
  through `route_table_changes`;
- at least every 60 s, so a lost wakeup cannot keep a revoked grant in force
  for longer than the route table's own idle reconcile would.

The store keeps its last snapshot in `<data_dir>/internal-routing/routes.json`
for a restart.

### D3. The names point at the gateway only while the proxy serves it

`DeploymentDnsPublisher` gets a second slot, separate from the resolver's
gateway slot that the deployer reads:

- **Worker bridges known:** names point at them, as before.
- **No worker bridge, and the control plane's internal proxy listening on
  `<gateway>:80`:** each environment's name is one A record at the gateway,
  with `node_id` `NULL` (the control plane has no `nodes` row).
- **Otherwise:** no name points at the control plane. Records with `node_id
  NULL` and owner kind `environment` are withdrawn.

The loop sets the slot from the supervisor's own status: listening, on this
gateway. When the value changes, it reloads the route table, which runs the
publisher. A name therefore never points at a gateway whose port 80 belongs to
something else. In particular, it never points at the public proxy, which
applies no caller check.

### D4. What the control plane reports

A worker reports its routing in the heartbeat's `capacity.internal_proxy`
(ADR-098 D5). The control plane has no heartbeat, so it writes the same
document to a new nullable column, `dns_resolver_reports.internal_proxy`
(jsonb), on node 0's row, each time it stores its resolver report. `NULL`
means not reported: cluster DNS is off, routing is not running, or the binary
predates this ADR.

The DNS page reads it as it reads a worker's: *Redirected*, *Direct on port
80*, *Down* with the reason, and the address (`172.20.0.1:80 → 172.20.0.1:591`).
"Main proxy" remains only for a control plane that reports nothing. The
`dns_internal_proxy_down` alarm covers node 0 too. Its message says the names
are withdrawn rather than that they resolve to nothing.

**Test resolution** from the control plane now finds the project's container
(its names are published) and adds a note when the answer is the control
plane's gateway: what serves the call, and that internal access applies. The
internal access verdict (grant state, whether a grant admits the caller, and
the link to the owner's card) comes from the same grant check as on a worker.
It was already independent of the node.

### D5. When the gateway's port is in use

The supervisor's fallbacks are unchanged (ADR-098 D4):

- **591 held, or no `nft`:** it tries a direct bind on `<gateway>:80`. On a
  control plane whose public proxy is bound to `0.0.0.0:80`, that bind fails
  too. Routing is then *Down*, with an error that says to install nftables or
  free port 591, and per D3 the names are withdrawn. The default listener
  (`127.0.0.1:3000`) leaves the gateway's port 80 free, so the fallback works
  there.
- **Both fail:** it retries every 30 s, and the state clears by itself once
  the cause is gone.

The redirect matches only traffic arriving from containers (prerouting).
Host-originated calls to the gateway are not redirected, as on a worker. The
supervisor's error messages say "worker" and "agent". They are the worker's
text, shown verbatim; the console's own copy names the control plane.

### D6. Hot-path budget

The internal proxy is not the public request path. It serves
container-to-container calls on one host. The expected load is tens to a few
hundred requests per second on a 2 vCPU / 4 GB single server. This ADR adds
no per-request work beyond what a worker does today (ADR-098, ADR-101):

- **Per request:** an in-memory host lookup and caller-identity lookup. These
  are `parking_lot` read locks in `RouteStore`, inherited from the worker, and
  written only when a snapshot is applied. Then one Docker inspect to
  re-confirm an admitted caller. That inspect is bounded per caller project
  (16 burst, refilled 8 per 125 ms, at most 2 concurrent) and globally (32
  concurrent, 100 ms queue, 500 ms timeout). Past the bound the proxy fails
  closed (403), so a busy caller cannot amplify into the Docker daemon. A
  refused caller never reaches Docker.
- **No database or disk I/O per request.** Grants are read once per snapshot
  rebuild (one query, O(grants)). The snapshot is written to disk once per
  rebuild.
- **Memory:** O(internal routes + local containers + calling projects). That
  is a few hundred bytes per route and per container, a token bucket per
  calling project, and the 256-entry ACME cache the worker proxy already has.
  For 100 environments and 300 containers, well under 1 MB.
- **Background:** one nft transaction every 30 s, one supervisor pass every
  30 s, one worker-node query every 5 s in the DNS loop (one indexed table,
  a handful of rows), and one snapshot rebuild per route-table generation.

The read locks on the worker's request path predate this ADR. Moving them to
`ArcSwap` would help workers and the control plane alike, and belongs in its
own change.

### D7. Upgrade and rollback

- **Upgrade:** the migration adds a nullable column with nothing to backfill.
  On the first DNS loop pass with cluster DNS on, the proxy starts, and within
  one pass after it listens, the names are published at the gateway. Running
  containers already use the control-plane resolver (ADR-024), so they
  resolve the new names without a restart. Installs with worker nodes change
  in no visible way: the control plane's routing stays off.
- **Rollback to an older binary:**
  - The nft redirect stops matching within 90 s, when the liveness element
    expires, and calls to `<gateway>:80` then reach whatever holds that port.
  - The `node_id NULL` environment records stay. The older publisher still
    finds no worker bridge, so it does not replace them, and the older
    resolver keeps answering them. Calls by name then reach `<gateway>:80`
    without the caller check.
  - So **turn cluster DNS off before rolling back**, which withdraws the
    names (D3), or delete those rows afterwards:
    `DELETE FROM service_endpoints WHERE owner_kind = 'environment' AND node_id IS NULL`.
  - The migration's `down` drops the column.

### D8. Not covered

- **Split mode.** `temps proxy` run separately from the console process does
  not serve local containers. The console process deploys them and owns the
  gateway slot, and the two processes do not share it. The standalone proxy
  passes no local routing, so nothing changes there.
- **The public proxy still routes `*.temps.local` hosts.** pingora loads these
  names into its route table and serves a request carrying such a `Host` to
  any peer that reaches it, without the caller check. A container can reach it
  through the host's own address (not the gateway's port 80, which the
  redirect takes). This was true before this ADR, on single-server and
  multi-node installs alike. Refusing internal hosts on the public listeners
  is a follow-up. It needs its own review of the public request path.
- `host.docker.internal`, persistent volumes and custom service types
  (issue #1591's other points).

## Consequences

- On a single server with cluster DNS on, `curl http://<env>.<project>.temps.local`
  from a container works for the same project, answers 403 for another
  project, and works once that project is granted. The rules are the same as
  on a worker.
- The control plane holds `<gateway>:591` and one nft table while it serves.
  Both go away when cluster DNS is off or a worker joins.
- The DNS page, the alarm and Test resolution describe the control plane's
  routing from what it reports, not from an assumption.

## Alternatives considered

- **Enforce in pingora.** This would put the caller lookup (Docker, source
  address) and the grant decision on the public hot path, and fork ADR-101's
  logic. It was rejected in favour of running the worker's proxy unchanged.
- **Bind the internal proxy on `0.0.0.0` or the host address.** It would
  expose internal routes beyond the app network. Rejected.
- **A column to opt in.** Cluster DNS on is already the opt-in. A second
  switch would only add a state in which the names resolve to nothing.
