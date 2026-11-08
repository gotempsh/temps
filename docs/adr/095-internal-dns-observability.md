# ADR-095: Internal DNS observability: resolver health, records, query log, traffic and alerts

## Status

Proposed — 2026-11-08

Related: ADR-011 (internal DNS and the per-node resolver), ADR-024 (the
control-plane resolver), ADR-025 (unified alarm log), ADR-055 §9–§10 (DNS
policy for restricted workloads, environment names), ADR-061 (worker metrics,
pulled by the control plane).

## Context

Every node runs a resolver for the internal `*.temps.local` zone (ADR-011).
On a worker it is embedded in `temps agent`, listens on the node's bridge
gateway (and `127.0.0.53`), long-polls the control plane's change feed
(`GET /internal/nodes/{id}/dns/changes`) and ACKs what it applied. On the
control plane it runs in the proxy process (`temps proxy`, or combined
`temps serve`), on the `temps-app-network` gateway, fed directly from
`service_endpoints` (ADR-024). Containers get the node's resolver as their
first nameserver through `HostConfig.dns`, layered behind Docker's embedded
`127.0.0.11`. Names are `<env>.<project>.temps.local` for applications
(owner `environment`), `<service>.temps.local` and `primary.` / `replica.`
for databases (owners `service_member`, `service_role`), and
`<node>.nodes.temps.local`. Outside-zone names are forwarded to the upstream
resolvers.

What an operator can see today is one card under Settings → Nodes →
Advanced: the cluster-DNS switch, and, per worker, the resolver fields its
heartbeat carries (`running`, `tasks_alive`, last sync, consecutive sync
failures, last error, record count), the zone's record count, name conflicts
and the applications deployed without the resolver
(`GET /cluster/dns/status`). Missing:

- **The control plane's own resolver** reports nothing. Node 0 is synthetic
  (it has no `nodes` row) and does not heartbeat.
- **What the zone contains.** No list of records, owners or nodes.
- **What was asked, and what was answered.** The resolver logs a `debug!`
  line per query and counts nothing. A container that gets NXDOMAIN or
  SERVFAIL leaves no trace an operator can find.
- **Traffic over time.** No counters, so no rates and no failure ratios.
- **A way to ask.** Checking "does `production.shop.temps.local` resolve on
  worker-2" needs SSH and `dig`.
- **Alerts.** A dead resolver, a node whose zone stopped syncing, or a burst
  of failures for a name that should exist reach nobody.

## Decision

### 1. Each resolver measures itself, off the hot path

`temps-node-dns` gains an observer (`observe::DnsObserver`) that every
`ZoneAuthority` records into: the control plane's and each worker's.

- **Counters.** Relaxed atomics, per zone class (`internal` for
  `*.temps.local`, `external` for forwarded names) and outcome: `noerror`
  (with answers), `nodata` (name exists, no record of that type), `nxdomain`,
  `servfail`, `refused` (a restricted workload asking for a name its network
  policy does not allow, answered NXDOMAIN, ADR-055 §9) and `error` (every
  other code). A latency sum and a fixed histogram (1, 5, 25, 100, 500 ms,
  more) per zone class. The resolver's own readiness probe
  (`_temps-resolver.temps.local`) is not counted.
- **Recent queries.** After the answer is sent, the authority offers one
  small entry to a bounded channel with `try_send`. A background task drains
  it into a ring of the **last 512 entries**, each with a sequence number. A
  full channel drops the entry and counts the drop. No lock, allocation or
  I/O is added to the query path beyond the one entry.
- **What an entry holds:** sequence, time, query name, type, zone class,
  outcome, latency, answer count and the source address. **Only
  `*.temps.local` names are recorded.** Forwarded names are what applications
  look up on the internet; they can carry identifiers (per-tenant hostnames,
  signed-URL hosts) and are none of the platform's business. A forwarded
  query is recorded only when it failed (SERVFAIL or another error), and then
  without its name. No answer data beyond the count is kept, and nothing
  about the query payload (EDNS options, flags) at all.

