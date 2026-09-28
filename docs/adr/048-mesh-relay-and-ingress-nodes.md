# ADR 048: Mesh Relay and Ingress Nodes

- **Status:** Proposed
- **Date:** 2026-09-28
- **Builds on:** ADR-017 (split proxy/console), ADR-020 (multi-node hardening), the managed WireGuard mesh (`crates/temps-wireguard/src/mesh.rs`, `crates/temps-network/src/mesh.rs`)

## Context

A cluster is a control plane (`temps serve`) plus worker nodes (`temps agent`). The managed WireGuard mesh lets nodes that only share the internet join: every node gets a private mesh address, the VXLAN overlay runs over it, and the control plane reaches a publicly-joined node's agent and published ports on that address (`nodes::Model::data_address`).

Two limits remain, and a third question sits next to them.

### 1. Every member must be directly reachable over UDP

The control plane already exchanges WireGuard keys: each agent registers its public key and endpoint (`PUT /internal/nodes/{id}/network/wireguard`), the control plane assigns mesh addresses and hands every node its peers (`GET /internal/nodes/{id}/network/peers`). Key exchange is solved. What is not:

- **A control plane without a public address.** On a laptop or a home server the Worker Nodes page tells a remote machine to `temps join https://app.localho.st …`, a URL that resolves to the worker itself. Workers need the control-plane API to join, sync routes, fetch peers and heartbeat; none of it works.
- **Nodes behind NAT.** A mesh endpoint is an `ip:port` other members dial. A node behind NAT or CGNAT has none, so it cannot join the mesh (`docs/features/multi-node/page.mdx`: "nodes behind NAT are not supported yet").

`temps join --relay-url` (`crates/temps-cli/src/commands/join.rs`) is a client for a relay whose server exists nowhere. It swaps keys over `POST {relay}/api/relay/clusters/{id}/join`, sets up a separate `wg0` with `10.100.0.x`, and still assumes direct reachability. It predates the managed mesh and solves the part that is already solved.

### 2. The control plane carries all application traffic

The control-plane proxy (Pingora, `crates/temps-proxy`) serves every domain, always. `temps serve --role console` moves the listeners into a separate `temps proxy` process, which needs direct Postgres access and the encryption key (`crates/temps-cli/src/commands/proxy.rs`); `--profile control-plane` stops local workloads but still proxies. There is no mode in which the control plane carries no application traffic.

Temps Cloud hosts each customer's control plane and customers join their own machines as workers. Cloud must not carry customers' end-user traffic: it is the dominant cost, and it would make every customer's site depend on Cloud's network.

Two partial mechanisms exist:

- **Worker public ingress** (`crates/temps-agent/src/public_ingress.rs`, `internal_proxy.rs`, `route_store.rs`): a small hyper/rustls listener in `temps agent` on `--public-ingress-address`. It long-polls the route snapshot (`crates/temps-routes/src/route_sync.rs`), receives exact-host certificates encrypted to the node's key, relays HTTP-01 challenges to the control plane and forwards to `<node address>:<published port>`. Its limits:
  - Only plain upstream routes qualify. Redirects, wake-on-request, attack mode, per-project security, static sites and wildcard hosts stay on the control plane (`route_table.rs` `snapshot_worker_public_routes`).
  - Global rate limiting, global security headers, a single IP access rule anywhere, or a custom request-policy gate withholds **every** route from **every** worker (`route_sync.rs`).
  - Routes lapse 300 s after the last successful sync (`route_store.rs`), so a control-plane outage longer than five minutes takes down every site served by workers.
  - DNS is manual.
- **`temps-edge`** (`crates/temps-edge`): a caching Pingora CDN node. Every request it does not serve from cache goes to one origin, the control plane (`proxy.rs` `upstream_peer`); its routes are domain flags without backends. It adds a hop in front of the control plane rather than removing it. Registration fails with default settings (unspecified `api_address` as `private_address`, no CSR when mTLS is required), and `role = "edge"` nodes are not excluded from scheduling (`node_scheduler.rs`).

For comparison, OpenShip runs one OpenResty per server that proxies only to that server's own apps, has the user point an A record at the server, and issues certificates with certbot per server. Its hosted edge carries traffic only for free `*.opsh.io` URLs, and it supports neither multi-node routing nor servers without a public IP.

