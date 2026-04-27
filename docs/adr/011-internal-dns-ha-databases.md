# ADR-011: Internal DNS for HA Database Resolution

**Status:** Accepted
**Date:** 2026-04-27
**Author:** David Viejo

## Context

ADR-superseding network work in `feat/multi-host-network` gave Temps an L3 overlay
between worker nodes: `temps-network` allocates a `/24` per node out of a `/16`
pool, attaches each container to a `temps-overlay` Docker bridge, and routes
inter-node traffic over a VXLAN underlay (or native routes). Containers on
different hosts can now reach each other by IP.

What is missing is **L7 service identity** — a stable name that resolves to the
right container IP from anywhere in the cluster. Three concrete failures fall
out of this gap today:

1. **`service_members.hostname` is never populated.**
   `crates/temps-providers/src/externalsvc/postgres_cluster.rs::cluster_connection_string`
   builds a libpq multi-host string from `member.hostname`, then falls back to
   the container name when `hostname` is `NULL` (which is always). Container
   names only resolve on the host that runs them, so any 2-node Postgres
   cluster is broken end-to-end.

2. **The proxy routes to NAT'd host ports, not container IPs.**
   `crates/temps-routes/src/route_table.rs` resolves remote backends to
   `node.private_address:host_port`. We have an L3 overlay; we should be able
   to reach `container_ip:container_port` directly without traversing host
   port mappings.

3. **No internal DNS.** `temps-dns` today is purely an *external* DNS-provider
   abstraction (Cloudflare / Route 53 / DigitalOcean / Azure / Namecheap).
   There is no in-cluster resolver. The only way for an app container to find
   its database is to be handed a pre-resolved IP via env var, which means
   *rotating a Postgres replica forces redeploying every consumer*. That's not
   HA; that's a kill switch with an extra step.

The first version of this work targets **databases only** (Postgres clusters
specifically, with Redis Sentinel and MongoDB ReplicaSet as fast-follows).
Service-to-service application discovery, DNSSEC, and IPv6 are explicitly out
of scope for this phase.

## Decision

### 1. Three-tier service identity

Three tiers, each owns one job. Don't merge them.

```
Tier 3: Service identity   — pg-orders.temps.local
Tier 2: Container identity — pg-orders-0.pg-orders.temps.local
Tier 1: L3 reachability    — 172.20.5.42 (compute_cidr, already done)
```

Tier 1 is `temps-network` (already shipped). This ADR adds Tier 2 (container
records, written by lifecycle hooks) and Tier 3 (role-aliased records, written
by a per-cluster reconciler). Both live in the same `service_endpoints` table —
the distinction is in `owner_kind`, not in separate tables.

### 2. Per-node Hickory resolver, embedded in `temps-agent`