Load: a few relaxed atomic increments and one `try_send` per query. At
saturation the channel drops entries (counted) and the counters stay exact.
Memory is bounded at 512 entries per resolver (~100 KiB) plus the channel.

### 2. Workers serve a report; the control plane pulls it (ADR-061)

The agent serves `GET /agent/dns/report?after=<seq>&instance=<id>` behind
node-token auth, over the same channel `GET /agent/metrics` uses. The body is
a `DnsReport`: format version, a random per-resolver `instance_id`, the
binary's build version, whether the resolver runs and its tasks are alive,
the listen addresses, the zone generation and record count it serves, the
sync status, the cumulative counters and the entries after `after` (all of
them when `instance` is not the current one). Entries carry their age at
report time, not a timestamp.

The control plane pulls every active or draining worker on the monitoring
scrape interval and treats the report as untrusted, as ADR-061 does:

- at most 256 KiB read; names over 253 bytes, unknown outcomes or a
  version it does not read are refused;
- counters become deltas against the previous pull of the same
  `instance_id`; a new id (agent or resolver restart) starts a new baseline
  and writes nothing for that pull;
- entries are stamped with the control plane's receive time minus their
  age, clamped to the last pull window, so a worker cannot backdate or
  forward-date them.

An agent that predates the endpoint answers 404. That node's resolver shows
"agent too old to report queries"; its heartbeat health still shows.

### 3. The control plane's resolver reports through the database

The proxy process owns the control-plane resolver, and in a split
deployment the console is a different process. So the proxy's DNS bootstrap
loop, which already probes the resolver on every pass, ingests its own
resolver's report on the same interval through the same code the console
uses for workers. Node 0 then reads like any worker, in either topology.

That loop starts only where the control plane finds a container runtime,
so a control plane on which no containers run (a hosted plane, whose
workloads all run on workers) never reports. Node 0 without any report is
therefore shown as **no local resolver**, which is expected and never
alarms; once it has reported, a missing report is an outage again.

### 4. Where it is stored

- **Counters** go to the metrics store (`SourceKind::Node`, `source_id` =
  node id, 0 for the control plane) as counter deltas under fixed names:
  `dns.internal.{noerror,nodata,nxdomain,servfail,refused,error}`,
  `dns.external.{noerror,nxdomain,servfail,error}` and
  `dns.internal.latency_us`. Constant points per node per interval,
  whatever the query rate. Retention is the store's.
- **Resolver reports** go to a new table `dns_resolver_reports`, one row per
  node (node 0 included, no foreign key since node 0 has no row): when it
  was last reported, the report state (`ok`, `agent_too_old`, `unreachable`,
  `rejected`, `invalid`), build version, whether it runs, listen address,
  zone generation and record count served, and the last error.
- **Query log** goes to a new table `dns_query_log`. Each entry's source
  address is attributed **when it is stored**, with one query per batch: the
  `service_endpoints` row on that node whose target is that address names the
  environment (and so project) or database whose container asked. An address
  that matches nothing is kept as an address. **Retention is 24 hours and
  at most 50,000 rows**, enforced by a prune every 5 minutes. At the
  per-pull bound of 512 entries per node, a busy node fills its share
  quickly; the counters, not the log, are the record of volume.

### 5. What the console shows

A **DNS** page in Settings (`/settings/dns`), with the cluster-DNS switch
on it as well (it stays under Nodes → Advanced, where existing links
point):

- **Resolvers:** one row per node, control plane first: state (healthy,
  stale, down, off, node unavailable, agent too old, not reporting, or for
  the control plane no local resolver) and
  why, version, records served against the zone's, last sync and failed
  syncs in a row, listen address, last report, and **internal routing**:
  whether the worker's internal edge proxy listens on its bridge address
  (port 80), from the heartbeat's `capacity.internal_proxy`, with the bind
  error when it does not. Every `<env>.<project>.temps.local` name points
  there, so a healthy resolver in front of a proxy that cannot bind (another
  program holds the port) resolves names whose every call fails; the page
  says so instead of showing the node healthy. The agent retries the bind
  with backoff rather than giving up after one attempt.