## Decision

### D1. One relay primitive, built into the `temps` binary

Everything missing in (1) is "forward bytes between two members of the same cluster that cannot reach each other". The relay provides exactly that and nothing else:

- Members keep **one outbound connection** to a relay: TLS on TCP 443 (WebSocket in the first version, QUIC later), so it passes any firewall that allows HTTPS out.
- The connection multiplexes two channel kinds:
  - **`api`**: a byte stream from a node to the control plane's HTTPS API. TLS runs end to end between node and control plane *inside* the channel; the relay forwards ciphertext.
  - **`wg`**: WireGuard datagrams between two members. WireGuard encrypts them end to end; the relay forwards ciphertext.
- The relay never generates, stores or distributes keys, never assigns addresses and never admits nodes. The control plane keeps all of that.

The relay is a mode of the existing binary, not a new product:

- **A control plane with a public address is its cluster's relay.** `temps serve` accepts relay connections from its own members on its existing HTTPS listener. Every Temps Cloud control plane is public, so every Cloud tenant gets relaying for NAT'd workers with no extra service.
- **`temps relay`** runs the same code as a standalone, multi-cluster relay. Temps Cloud operates one for control planes *without* a public address (laptops, home servers); anyone can self-host it. A control plane opts in with `temps serve --relay <url>` (or the equivalent setting), keeps an outbound connection to it, and exposes its API through it.

### D2. WireGuard over the relay with kernel WireGuard

The mesh uses kernel WireGuard, which sends UDP to a peer endpoint and cannot speak to a relay itself. When a peer is relayed, the agent (or `temps serve`) runs a local UDP forwarder bound to `127.0.0.1:<port>` for that peer and sets the peer's endpoint to it. The forwarder wraps each datagram into the peer's `wg` channel and unwraps the reverse direction.

- **Path choice.** Direct first, as today. A peer with no handshake for 30 s while the relay is available switches to relayed; a relayed peer retries direct every few minutes and switches back after a direct handshake. The existing roaming rule (keep a live-handshake endpoint, `LIVE_HANDSHAKE`) prevents flapping.
- **MTU.** The relay path adds the channel framing (under 40 bytes) on top of TCP/TLS, which segments freely, so the mesh MTU (`mesh_mtu_for`) does not change. Throughput is bounded by one TCP stream per peer pair: fine for control traffic and modest service traffic, not for bulk data. The UI marks relayed peers, and the docs say to open UDP when throughput matters.
- **NAT hole punching** is a later optimisation. Once members report the public `ip:port` the relay observes, peers can try simultaneous direct handshakes before falling back. It is not needed for correctness.

### D3. Reaching a control plane without a public address

`temps join` accepts a relay join string:

```
temps join relay://<cluster-id>@<relay-host>#<ca-fingerprint> <join-token> --private-address <ip>
```

The Worker Nodes page renders it whenever the control plane is relay-connected. The agent opens an `api` channel through the relay and speaks HTTPS to the control plane inside it, verifying the control plane against the cluster CA fingerprint carried in the join string. Trust is anchored in the join secret, not in the relay's certificate, so a relay (including Temps Cloud's) cannot impersonate the control plane.

Once a node is on the mesh, its API traffic moves to the control plane's mesh address. From then on the relay carries only WireGuard ciphertext for that node, plus reconnection when the mesh is down.

### D4. Ingress is a node capability; the control-plane proxy can be switched off

- **Any node can take public traffic.** The existing public-ingress toggle becomes the "ingress" capability of a node. An ingress node forwards to containers on any node over the mesh or the private network, so DNS may point at any healthy ingress node, not only the one running the app (ADR-020: every node accepts ingress for any domain). A **dedicated edge** is an ingress node that runs no workloads; the scheduler excludes it, and `role = "edge"` today.
- **The mesh allows members to reach published ports.** The mesh firewall (`render_mesh_lockdown`) accepts DNAT'd traffic from the whole mesh pool, not only the control plane, and still drops everything else arriving over the mesh.
- **Control-plane proxy mode: `full` (default) or `off`.** With `off`, the control plane serves only its own console/API host and the endpoints ingress nodes depend on (route sync, the ACME HTTP-01 relay). Any other host gets a short page naming where the app is served. Temps Cloud runs every hosted control plane with `off`.
- **The relay never carries end-user traffic by default.** An ingress node needs a public address. Serving apps from a NAT'd node through a Cloud tunnel is a separate, opt-in, metered offering.