The DNS data plane runs **per node**, not centrally. A new crate
(`temps-dns-resolver`, added in step 2 of the rollout — not this ADR's
schema-only step 1) embeds [Hickory DNS](https://hickory-dns.org) as a Tokio
task inside `temps-agent`.

It listens on **two** sockets:

- **`<bridge_gateway>:53`** (e.g. `172.20.5.1:53`) — the resolver every
  container on this node sees. Containers attached to `temps-overlay` are
  given the bridge gateway as their first nameserver via Docker's `--dns`
  flag, set by the deployer at attach time.
- **`127.0.0.53:53`** — host-local resolver, so the agent itself and ops can
  `dig pg-orders.temps.local @127.0.0.53` for debugging.

Why per-node, not central:

- **Blast radius.** A central resolver is a cluster-wide SPOF. Per-node
  resolvers fail independently; a node losing its resolver only loses its own
  containers' name resolution, not the cluster's.
- **Latency.** Localhost DNS responds in <1 ms; central DNS over the underlay
  inherits the overlay's tail latency on every lookup.
- **Authority is local.** Each node already knows what containers it hosts;
  resolving a local name doesn't require a network hop.

Each node serves the **full zone**, not just its own records. Cost is trivial
(few KB of records for a small cluster) and it means containers can be given a
secondary nameserver pointing at a peer node for DNS HA.

### 3. Centralised authoritative state in PostgreSQL

The single source of truth is two tables on the control-plane database:

```sql
-- Authoritative records, written by lifecycle hooks and reconcilers.
CREATE TABLE service_endpoints (
    id              BIGSERIAL PRIMARY KEY,
    fqdn            TEXT      NOT NULL,
    record_type     TEXT      NOT NULL,         -- 'A' | 'AAAA' | 'SRV' | 'CNAME'
    target_ip       TEXT,                        -- v4 or v6 string; parsed at boundary
    target_port     INTEGER,                     -- nullable except for SRV
    ttl             INTEGER   NOT NULL DEFAULT 30,
    owner_kind      TEXT      NOT NULL,         -- 'service_member' | 'service_role' | 'node' | 'static'
    owner_id        BIGINT    NOT NULL,         -- FK *interpretation* depends on owner_kind
    node_id         INTEGER   REFERENCES nodes(id) ON DELETE SET NULL,
    generation      BIGINT    NOT NULL,          -- monotonic, bumped on every mutation
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT service_endpoints_record_type_valid
        CHECK (record_type IN ('A', 'AAAA', 'SRV', 'CNAME')),
    CONSTRAINT service_endpoints_owner_kind_valid
        CHECK (owner_kind IN ('service_member', 'service_role', 'node', 'static'))
);
CREATE UNIQUE INDEX service_endpoints_uniq
    ON service_endpoints (fqdn, record_type, target_ip);
CREATE INDEX service_endpoints_generation_idx
    ON service_endpoints (generation);

-- Per-node resolver applied-state, so we can see drift.
CREATE TABLE node_dns_state (
    node_id             INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    applied_generation  BIGINT NOT NULL DEFAULT 0,
    last_sync_at        TIMESTAMPTZ,
    health              TEXT NOT NULL DEFAULT 'unknown',
    CONSTRAINT node_dns_state_health_valid
        CHECK (health IN ('healthy', 'degraded', 'stale', 'unknown'))
);
```

`generation` is a monotonic counter bumped on every mutation. Agents long-poll
`GET /internal/nodes/{node_id}/dns/changes?since=N` and receive only the diff
since their last applied generation; they ACK by writing `applied_generation`
back. If the gap is too large or `since=0`, the server responds with
`full_snapshot: true` and the entire zone.

`target_ip TEXT` (not Postgres `inet`) is deliberate: it matches the existing
`nodes.private_address` / `nodes.compute_cidr` storage pattern and keeps a
single column path for both v4 and v6. The Rust layer parses to `IpAddr` at
the boundary.

### 4. Naming scheme

Boring conventions. Everything under `.temps.local`.

| What                      | FQDN pattern                                       | Example                                       |
| ------------------------- | -------------------------------------------------- | --------------------------------------------- |
| Service member (specific) | `<svc>-<ordinal>.<svc>.temps.local`                | `pg-orders-0.pg-orders.temps.local`           |
| Service VIP (any healthy) | `<svc>.temps.local`                                | `pg-orders.temps.local` (multi-A round-robin) |
| Service primary (writes)  | `primary.<svc>.temps.local`                        | `primary.pg-orders.temps.local`               |
| Service replicas (reads)  | `replica.<svc>.temps.local`                        | `replica.pg-orders.temps.local`               |
| Node                      | `<node-name>.nodes.temps.local`                    | `worker-1.nodes.temps.local`                  |

This kills the worst pattern in the current code — the `member.hostname` field
that's "WireGuard IP or DNS name or container name maybe". After this work,
`member.hostname` is **always** an FQDN, resolved at use-time.

### 5. Postgres reconciler runs on the control plane

A 5-second-tick reconciler lives in `temps-providers::externalsvc::postgres_cluster`
and runs on the **control plane** (not on the node hosting the monitor).
Reasons:

- One reconciler per cluster, simple ownership.
- Reconciler doesn't move when the monitor moves.
- Monitor address is already in `service_members`, so reachability is the
  same either way.

The reconciler queries `pg_auto_failover.pgautofailover.node` for each cluster,
and upserts:

- `primary.<svc>.temps.local` → A record for current primary, **TTL 5 s**.
- `replica.<svc>.temps.local` → multi-A for healthy secondaries, **TTL 30 s**.
- `<svc>.temps.local` → multi-A for all healthy data members.

Failover path: monitor promotes secondary → reconciler observes within 5 s →
DNS records flip → consumer's next libpq connection (or retry triggered by
`target_session_attrs=read-write`) lands on the new primary. Tunable,
deterministic, observable.

### 6. Connection string simplification

`cluster_connection_string()` becomes one line:

```rust
format!(
    "postgresql://{u}:{p}@{svc}.temps.local:{port}/{db}?target_session_attrs=read-write",
    svc = service.slug,
)
```

No more `members.iter().map(...).join(",")`. The DNS layer handles host
enumeration. The same string works on every node, on every host, forever —
even after replicas are added or removed.

This is the load-bearing simplification. Everything else in this ADR is
plumbing to make it true.

### 7. Hardening