- **Traffic:** one node at a time, over 1 h, 6 h, 24 h or 7 d: internal
  queries by outcome, the NXDOMAIN rate on its own chart, and totals
  (queries, failure rate, NXDOMAIN rate, forwarded queries).
- **Query log:** time, node, name, type, outcome, latency and the
  project/environment or database that asked, filterable by node and
  outcome, paginated.
- **Records:** the zone as served: name, type, target, owner (project and
  environment, database, node), node, TTL and last update, searchable,
  paginated.
- **Test resolution:** pick a node, optionally a project, type a name and
  record type, and see the answer, response code and latency from that
  node's resolver. With a project, the query is evaluated as that project's
  container on that node would be answered, network policy included; on the
  control plane in a split deployment, where the resolver is in another
  process, the console asks the resolver over the network and says it is
  not answering as the project.
- **Alerts:** whether a DNS alarm is firing, with a link to the
  notification settings, which onboard when no channel is configured.

Endpoints (all `SettingsRead`, like `GET /cluster/dns/status`):
`GET /cluster/dns/resolvers`, `GET /cluster/dns/records`,
`GET /cluster/dns/queries`, `GET /cluster/dns/traffic`,
`POST /cluster/dns/resolve`. Lists are paginated (default 20, max 100,
newest first).

### 6. Alerts through the alarm log

The console evaluates DNS health after each pull and fires alarms through
`AlarmService`, so they reach the user's notification channels and the
alarm log, are scoped to the node (`metadata.node_id`), and resolve on their
own when the condition clears. Only while cluster DNS is on:

- **`dns_resolver_down`** (critical): an active node whose resolver is not
  running, or whose tasks died, on two evaluations in a row; for node 0,
  also when its report is older than three intervals (a control plane that
  never reported runs no resolver and raises nothing, §3).
- **`dns_sync_stale`** (warning): a worker whose last successful sync is
  older than 2 minutes, or that failed 5 syncs in a row.
- **`dns_query_failures`** (warning): over the last 5 minutes on one node,
  at least 10 SERVFAIL answers for `*.temps.local` names making up at least
  5% of them, or at least 5 NXDOMAIN answers for names that should exist: a
  name in the zone, or one the control plane would publish for an existing
  environment (`<env>.<project>.temps.local`) or database
  (`<service>.temps.local`, `primary.`/`replica.`). A typo resolves nowhere
  and alerts nobody. The message names the top failing names.

- **`dns_internal_proxy_down`** (critical): an active worker whose
  internal edge proxy reports not listening on two evaluations in a row.
  Not raised for an agent that does not report it (unknown is not down).

The thresholds are constants in this version.

## Backwards compatibility

- Additive. A new agent against an old control plane serves a report
  nobody pulls. An old agent against a new control plane answers 404 and is
  shown as too old to report; its heartbeat health is unchanged.
- No change to how names are published, synced or answered. Turning
  cluster DNS off stops the resolvers as before; their rows show "off".
- Two new tables, no change to existing ones.

## Consequences

- Operators see the resolver on every node, the zone, recent internal
  lookups, traffic, and can test a name from a node, without SSH.
- Failures reach the user's channels instead of a debug log.
- The query log records which container asked for which internal name for
  24 hours. That is an operational record of the cluster's own names, kept
  short and bounded, and readable only by instance administrators.
- Forwarded names are never stored, so the log cannot answer "what did this
  app look up on the internet". That is deliberate.
- A busy node's log shows the most recent queries only; rates come from the
  counters.

## Alternatives considered

- **Push from the agent in its heartbeat.** Rejected: the heartbeat is a
  liveness signal, and query entries would grow it without bound; ADR-061
  already settled on pull for worker telemetry.
- **Log every forwarded query.** Rejected for privacy and volume.
- **Keep the log only in the resolvers' memory and fan out on read.**
  Simpler, but the log would vanish with a worker that is down, which is when
  it is needed, and node 0 in a split deployment would need a second
  channel.
- **A per-query row in TimescaleDB or ClickHouse.** Rejected: per-query
  writes, no bound. Counters in the metrics store give the rates at constant
  cost.
