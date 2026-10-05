# ADR 048: Control-Plane Enrollment, Mesh Hubs and Ingress Nodes

- **Status:** Proposed
- **Date:** 2026-09-28 (revised 2026-09-29)
- **Builds on:** ADR-017 (split proxy/console), ADR-020 (multi-node hardening), the managed WireGuard mesh (`crates/temps-wireguard/src/mesh.rs`, `crates/temps-network/src/mesh.rs`)

## Context

A cluster is a control plane (`temps serve`) plus worker nodes (`temps agent`). The managed WireGuard mesh lets nodes that only share the internet join: every node gets a private mesh address, the VXLAN overlay runs over it, and the control plane reaches a publicly-joined node's agent and published ports on that address (`nodes::Model::data_address`).

Two limits remain.

### 1. The worker must reach the control plane to join

The control plane already distributes keys: each agent registers its public key and endpoint (`PUT /internal/nodes/{id}/network/wireguard`), the control plane assigns mesh addresses and hands every node its peers (`GET /internal/nodes/{id}/network/peers`). But every step starts with the worker calling the control plane:

- **A control plane without a public address.** On a laptop or a home server the Worker Nodes page tells a remote machine to `temps join https://app.localho.st …`, a URL that resolves to the worker itself. Workers need the control-plane API to join, sync routes, fetch peers and heartbeat; none of it works.
- **Nodes behind NAT.** A mesh endpoint is an `ip:port` other members dial. Two members that are both behind NAT cannot reach each other at all (`docs/features/multi-node/page.mdx`: "nodes behind NAT are not supported yet").

`temps join --relay-url` (`crates/temps-cli/src/commands/join.rs`) is a client for a relay whose server exists nowhere. It swaps keys over `POST {relay}/api/relay/clusters/{id}/join`, sets up a separate `wg0` with `10.100.0.x`, and still assumes direct reachability. It predates the managed mesh.

WireGuard itself is symmetric and already handles one NAT'd side: whichever peer can reach the other initiates, keepalives (`PERSISTENT_KEEPALIVE_SECS`) hold the NAT mapping open, and the responder learns the initiator's address from its packets (roaming, preserved by `LIVE_HANDSHAKE` in `reconcile_peers`). A link needs **one** reachable side, not two. What is missing is enrollment that does not depend on the worker reaching the control plane first, and a path between two members that are both unreachable.

### 2. The control plane carries all application traffic

The control-plane proxy (Pingora, `crates/temps-proxy`) serves every domain, always. `temps serve --role console` moves the listeners into a separate `temps proxy` process, which needs direct Postgres access and the encryption key (`crates/temps-cli/src/commands/proxy.rs`); `--profile control-plane` stops local workloads but still proxies. There is no mode in which the control plane carries no application traffic.

Temps Cloud hosts each customer's control plane and customers join their own machines as workers. Cloud must not carry customers' end-user traffic: it is the dominant cost, and it would make every customer's site depend on Cloud's network.

Two partial mechanisms exist:

- **Worker public ingress** (`crates/temps-agent/src/public_ingress.rs`, `internal_proxy.rs`, `route_store.rs`): a small hyper/rustls listener in `temps agent` on `--public-ingress-address`. It long-polls the route snapshot (`crates/temps-routes/src/route_sync.rs`), receives exact-host certificates encrypted to the node's key, relays HTTP-01 challenges to the control plane and forwards to `<node address>:<published port>`. Its limits:
  - Only plain upstream routes qualify. Redirects, wake-on-request, attack mode, per-project security, static sites and wildcard hosts stay on the control plane (`route_table.rs` `snapshot_worker_public_routes`).
  - Global rate limiting, global security headers, a single IP access rule anywhere, or a custom request-policy gate withholds **every** route from **every** worker (`route_sync.rs`).
  - Routes lapse 300 s after the last successful sync (`route_store.rs`), so a control-plane outage longer than five minutes takes down every site served by workers.
  - DNS is manual.
- **`temps-edge`** (`crates/temps-edge`): a caching Pingora CDN node. Every request it does not serve from cache goes to one origin, the control plane (`proxy.rs` `upstream_peer`). It adds a hop in front of the control plane rather than removing it. Registration fails with default settings, and `role = "edge"` nodes are not excluded from scheduling (`node_scheduler.rs`).

### How OpenShip does it

