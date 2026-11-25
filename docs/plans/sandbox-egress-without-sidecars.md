<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Sandbox egress without a sidecar per sandbox, and records kept per workload

Status: section 6 is built, for every workload kind
([ADR-115](../adr/115-workload-capabilities-written-once.md)). Sections 1
to 5 (the egress gateway and its console status) are a proposal and are not
built yet.

## Goal

- **Fewest resources.** No container per sandbox for egress. Each node gets
  one entry point for every sandbox's outbound traffic, and one policy and
  recording engine.
- **Every sandbox's activity can be read on its own**, from the sandbox
  page. Nobody has to open a project's logs and filter by an attribute.
- **Records stay OpenTelemetry log records**, with the same fields,
  semantic-convention attributes and trace and span ids. Anything that reads
  OTel logs keeps working.

## Where things are today (measured on a development host)

| Per node | Containers | Memory in use | Cap |
|---|---|---|---|
| Preview gateway (Rust, `crates/temps-preview-gateway`) | 1 | 3.1 MiB | 128 MiB |
| Egress sidecar (Node.js) | **1 per sandbox** | **16.5 MiB each** | 128 MiB, 64 pids, 0.25 CPU each |

At 50 sandboxes the sidecars use about 825 MiB, which is 40% of the
1 vCPU / 2 GB target, before any sandbox does real work.

Each sandbox runs on its own internal Docker network, with no gateway and no
host address, so the network itself has no way out. The sidecar is the only
container attached both to that network and to the shared outbound network
`temps-sandbox-egress-v1`. The sidecar does five jobs:

1. It enforces the sandbox's egress policy (internet or restricted, public
   addresses only).
2. It relays model calls (`/.temps/model-relay/...`), MCP (`/.temps/mcp/...`)
   and git (`/.temps/git/...`) to the control plane.
3. It routes `*.temps.local` names to this host's internal proxy (ADR 105),
   which identifies the sandbox by the sidecar's address.
4. It logs refusals.
5. It hands recorded sandboxes' traffic to the Rust recorder. **This
   hand-off is removed by this plan.**

**The preview gateway is already attached to every sandbox network.** That
is the key fact: a container that reaches every sandbox already exists, and
it is the smallest one on the node.

## Proposal

### 1. One egress gateway per node, separate from the preview gateway

The preview gateway and the egress gateway are separate containers. They
can be built from the same image, with a different entrypoint each, so
there is one image to release and the code that walks sandbox networks is
shared. Keeping the two jobs apart is worth one fixed container per node:

- **Privilege.** The preview gateway handles traffic from the internet.
  The egress gateway asserts sandbox identity and sits on the outbound
  network. In one process, a flaw on the internet-facing side could forge
  any sandbox's identity and send traffic out.
- **Failure and restarts.** Restarting or upgrading one gateway does not
  cut the other's connections, and heavy outbound traffic cannot starve
  previews of the shared CPU and memory caps.
- **Rollout.** Each one gets its own capability label, drift check and
  fallback.

The cost is fixed per node, a few MiB (the preview gateway uses 3.1 MiB),
and does not grow with sandboxes.

- The egress gateway joins every sandbox network with the network alias
  `temps-sandbox-egress-proxy`. A network alias is set per network, so a
  sandbox resolves it only on its own network. The gateway listens on
  `:3128`, so sandboxes keep their current `HTTP(S)_PROXY` value and need no
  environment change.
- The gateway does not parse the traffic. For each connection it:
  1. works out the sandbox from the local address the connection arrived
     on. That address belongs to the gateway's interface on one sandbox's
     network, Docker assigns it, and nothing inside a sandbox can change
     it, so the identity cannot be spoofed;
  2. opens a connection to the node's Rust egress proxy (in the Temps
     process) through `temps-sandbox-egress-v1`;
  3. writes one identity preamble naming the sandbox, signed with the
     gateway's existing secret, then forwards bytes both ways.
- The egress gateway joins `temps-sandbox-egress-v1`. Once the sidecars are
  gone, it is the only container on that network. It is not attached to
  the preview networks, and nothing from the internet reaches it.
- The Rust egress proxy accepts the preamble only from the gateway's
  address on that network, with a valid signature. Without it, the
  connection is dropped.

Cost: one fixed container per node, and the sidecars go (−16.5 MiB each). The gateway
gains one socket pair per open connection, bounded by the proxy's existing
semaphore. All forwarding is local to the host.

### 2. The Rust egress proxy takes over every job the sidecar does

The Rust proxy (`temps-workload-netpol`) already implements policy,
public-address-only checks, DNS pinning, collapsing of repeated refusals and
recording. To replace the sidecar, it adds:

- the model, MCP and git relays: path-checked forwards to the control plane,
  with the sandbox identity from the preamble instead of the sidecar's
  address;
