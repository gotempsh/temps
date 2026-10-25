# ADR 055: Workers Join With No Public Address

- **Status:** Proposed
- **Date:** 2026-10-19
- **Builds on:** ADR-048 (D1 optional endpoints, D2a URL join, D2b pairing, D3 node API over the mesh)

## Context

A worker that joins over the internet ("The worker reaches this server") runs
`temps join <url> <token> --private-address <worker-public-ip>`. That address is
reused for four things:

- the agent URL the control plane registers (`https://<ip>:3100`);
- a SAN of the agent's TLS leaf;
- the address published container ports bind to, which the proxy dials
  (`nodes.private_address`);
- the WireGuard endpoint the agent publishes (`private_address:<mesh port>` when
  no `--wg-endpoint` is given).

So the guide asks the operator to open the worker's mesh port, to find its public
IP, and, behind NAT, to forward a port and pass `--wg-endpoint`. `--pair` is no
better: the control plane dials the worker, so the worker must accept inbound
UDP. Every internet join therefore exposes the worker.

None of it is needed. WireGuard needs only one reachable side (ADR-048 D1).
When the control plane publishes an endpoint, the worker can dial it, keep the
NAT mapping open with keepalives, and be reached by the control plane through
the tunnel it opened. Agent calls already travel the mesh. What is missing:

- a way for the worker to hand the control plane its WireGuard key before it
  is registered, without the control plane dialling it;
- registration under the worker's own mesh address (today every mesh-pool
  address is refused);
- a node with no WireGuard endpoint (today `register_node_in`, the agent's
  `PUT /network/wireguard` and `mesh_entry` all require one).

## Decision

`temps join <url> <token>` with neither `--private-address` nor `--relay-url`
(today an error) enrolls the worker **outbound only**. The worker never listens
on a public address, never needs an inbound port and never needs to know its
public IP. `--private-address` and `--pair` are unchanged.

### 1. The worker sends its key over HTTPS (mesh enrollment)

On Linux only (kernel WireGuard), the worker loads or creates its mesh key and
calls `POST {url}/api/internal/nodes/mesh-enroll` with
`{ name, join_token, wg_public_key, labels }`, trusting public roots. Unlike a
direct join, the answer is the join's trust anchor (the CA fingerprint the rest
of the join pins), so the URL must be `https://` (plain HTTP only to a loopback
address), redirects are not followed (a redirected POST would re-send the
token), and the request is bounded (10 s to connect, 30 s in all) so a
control plane that never answers cannot hold `temps join`.

The control plane:

1. Validates the join token with the same rules as `/internal/nodes/register`
   (expiry, revocation, uses, bound name, bound labels), rate-limited per
   source IP the same way. The legacy shared token is refused here: it has no
   use count, so it could hold every pending pairing slot one fresh key at a
   time, and nothing older calls this route.
2. Requires the mesh to be on and the control plane to publish a WireGuard
   endpoint (`network_config.control_plane_wg_endpoint`). Otherwise it answers
   409 with what to do (enable the mesh; start `temps serve --wg-endpoint`, or
   use `--pair` / `--private-address`). These checks run **before** the token
   use is claimed, so a refused enrollment does not spend it; if creating the
   pairing fails after the claim, the use is released again.
3. Claims one use of the token, then creates a pairing (`node_pairings`) the way
   `NodePairingAdminService::start` does, except that it has **no node
   endpoint** and starts in `key_received` with the worker's key. The row is
   inserted directly as `key_received` in one transaction under the
   `network_config` lock, applying the same key checks as
   `pairing::record_key` (one shared helper): the key must not belong to the
   control plane, a node or another pending pairing. The pairing gets the usual
   single-use, name-bound join token with the pairing TTL, carrying the
   original token's bound labels and creator. A pairing secret is still
   generated and stored encrypted (the column is required) but never handed
   out. The enrollment is audited like a console pairing (`user_id` 0, no
   endpoint).
4. Answers with what `--pair` carries in its code, minus the pairing secret:
   control-plane key, endpoint and mesh address, the node's reserved mesh
   address, prefix length, mesh port, node API port, cluster CA fingerprint,
   the pairing's join token and its expiry.

`mesh-enroll` is not on the node-API allow-list, so a pairing's own token
cannot be used over the mesh to start another pairing. A control plane without
the route answers 404, which `temps join` turns into "upgrade the control
plane, or join with `--private-address`".

The pairing initiator ignores it (it only dials `waiting` pairings). The
control plane peers it like any `key_received` pairing, with no endpoint, so
WireGuard learns the worker's address from its first handshake (roaming).

### 2. The worker brings up the mesh and registers through it