### D5. Ingress feature parity by reusing the proxy engine

Ingress nodes run the same Pingora engine as the control-plane proxy, fed by the route-sync snapshot instead of Postgres. Customer machines never hold database credentials. The snapshot grows the policy data the engine needs:

- redirects and force-HTTPS;
- security headers and per-project security settings;
- rate limits, counted per ingress node. This changes semantics from cluster-wide and must be documented;
- IP rules, applied to the real client address;
- attack mode;
- the request-policy gate;
- wake-on-request, by asking the owning node's agent;
- static sites.

This replaces the agent's small listener.

Until a feature has an ingress implementation, a cluster whose control-plane proxy is `off` refuses to enable it and says why ("not available while apps are served by ingress nodes"), rather than dropping the route.

### D6. Ingress keeps serving when the control plane is unreachable

Ingress routes and certificates are **fail-static**: an ingress node that cannot reach the control plane keeps its last snapshot. It still drops everything on an explicit rejection (401/403/404: the node was removed or revoked). Certificates keep working until they expire; renewal needs the control plane.

### D7. DNS and certificates for ingress nodes

- A node records its public ingress address.
- **Self-hosted:** the operator points DNS at one or more ingress addresses, and the UI lists them.
- **Temps Cloud:** managed DNS maintains a per-tenant name (e.g. `ingress.<tenant>.<cloud-domain>`) that points at the tenant's healthy ingress nodes; customers CNAME their domains to it (apex domains need A records or provider flattening).
- A tenant-scoped wildcard certificate may be exported **only** to that tenant's own ingress nodes, which makes generated hostnames work on ingress nodes. Wildcards remain unexported otherwise.

### D8. Retire what this replaces

- Remove the `--relay-url` join mode and `temps_wireguard::WireGuardManager`'s `wg0`/`10.100.0.x` path once relay joins ship.
- `temps-edge`: exclude `role = "edge"` from scheduling now. Fold its cache into the ingress engine later instead of fixing its registration, and document it as experimental until then.

## Alternatives considered