- `*.temps.local` names, handed to the internal proxy with the sandbox
  identity (ADR 105 changes its source of identity from "the sidecar's
  address" to "the preamble from the gateway");
- refusal logging (it already does this).

Recording stays opt-in per sandbox. The proxy enforces policy for every
sandbox and writes a record only for those that opted in. The batching,
bounded channel, counted drops and collapsing all stay as they are.

### 3. Workers

A worker runs its own preview gateway, and its node agent already runs the
egress proxy, with the table that is published in every mode. The design is
the same as on the control plane. Records travel through the existing
node-authenticated `egress-records` route, and the control plane re-checks
the opt-in and the sandbox's placement.

### 4. Rollout and version skew

- The gateway advertises the capability with a label
  (`sh.temps.preview-gateway-egress=v1`). Only when it is present does the
  provider skip the sidecar and give the gateway the alias.
- With an older gateway image, the sidecar stays exactly as it is today, so
  nothing breaks mid-upgrade.
- An existing sandbox moves over the next time it is reconciled: its
  sidecar is removed and the gateway reconnects to its network with the
  alias.
- Sidecar code is deleted only in a later contract release (ADR-055
  phase 7).

### 5. Gateway status and ports in the console

- **Status**, on each node's page (the control plane included): whether
  the egress gateway is running, its image version and capability label,
  how many sandbox networks it is attached to, open connections, and
  connections refused or dropped since start. When it is not running, the
  page says why (image not pulled, older gateway image without egress
  support, or Docker unreachable) and what fixes it. While a node still uses
  sidecars, the page says so and counts them.
- **Ports**, editable per node and stored as columns on the node's settings
  row (no environment variables), audit-logged:
  - the gateway's listening port (3128 today), the port in every sandbox's
    `HTTP(S)_PROXY`. Changing it updates new sandboxes at once; running
    sandboxes pick it up when they are next reconciled. The page shows how
    many still use the old port;
  - the host egress proxy's port, which the gateway forwards to. Changing
    it restarts the gateway.
- Allowed destination ports stay per sandbox, in its egress policy.
  Internet mode allows 80 and 443; restricted rules list their own ports.
  *Assumption: "change ports" means the gateway's own ports. Say if you
  meant the allowed destination ports.*

### 6. Records stored per workload (built)

Every record stays an OTel `LogRecord`: same body, same attributes, and
trace and span ids when present. Storage gains a workload owner, the same
for every workload kind:

- `otel_log_events` gets nullable `workload_kind` and `workload_id` columns
  and a partial index on `(workload_kind, workload_id, timestamp DESC)
  WHERE workload_id IS NOT NULL`. `project_id` becomes nullable. ClickHouse
  delegates logs to this table, so it needs no change. Workload logs leave
  the workload columns `NULL`. Only the platform ingest
  (`ingest_platform_logs`) sets them, and the existing `temps.platform.*`
  stripping still stops workloads from claiming them.
- `GET /v1/sandboxes/{id}/egress-records` and
  `GET /projects/{project_id}/environments/{environment_id}/egress-records`
  return 20 per page (maximum 100), newest first, filterable by outcome.
  Each is guarded in its own product's terms and both call one service in
  `temps-workload-egress`.
- One **Outbound activity** panel, on the sandbox page and on the
  environment's settings, shows time, destination, method and path, the
  outcome with its reason, and bytes. When recording is off, the panel is in
  an onboarding state that shows an example row and where to turn it on.
- A workload with a project still shows its records in the project's logs.
  One without a project (a standalone sandbox) records too; its records are
  read from its own page.

## Decisions

1. **Entry point:** a separate egress gateway container per node, built
   from the preview gateway's image with its own entrypoint. *Decided.*
2. **Workloads without a project:** they record. Records are owned by the
   workload (kind and id), and by its project when it has one. This
   replaces the first default, which refused to record a projectless
   sandbox. *Decided, built.*
3. **Order of work:** storage and the activity view, then the egress
   gateway, then retiring the sidecar. *Decided; the first is built.*
4. **Sandboxes and environments stay separate entities, and recording is
   written once.** The flow, ingest, records, activity query, node-0
   recorder, API types and console components are shared
   (`temps-workload-egress`); each kind adds an adapter and thin handlers.
   *Decided, built: ADR-115 and
   `docs/architecture/workload-capabilities.md`.*
5. **Environments are steered with proxy variables first.** A replica of an
   environment that records starts with the node's proxy as its HTTP(S)
   proxy. It applies to containers started after it is turned on, records
   only clients that honour the variables, sends only public destinations
   through the proxy, and makes the environment's outbound HTTP(S) depend
   on the proxy being up. Redirecting at the network layer (ADR-055 §9)
   replaces it. *Decided, built for replicas on the control plane.*
6. **The egress gateway serves every kind.** When it is built, it is the
   entry point of the same per-node proxy and table, not a second recorder:
   a kind's traffic source says how its workloads reach it.

## Open

- Whether "change ports" (section 5) means the gateway's own ports, as
  assumed, or the allowed destination ports.
- Worker nodes do not serve the proxy yet, so they record nothing for any
  kind. Their status says so.

## Not covered here

- Each sandbox network takes a whole `/16` from Docker's default address
  pools. That caps a node at about 30 sandboxes, whatever the memory. The
  ADR's other candidate, one shared internal network with nftables
  isolation, would remove that limit. It is a separate change.
- TLS interception: still none. The proxy sees only the CONNECT host and
  port for HTTPS.