The worker brings up `temps-wg0` with its reserved address and the control
plane as its only peer (`bootstrap_mesh`, with no endpoint of its own), dials
the control plane's endpoint with keepalives, and then registers over the node
API exactly as the second half of `--pair` does: the CA is pinned by the
fingerprint, the server is verified by the reserved control-plane name, and the
token is accepted only from the pairing's reserved address.

It registers with `private_address` = **its own mesh address**. So the agent
URL, the leaf's SAN, the bind address of published ports and the proxy's
backend address are all on the mesh, and `private_address` being RFC 1918 means
the existing "private network" paths apply unchanged: the control plane dials
`address` directly (over the tunnel) and the data address is the mesh address.

Registration accepts a mesh-pool address in `address`, `private_address` and
the CSR only in this case: the registration came through a pairing, from that
pairing's reserved address, and the address is that same reserved address.
Every other mesh-pool address, other members' addresses and the reserved
control-plane name stay refused (ADR-048 Security). The node **name** is never
exempt, and another member's address is reported as a pool address, the first
check it fails. When a node re-joins with a new key and registers under the
address its pairing reserved, that address wins over the one it held before.
Enabling or re-configuring the mesh does not count a node's own mesh address
in `address` / `private_address` as "outside the pool" against it.

### 3. Nodes may have no WireGuard endpoint

- `nodes.mesh_wg_endpoint` (already nullable) is `NULL` for such a node, and
  `node_pairings.node_endpoint` becomes nullable, with a check constraint
  that a `waiting` pairing (the one the control plane dials) still has one.
- `register_node_in` and `PUT /internal/nodes/{id}/network/wireguard` take an
  optional endpoint; when present it is validated as today.
- The agent publishes no endpoint when it has no `--wg-endpoint` and its
  `private_address` is inside the mesh CIDR, instead of `private_address:port`.
  `mesh_entry` returns the node's own entry with `endpoint: null`.
- The control plane peers it with no endpoint and keepalives; WireGuard roams to
  wherever the worker's packets come from. Two members without endpoints never
  get a direct entry for each other (ADR-048 D1); their traffic needs a hub
  (D4), as today for members that cannot reach each other: such a pair never
  handshakes, so the hub routing that is driven by handshake observation
  carries it. A worker that does
  have a public address may still pass `temps agent --wg-endpoint` to let other
  workers dial it directly.

### 4. Console

"The worker reaches this server" becomes: open the control plane's mesh port
(nothing on the worker), install, `temps join <url> <token>`, `temps agent
service install`. The worker-side port, the public-IP placeholder and the NAT
hint go away. The option is offered only when the control plane publishes an
endpoint and the join URL is reachable over `https://`; a public plain-HTTP
URL is explained (`temps join` refuses it for this mode) and the page keeps
today's choices. Because such workers publish no endpoint, the guide says
that a second one needs a mesh hub (ADR-048 D4) to reach the first, or names
the hub when one is chosen.

## Security

- **Nothing new is exposed on the worker.** It opens no listening socket on a
  public interface: WireGuard sends from its port and accepts only the control
  plane's key; the agent and published ports bind to the mesh address.
- **The join token stays the only credential.** Mesh enrollment needs a valid
  token and consumes one use. It reveals the CA fingerprint, the control
  plane's public key and endpoint, which a token holder learns today anyway.
  A leaked token lets someone join, as today.
- **Pending enrollments are bounded** by the existing pairing cap (`MAX_PENDING`)
  and TTL; an expired pairing releases its address and stops being peered.
- **Registration from a pool address is narrow**: only the pairing's own
  reserved address, only for a registration that arrived from that address over
  the node API with that pairing's token. Leaves for other members' addresses
  and for the control-plane name remain impossible.
- **Only public keys cross the wire**; the private key never leaves the worker
  (ADR-048).
- **An address named by a certificate is never handed out again.** Agent
  leaves are not revoked, and an outbound worker's leaf names its mesh
  address. Every address in a leaf issued by the current cluster CA
  (`cluster_certificate_addresses`) is excluded from new pairing
  assignments, so a later member can never be given an address an older
  leaf still vouches for.

## Consequences

- Joining over the internet needs no worker address, no inbound port and no
  NAT configuration on the worker.
- The worker's mesh must be up before the agent can bind published ports;
  the agent restores the mesh from its snapshot when it starts. Docker may
  restart containers at boot before the agent has done so, and a container
  that publishes on the mesh address then fails to start until it is
  restarted. This is the same for `--pair` workers today and is not
  addressed here.
- Such workers can reach each other only through a hub (or by passing
  `--wg-endpoint`); with one worker nothing changes.
- Existing workers, `--private-address`, `--pair`, relay and SSH joins keep
  their behaviour. The schema change (nullable `node_endpoint` plus the check)
  migrates no existing rows; rolling it back drops outbound pairings.