| Option | Why not (now) |
|---|---|
| Keep requiring public addresses (status quo) | Excludes laptops, home servers and NAT'd VPSes; the Worker Nodes page already shows unreachable join commands. |
| Embed Tailscale-style coordination + DERP (Headscale, BSD-3) | Headscale replaces what the control plane already does (keys, addresses, ACLs); DERP expects userspace WireGuard with magicsock. Two control planes to keep consistent. The forwarding idea is the same one D2 uses. |
| NetBird / Nebula | Full mesh products with their own management plane or certificate authority; same duplication, plus another agent on every node. |
| TURN/STUN servers | Covers datagrams only, not the control-plane API tunnel, and adds its own credential scheme. |
| Cloudflare Tunnel / ngrok for the control-plane API | Solves D3 only, ties self-hosters to a third-party account, terminates TLS at the provider, and does nothing for NAT'd workers. |
| A separate relay product/service | Two deployables where one suffices; a public control plane already has everything a relay needs. |
| Extend `temps-edge` as the ingress | Single-origin design, no backends, no policy enforcement, broken registration; worker ingress already has backends, certificate delivery and the ACME relay. |
| Cloud-run edge fleet in front of workers | Cloud carries all customer traffic, the thing this ADR avoids. Kept only as a paid add-on (CDN, NAT'd ingress). |
| Each node serves only its own apps (OpenShip) | Simpler, but DNS must follow every redeploy and scale-out; loses cross-node ingress for no security gain inside one cluster. |

## Security

- **What a relay sees.** Member addresses, connection times, which members talk, and volumes. Never plaintext: WireGuard and the end-to-end TLS in `api` channels both hide it. A compromised relay can drop or delay traffic, not read or forge it.
- **Admission.** A member authenticates to the relay with a short-lived relay credential minted by the control plane: cluster id, member id, expiry, signed with a per-cluster key the relay learns when the control plane registers. The relay routes channels only between members of the same cluster and cannot mint credentials or admit nodes. Node admission stays with the join token and the control plane.
- **Control-plane identity.** Pinned through the cluster CA fingerprint in the join string (D3), never through the relay's certificate.
- **Shared relay abuse.** Per-cluster connection and bandwidth quotas, per-channel rate limits, and metering. The relay refuses `wg` channels between members that are not both connected and admitted.
- **Mesh firewall change (D4).** A compromised node can reach other nodes' published app ports over the mesh. That already holds on a shared private network, which ADR-020 accepts; host services, the agent API and everything else stay unreachable over the mesh.
- **Ingress nodes** hold private keys for the certificates of the hosts they serve and see that traffic in plaintext: the same trust as a worker of that tenant. Cloud never exports one tenant's material to another tenant's nodes.
- **Fail-static (D6)** keeps a revoked node serving until its revocation reaches it; removal still takes effect on the node's next contact with the control plane.

## UX and onboarding

- The Worker Nodes page checks whether its join URL is reachable from other machines:
  - A loopback, `*.localho.st` or private URL gets a warning on the internet path.
  - Where a relay is configured, it shows the relay join string instead.
  - Where none is configured, it offers "Connect this server to a relay", with the Temps Cloud relay pre-filled and self-hosting documented.
- A capability endpoint reports relay state: `configured: false` + reason + setup path, so the UI and CLI tell "not set up" apart from "not built".
- Relayed peers show as **Relayed** in the Mesh column, with a hint to open UDP for direct connections.
- Nodes gain an **Ingress** badge with their public address. With the proxy `off`, the settings page lists which features are unavailable and why.
- CLI parity lives in `bunx @temps-sdk/cli` (`nodes mesh`, `nodes ingress`, relay status). `temps relay` is a server lifecycle command in the Rust binary.

## Rollout

| Phase | Scope | Unlocks |
|---|---|---|
| P0 (current branch) | Remote backends use `data_address()`; the mesh firewall admits mesh members to published ports; e2e: a publicly-joined worker serving ingress for an app on another worker | Ingress over the mesh works |
| P1 | Control-plane proxy `off` mode; ingress capability + scheduling exclusion; guardrails for unsupported features; fail-static lease | Temps Cloud without carrying app traffic (public workers) |
| P2 | Relay in `temps serve`: `wg` channels, local forwarder, direct/relayed path choice | NAT'd workers on Cloud and on any public control plane |
| P3 | `temps relay` standalone + `api` channels + relay join strings; Temps Cloud shared relay | Laptop/home control planes |
| P4 | Pingora engine on ingress nodes, snapshot policy data | Full feature parity; guardrails removed |
| P5 | Cloud managed ingress DNS, tenant wildcard certificates; hole punching; edge cache | Zero-touch DNS, faster paths |

## Testing

- **DinD cluster** (`tools/dev-cluster`): put one worker on a "public" underlay (e.g. `203.0.113.0/24`, which `is_private_node_address` treats as public), join it with that address, enable the mesh and ingress, deploy an app on another worker, then:
  - `curl --resolve` the host against the ingress node;
  - assert the control-plane proxy logged no request for it.
- **NAT simulation:** put a worker behind a network namespace with `MASQUERADE` and no inbound UDP. Assert it joins through the relay and appears as **Relayed**; then remove the NAT and assert it switches to direct.
- **Relay isolation:** two clusters on one relay; a member of one cannot open a channel to the other.
- **Proxy `off`:** only the console host answers on the control plane; route sync and the ACME relay keep working.

## Open questions

1. Relay credentials: signed tokens verified by the relay (preferred, stateless) or a registration API on the relay?
2. Should a public control plane advertise itself as the default relay for its members, making P2 zero-configuration?
3. Rate-limit semantics with several ingress nodes: per-node limits (simple) or a shared budget (needs coordination)?
4. Pricing and quotas for the Temps Cloud shared relay and for NAT'd ingress tunnels.
5. Whether `--profile control-plane` servers (no local workloads) must also bring up their mesh end. Today only a server that runs workloads does, so the mesh cannot be enabled there.