| Failure mode                 | Mitigation                                                                                                                                                                                  |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Local resolver dies          | Containers are configured with two nameservers: local bridge gateway + a peer node's bridge gateway. Each node serves the **full** zone, not just its own.                                  |
| Control plane unreachable    | Resolvers serve last-known state from on-disk snapshot at `/var/lib/temps/dns/zone.json`, fsynced on each generation bump. Stale-but-serving > NXDOMAIN. `node_dns_state.health = 'stale'`. |
| DNS poisoning / spoofing     | Resolver binds to `<bridge_gateway>` and `127.0.0.53` only — never `0.0.0.0`. Containers cannot reach the control-plane sync API directly; agent calls it on their behalf with node token.  |
| Split brain on failover      | `pg_auto_failover` monitor is single-writer; reconciler reads, never decides. If monitor is down, `primary.*` is *not* updated — apps see stale primary, fail writes, surface the incident. |
| Generation skew / lost ACKs  | Monotonic `generation` + ACK-back-the-applied-version makes drift visible. Ops dashboard query: `SELECT node_id FROM node_dns_state WHERE last_sync_at < now() - interval '60 seconds'`.    |
| Record GC on container stop  | Container-stop lifecycle hook calls `delete_by_owner(service_member, member_id)`. Hourly janitor reconciles `service_endpoints` against `service_members` + `containers` and removes orphans. |
| App-side DNS caching         | Aggressively low TTLs (5 s primaries, 30 s replicas, 300 s static). libpq doesn't cache; JVM apps do — documented requirement to set `networkaddress.cache.ttl=10`.                         |

## Consequences

### Positive

- **Failover becomes a database problem, not a redeploy problem.** Apps never
  hold IPs; they hold FQDNs. Replica added/removed/promoted = DNS records
  flip = next connection lands correctly.
- **Multi-host clusters actually work.** The `member.hostname` regression in
  `postgres_cluster.rs` becomes structurally impossible: hostnames are now
  always populated by the lifecycle hook, and always FQDNs.
- **Same machinery for Redis Sentinel and Mongo ReplicaSet.** Both expect a
  list of seed hosts and discover topology themselves. Give them
  `<svc>.temps.local` as seed; the resolver returns multi-A; they figure out
  the rest. **No service-specific code in the DNS layer.**
- **Connection strings stop encoding topology.** The libpq multi-host
  workaround is replaced with a single FQDN; topology lives in DNS.
- **Per-node DNS is an isolation win.** A control-plane outage doesn't break
  resolution; only mutations stop. Existing connections keep working.

### Negative

- **One more moving part on every node.** Hickory in-process is small (~3 MB
  RSS), but it's another thing that can fail. Mitigated by host-local
  fallback nameserver and on-disk snapshot.
- **Schema growth.** `service_endpoints` will be the largest "config-shaped"
  table for clusters with many services and replicas. With aggressive GC and
  `BIGINT` keys, growth is bounded — but plan for `VACUUM` tuning.
- **Eventual consistency window.** A 5-second tick + 30 s long-poll means
  worst-case ~6 s from primary promotion to DNS flip. Acceptable for HA
  databases (apps will retry); documented limit.
- **Container DNS caching is out of our control.** Apps that aggressively
  cache DNS (JVM defaults: `networkaddress.cache.ttl=-1`) need configuration.
  We document this; we do not enforce it.

## Alternatives considered

- **Use Docker's embedded DNS.** Rejected: only resolves on the same host;
  doesn't know about the overlay; we lose control of TTLs and record types
  (no SRV, no AAAA, no per-record overrides).
- **Service mesh (Linkerd, Consul Connect, etc.).** Rejected: yak-shave that
  adds 10 ms p99 to every internal call, plus mTLS termination overhead, in
  exchange for features we don't need (the WireGuard underlay already
  encrypts node-to-node). PaaS scope, not microservice scope.
- **VIP / IPVS / kube-proxy-style L4 load balancing.** Rejected: stateful
  database connections don't multiplex; libpq's `target_session_attrs` does
  the right thing with multi-A. L4 LBs add a hop and a failure domain.
- **Centralised resolver running on the control plane.** Rejected: SPOF,
  cross-region latency, complicates the underlay's failure-isolation story.
- **`coredns` running per node.** Rejected: external process, harder to
  embed cleanly into `temps-agent`'s lifecycle, and Hickory gives us
  Rust-native error handling and the same on-the-wire protocol with no
  separate binary to ship.
- **Push (control plane → agent) instead of long-poll.** Rejected for now:
  long-poll piggybacks on existing internal API patterns (`peer_list`,
  `route_table` reload), and a second push channel is not worth the
  complexity for sub-minute reconciliation. Re-evaluate if we ever need
  sub-second propagation.
- **Synthesise records on the fly from `service_members`.** Rejected:
  reconciler needs a place to write health-derived records (`primary.*` is
  not a column on `service_members`). Materialising them gives one obvious
  query path for the resolver and one obvious diff for sync.

## Scope

This ADR covers **database resolution only**. Out of scope for this phase:

- **Service-to-service app names** (`api.shop.apps.temps.local`). The
  schema and resolver support it for free; lifecycle hooks for user
  deployments are deferred until needed.
- **DNSSEC.** Internal-only zones, authenticated control plane, low value.
  Not implemented.
- **IPv6 overlay/underlay.** The schema column is plain text and Hickory
  serves AAAA records when present, so the day v6 ships in `temps-network`,
  records "just appear" with no migration. But no v6-specific work in this
  ADR.
- **Redis / Mongo reconcilers.** The framework is generic; the per-engine
  reconcilers land as fast-follows once the Postgres path is proven.