OpenShip has no mesh and no key exchange. Its controller (a desktop app or a server) dials **out** to each server over SSH (password, key or the local SSH agent), keeps a pooled connection, and drives Docker through an SSH tunnel. That is why a laptop controller works: it only ever connects outward to public servers. Unreachable servers go through Cloudflare Tunnel or a jump host. Each server runs its own OpenResty and certbot; the user points DNS at it; servers never talk to each other.

## Decision

The control plane enrolls nodes and may dial them, the way OpenShip's controller does; WireGuard carries everything after enrollment; members that cannot reach each other go through a **hub**, a role the operator assigns to a reachable member from the control plane. There is no relay protocol and no service Temps must run for a cluster to work.

### D1. Either side of a mesh link may dial

- A member **publishes an endpoint only if it is reachable** (public address, or `--wg-endpoint`). The control plane's endpoint becomes optional: a control plane behind NAT publishes none (`network_config.control_plane_wg_endpoint = NULL`, allowed once D2 exists) and dials its peers instead.
- Every peer entry that has an endpoint gets keepalives (already true), so the NAT'd side of each link keeps it open. Nothing else changes in the WireGuard layer.
- The control plane classifies each member as **reachable** (published endpoint) or **not reachable**. Two unreachable members never get a direct peer entry for each other (D4 routes them through a hub).

### D2. The control plane enrolls nodes

Enrollment ends in the same state whichever transport carried it: the control plane holds the node's WireGuard public key and endpoint (if any); the node holds the control plane's public key and mesh address, its own mesh address, the cluster CA fingerprint and a one-time enrollment credential. **A node's WireGuard private key never leaves the node.**

Every path is **one paste** or none. Which side sends the worker's public key to the other depends on which side is reachable:

- **D2a. Control plane reachable: URL join (today).** `temps join <url> <token>`; the worker pushes its key over HTTPS. The Worker Nodes page offers it only when the join URL is reachable from other machines (not loopback, not `*.localho.st`).
- **D2b. Control plane not reachable, worker reachable: pull pairing.** The control plane fetches the worker's public key itself:
  1. **Worker Nodes → Add node** (CLI: `bunx @temps-sdk/cli nodes pair create --address <ip>[:port]`): the operator enters the worker's public address. The control plane creates a pending node, assigns its mesh address and returns one command: `temps join --pair <code>`. The code carries the control plane's WireGuard public key, both mesh addresses, the mesh CIDR and port, the cluster CA fingerprint and a single-use 256-bit pairing secret with a short TTL.
  2. On the worker, `temps join --pair <code>` generates the mesh key locally and binds the mesh UDP port, answering only pairing messages authenticated with the secret (below).
  3. The control plane sends pairing hellos to that address until the code expires. On a valid exchange it records the worker's public key, adds it as a peer with the operator-entered endpoint, and dials it. The worker releases the port to WireGuard with the control plane as its only peer, then completes the normal registration (CSR, agent token, `edge_public_key`) over the mesh (D3).

  **Pairing exchange** (UDP, on the mesh port, before WireGuard owns it; every message carries the pairing id and `HMAC_k` over the rest, `k = HKDF-SHA256(secret, salt = pairing id, info = "temps-pair-v1")`):
  - `HELLO { nonce_cp, cp_public_key }`, control plane → worker, padded to 160 bytes and repeated every second. The worker checks the MAC and that `cp_public_key` matches the code.
  - `OFFER { nonce_cp, nonce_w, worker_public_key }`, worker → control plane. The control plane checks the MAC and the echoed nonce, then records the key: it must not belong to the control plane, a node or another pending pairing, and the pairing must still be waiting.
  - `CONFIRM { nonce_w }` once the key is recorded, or `REJECT { nonce_w, reason }` when it is refused (key in use: the worker's key file was copied from another machine; pairing closed: cancelled, expired or already used). A rejected worker stops and says what to do; a key that could not be stored yet (database unavailable) gets no reply, so the worker keeps answering and the next attempt retries. Replies are sent three times; a worker that sees no reply for 20 s after its last `OFFER` treats the control plane's silence as a lost `CONFIRM`.
  - Only public keys cross the wire, so confidentiality is not needed; the MAC gives integrity and proves both ends hold the secret. The worker answers nothing without a valid MAC, and every reply is smaller than the `HELLO` that caused it, so the port is useless for amplification. The pairing takes one key; the join token in the code is single-use.
  - It reuses the port the mesh needs open anyway, so pairing adds no firewall rule.
- **D2c. SSH.** **Worker Nodes → Add server over SSH**: host, port, user and a password, private key or the server's SSH agent. The control plane connects, shows the host-key fingerprint for confirmation on first use, installs `temps` if it is missing and runs `temps join --pair -` (the code on stdin) plus `temps agent service install`; the pairing exchange (D2b) then runs as usual. Credentials are used for that operation and discarded. Keeping them (encrypted) for later upgrades is not built yet.
- **Neither side reachable** needs a hub (D4); pairing then runs through the hub, which is reachable by definition.

### D3. Node API over the mesh

A paired node reaches the control plane at its mesh address. `temps serve` runs a **node API listener** on `<control-plane mesh address>:<node-api port>` (TCP; defaults to the mesh port number and is configurable, like the mesh UDP port, with `temps network setup-multi-node --wireguard --node-api-port` and the enable API) that serves only the node-facing routes (`/internal/nodes/*`, route sync, the ACME HTTP-01 relay), over TLS with a certificate from the cluster CA, verified by the fingerprint in the acceptance. The mesh firewall on the control plane accepts that port from mesh members and nothing else new. WireGuard authenticates both ends; TLS and the agent's existing credentials are defence in depth.

Every control plane brings up its mesh end, including one that runs no workloads (`--profile control-plane`, every Temps Cloud control plane). Today only a server with local workloads does, which is why enabling the mesh there is refused.

Result: a control plane on a laptop enrolls and manages public workers with no inbound port, no relay and no third party.

### D4. Hubs: a role managed in the control plane

When two members cannot reach each other (both unreachable, or the operator marks a link as blocked), their traffic goes through a **hub**: a reachable member the operator assigns in **Worker Nodes → Mesh** (CLI: `nodes mesh hub set <node>`).

- The control plane computes each node's peers: a direct entry for every member it can reach, plus the hub's entry carrying the `/32`s of the members it cannot reach. The hub has a direct entry for every member; unreachable members keep it alive with keepalives.
- The hub forwards `temps-wg0` → `temps-wg0` between mesh members only (a hub-only rule in `render_mesh_lockdown`); the members' own mesh firewalls still decide what they accept.
- A hub decrypts and re-encrypts what it forwards, so it sees relayed mesh traffic. It is one of the operator's own members, with the same trust as any member on a shared private network (ADR-020). The UI says this when assigning the role and marks relayed peers **Via hub**.
- The control plane is the default hub when it is reachable. **On Temps Cloud it is not the default**: a customer's reachable node is, because relayed traffic can include application traffic. Using the Cloud control plane as a hub is opt-in and metered.
- A cluster with no reachable member at all (laptop control plane and home-lab workers) needs one reachable machine. Temps Cloud may offer a hub-only node that joins the customer's cluster through D2 like any other node and is managed from the customer's control plane.
- WireGuard is UDP-only. Networks that block outbound UDP are out of scope here; a TCP fallback is a later option.

As built (P3): the control plane does not guess reachability from addresses; it observes it. Each agent reports the age of its last handshake with every peer (`PUT /internal/nodes/{id}/network/wireguard/handshakes`, stored in `node_mesh_reports`) and the control plane reads its own from `temps-wg0`. Per pair it keeps a `mesh_links` row:

- A new pair starts **direct**. It moves **via hub** when a hub is set, the hub is neither member and has handshaken with both within `LIVE`, both members reported within 90 s, and the pair has had no handshake for 150 s since the link (or its last endpoint change) started.
- A relayed pair returns to direct when either member's endpoint changes, the hub is removed or becomes one of the two, the hub stops reaching one of them, or a node hub's own report is more than 90 s old (it stopped reporting, or its relay rule could not be installed). There is no periodic retry of the direct path: it would break a working relayed path to probe.
- A relayed member's `/32` moves onto the hub's peer entry (`MeshPeer::relayed`) on both sides. Every member enables `forwarding` on `temps-wg0`, because the kernel checks it on the interface a packet arrives on and published ports are DNAT'd, that is forwarded. Relaying is limited to the hub by the mesh lockdown alone: only the hub's lockdown carries the hub rule, and every other member drops anything from the tunnel that is not a published-port connection. The hub also installs an `iptables DOCKER-USER` accept (comment `temps-mesh-relay-v1`) on every iptables backend that has that chain, because Docker's `FORWARD` policy drops otherwise; a hub that cannot tell whether the chain exists reports an error rather than relaying nothing silently.
- The hub is one row in `network_config` (`mesh_hub_node_id`, or `mesh_hub_control_plane`), set with `PUT /nodes/wireguard/hub` (settings write, sensitive-action step-up, audit record `WIREGUARD_MESH_HUB_CHANGED`). The control plane is not made the hub automatically: the operator chooses, from the pairs the status shows as unreachable and the members it names as reaching both.

### D5. Ingress is a node capability; the control-plane proxy can be switched off

- **Any node can take public traffic.** The existing public-ingress toggle becomes the "ingress" capability of a node. An ingress node forwards to containers on any node over the mesh or the private network, so DNS may point at any healthy ingress node (ADR-020: every node accepts ingress for any domain). A **dedicated edge** is an ingress node that runs no workloads; the scheduler excludes it.
- **The mesh allows members to reach published ports** (shipped in P0: `render_mesh_lockdown` accepts DNAT'd traffic from the mesh pool).
- **Control-plane proxy mode: `full` (default) or `off`.** With `off`, the control plane serves only its console/API host and the endpoints ingress nodes depend on. Any other host gets a short page naming where the app is served. Temps Cloud runs every hosted control plane with `off`.
- An ingress node needs a public address. Serving apps from a NAT'd node through Cloud is a separate, opt-in, metered offering.

### D6. Ingress feature parity by reusing the proxy engine

Ingress nodes run the same Pingora engine as the control-plane proxy, fed by the route-sync snapshot instead of Postgres; customer machines never hold database credentials. The snapshot grows the policy data the engine needs: redirects and force-HTTPS, security headers and per-project security, rate limits (per ingress node, documented), IP rules on the real client address, attack mode, the request-policy gate, wake-on-request (asking the owning node's agent) and static sites. This replaces the agent's small listener. Until a feature has an ingress implementation, a cluster whose control-plane proxy is `off` refuses to enable it and says why, rather than dropping the route.

### D7. Ingress keeps serving when the control plane is unreachable

Ingress routes and certificates are **fail-static**: an ingress node that cannot reach the control plane keeps its last snapshot, and drops everything on an explicit rejection (401/403/404: removed or revoked). Certificates work until they expire; renewal needs the control plane. This matters most for a laptop control plane, which is offline whenever the laptop sleeps.

### D8. DNS and certificates for ingress nodes

- A node records its public ingress address. Self-hosted operators point DNS at one or more ingress addresses, listed in the UI.
- Temps Cloud's managed DNS maintains a per-tenant name (e.g. `ingress.<tenant>.<cloud-domain>`) pointing at the tenant's healthy ingress nodes; customers CNAME their domains to it.
- A tenant-scoped wildcard certificate may be exported **only** to that tenant's own ingress nodes.

### D9. Mesh doctor

Every mesh failure is reported as a state plus the action that fixes it, on both ends:

- **Node side: `temps doctor mesh`** (run on a worker or the control plane, no database needed on workers). Checks, each with the command or setting that fixes a failure:
  - the WireGuard kernel module and `temps-wg0` exist and hold the expected key, address and port;
  - the mesh UDP port is bound by WireGuard, not by another process;
  - each peer's last handshake, and whether it is direct or via the hub;
  - the control plane answers on the node API port over the mesh;
  - the mesh firewall table is installed at the expected version;
  - MTU: a full-size packet crosses the mesh without fragmentation;
  - pending pairing: reported live by `temps join --pair` itself (the port and protocol to open, the expiry, and a refusal with the file to delete) and by the control-plane side below, since a node has no cluster state until it has joined.
- **Control-plane side: the mesh status API** (`GET /nodes/wireguard`) carries per-node `checks` from what the control plane can observe (agent heartbeat, mesh key, handshake, and which end must open its port), plus hub routing once hubs exist; pairings carry their last error and, separately, their last refusal, which later "no answer" attempts do not overwrite. The Worker Nodes page shows each failing check with its fix ("What to fix"). CLI: `bunx @temps-sdk/cli nodes mesh doctor` (exits 1 when a check fails).
- The existing `temps doctor` runs the node-side mesh checks when the mesh is enabled.

### D10. Retire what this replaces

- Remove the `--relay-url` join mode and `temps_wireguard::WireGuardManager`'s `wg0`/`10.100.0.x` path.
- `temps-edge`: exclude `role = "edge"` from scheduling now; fold its cache into the ingress engine later and document it as experimental until then.

## Alternatives considered

| Option | Why not |
|---|---|
| Keep requiring the worker to reach the control plane (status quo) | Excludes laptop and home control planes; the Worker Nodes page already shows unreachable join commands. |
| A custom relay protocol (earlier draft of this ADR): members hold a TLS/WebSocket connection to a relay carrying API streams and WireGuard datagrams through local UDP forwarders | A new protocol and userspace forwarding to solve what WireGuard already does when one side is reachable, plus TCP-over-TCP throughput limits. Its one advantage, UDP-blocked networks, can be added later as a fallback. |
| A Temps-Cloud-run WireGuard hub service outside the cluster, with nested tunnels so it sees only ciphertext | Needs a key-registration service and a second tunnel layer (MTU cost). A hub that is a cluster member (D4) needs neither, and stays under the operator's control. |
| Control plane generates the worker's key and embeds it in a one-copy join command | A private key would leave the control plane in a pasteable string. Pull pairing (D2b) gets one paste without it. |
| Two-paste pairing codes (worker prints its public key, operator pastes it into the control plane and copies a command back) | Works without either side dialing, but two pastes; pull pairing needs one because the control plane can dial a reachable worker. |
| Embed Tailscale-style coordination + DERP (Headscale), NetBird, Nebula, Netmaker | Each brings its own key, address and ACL management, duplicating the control plane, plus another agent on every node. |
| Cloudflare Tunnel / ngrok for the control-plane API | Ties self-hosters to a third party, terminates TLS at the provider, does nothing for member-to-member traffic. |
| Extend `temps-edge` as the ingress | Single-origin design, no backends, no policy enforcement, broken registration. |
| Cloud-run edge fleet in front of workers | Cloud carries all customer traffic, the thing this ADR avoids. Kept only as a paid add-on. |
| Each node serves only its own apps (OpenShip) | Simpler, but DNS must follow every redeploy and scale-out; loses cross-node ingress. |

## Security

- **Private keys never leave their host.** The control plane never generates, sees or transports a node's WireGuard private key; only public keys cross the wire in any enrollment path.
- **Pairing codes (D2b)** carry a single-use, short-lived secret bound to the pending node and the operator-entered address. A leaked code lets someone pair only from that address and only before the real worker does; the pending node shows who paired, and the code expires unused.
- **Operator approval.** No node enters the mesh without an operator action in the control plane (creating a pairing, or running the SSH flow). Creating a pairing requires `SettingsWrite` and a sensitive-action step-up, and is audited.
- **SSH (D2c).** Host keys are pinned on first use after the operator confirms the fingerprint. Credentials are held in memory for the operation and never stored. Remote commands are fixed (`uname`, `id`, `sudo` probes, `docker info`, locating `temps`, the installer, `temps join --pair -`, `temps agent service install`) with no operator-supplied shell interpolation; the pairing code and a sudo password travel on stdin. Reading a host key and adding a server both require `SettingsWrite` and a sensitive-action step-up; adding one is audited. The host key is only as trustworthy as the operator's check: the UI and CLI ask them to compare it with the server's own out of band (`ssh-keygen -lf` on its console), and every later connection is pinned to it. The server's output is kept in the enrollment log (readable with `SettingsRead`) with every secret the enrollment holds masked: the password, the passphrase, the private key, the pairing code and the join token and secret inside it. A server without `temps` runs the official installer (`curl -fsSL https://temps.sh/install.sh | bash`) as root, so the enrollment trusts temps.sh and its TLS certificate like a manual install does.
- **Node API listener (D3)** binds only to the control plane's mesh address, serves only node-facing routes, and requires the same agent credentials as today. The mesh firewall opens only that port, only on the control plane.
- **Hubs (D4)** see the plaintext of the traffic they relay. Only reachable members the operator chose can be hubs; the UI states the trade-off; Cloud control planes are hubs only on opt-in.
- **Mesh firewall (P0)**: a compromised node can reach other nodes' published app ports over the mesh, as on a shared private network (ADR-020); host services and the agent API stay unreachable.
- **Ingress nodes** hold private keys for the hosts they serve and see that traffic in plaintext: the same trust as a worker of that tenant. Cloud never exports one tenant's material to another tenant's nodes.
- **Fail-static (D7)** keeps a revoked node serving until its revocation reaches it.

## UX and onboarding

- **Add node** on the Worker Nodes page offers three paths and picks the default from the situation:
  - **Join URL** when the join URL is reachable from other machines; a loopback, `*.localho.st` or private URL shows why it is hidden;
  - **Pairing** (enter the worker's address, run one command on it) when the control plane is not reachable;
  - **Over SSH** as the one-step alternative to either.
- Pending pairings show until the worker completes `temps join --pair`, with their progress (waiting for the worker, key received, handshake, registered), an expiry and a cancel action.
- The **Mesh hub** card on Worker Nodes lists every pair that is not simply direct (**Through the hub**, **Connecting**, **Cannot connect**) with its fix, and each node's **Links** check says which members it cannot reach. With the mesh off the card stays and says what turns it on.
- A capability endpoint reports enrollment and hub state (`configured: false` + reason + setup path), so the UI and CLI tell "not set up" apart from "not built".
- CLI parity lives in `bunx @temps-sdk/cli` (`nodes pair`, `nodes pair create/cancel`, `nodes ssh`, `nodes ssh add/show`, `nodes mesh doctor`, `nodes mesh hub`, `nodes mesh hub set/unset`). `temps join --pair`, `temps agent service` and `temps doctor mesh` are node-side commands in the Rust binary, like `temps join`.

## Rollout

| Phase | Scope | Unlocks |
|---|---|---|
| P0 (shipped on this branch) | Remote backends use `data_address()`; the mesh firewall admits mesh members to published ports | Ingress over the mesh works |
| P1 | Pull pairing (D2b), node API over the mesh (D3), optional control-plane endpoint (D1), mesh on control planes without local workloads, mesh doctor (D9) | Laptop/home control planes with public workers; Cloud control planes on the mesh |
| P2 | SSH enrollment (D2c) | One-step onboarding from the control plane |
| P3 (done) | Hub role (D4) | NAT'd workers; clusters with only one reachable member |
| P4 | Control-plane proxy `off`, ingress capability, scheduling exclusion, guardrails, fail-static (D5, D7) | Temps Cloud without carrying app traffic |
| P5 | Pingora engine on ingress nodes (D6) | Full feature parity; guardrails removed |
| P6 | Cloud managed ingress DNS, tenant wildcard certificates (D8); UDP-blocked fallback | Zero-touch DNS |

## Testing

- **P0 (done):** DinD cluster, a worker joined on a CGNAT address with the mesh on; the app on it answers through the control-plane proxy and through another worker's public ingress, with no request in the control-plane proxy log.
- **P1:** put the control plane behind a `MASQUERADE` namespace with no inbound path; pair a worker on the public network; assert it joins, heartbeats, receives routes and runs a deployment, all over the mesh.
- **P2:** SSH enrollment against a DinD worker running `sshd`, including host-key mismatch refusal.
- **P3 (done):** DinD cluster with two workers on a private network and two on a separate one, the control plane and a fifth worker reaching both. Without a hub the four cross pairs show **Unreachable** and `ping` between them fails; with the control plane as hub, and then with a worker as hub, they show **Via hub** within one evaluation, mesh `ping` and container-to-container HTTP over the overlay work, and the hub's relay counter confirms the path; removing the hub brings back **Connecting**, then **Unreachable**.
- **P4:** with proxy `off`, only the console host answers on the control plane; route sync and the ACME relay keep working.

## Open questions

1. Pairing through a hub when neither side is reachable: the hub forwards the pairing exchange, or the hub pairs the node on the control plane's behalf? (Not addressed by P3: pairing still needs one side the other can dial.)
2. ~~Should a node keep a direct peer entry to a member it believes unreachable?~~ Resolved in P3: every pair starts direct and moves to the hub only after it has observably failed; it returns to direct on an endpoint change or when the hub goes, not on a timer.
3. Rate-limit semantics with several ingress nodes: per-node limits (simple) or a shared budget?
4. Pricing for Cloud-control-plane-as-hub and Cloud hub-only nodes.
