# ADR 060: Re-joining the same control plane, and removing an empty node

## Status

Accepted — 2026-10-23

Related: ADR-020 (enrollment hardening: `prior_token`), ADR 088 (workers join
outbound only), ADR 056 (resuming a pending enrollment), ADR 057 (removing a
node that is gone), ADR 059 (a join that looks first).

## Context

Three things went wrong when a worker that already belongs to a cluster ran
`temps join` again.

1. **The preflight compared URLs.** ADR 059's check decides "same cluster"
   by comparing the target URL with the `control_plane_url` / `join_url` in
   `agent.json`. A worker that joined outbound saved only the control plane's
   mesh address (`https://10.201.0.1:…`). Every cluster's control plane has
   that address, so the comparison said nothing: the same cluster under its
   public URL was reported as "another control plane", and two different
   clusters could match.
2. **A re-join was refused.** `mesh-enroll` reserves a new mesh address for a
   new pairing, and refuses a WireGuard key a node already holds ("WireGuard
   Key In Use", 409). A worker re-running `temps join` keeps its key, so it
   could never re-join. The only way out was to remove the node and join it
   as a new one.
3. **An empty node took two steps to remove.** The console offered Remove
   only for drained, offline or unreachable nodes. A reachable node with
   nothing on it had to be drained first, and the drain had nothing to do.

## Decision

### Identity, not address, decides "same control plane"

The control plane serves `GET /internal/nodes/plane-identity` without
authentication, rate-limited like enrollment:

```json
{ "ca_fingerprint": "…", "control_plane_wg_public_key": "…" }
```

Both values are public: the CA fingerprint is in every join command, and a
WireGuard public key is public by design. `temps join` reads the matching
values from this machine before it sends the join token:

- the fingerprint of the cluster CA at `agent.json`'s `cluster_ca_path`;
- the `control-plane` peer's key in the agent's last network snapshot.

The cluster CA decides when both sides have one; otherwise the mesh key. The
result is one of three:

- **same**: the join goes on and is a re-join;
- **different**: the join stops and says which identity differs. `--force`
  overrides it;
- **unknown**, when nothing can be compared or the control plane does not
  answer: the join falls back to the URL comparison. A differing URL then
  stops with a message saying identity could not be determined, and why. It
  does not claim the control plane is another one. `--force` overrides it.

The answer is only compared, never trusted. A party that echoes the right
values gains nothing it could not get by being at the URL the operator typed:

- the agent token is never sent on the strength of this answer (see below);
- on an outbound re-join recognised by its CA, `temps join` pins the CA it
  already holds, so the registration over the mesh can only complete with
  the real control plane.

### A re-join proves it is the node

The key and the name are not secrets, so they prove nothing on their own.
The proof is the node's current agent token, which only that node and (as a
SHA-256 hash) the control plane hold. The token itself is not sent. Instead
the worker sends:

```
rejoin_proof = HMAC-SHA256(key = sha256_hex(agent_token),
                           "temps-node-rejoin/v1" 0 name 0 wg_public_key 0 join_token 0)
```

(`temps_core::node_pki::rejoin_proof`.) The control plane recomputes it from
`nodes.token_hash` and compares in constant time. Because it is bound to the
single-use join token of this enrollment, a proof sent to the wrong control
plane cannot be replayed anywhere. So `temps join` sends it whenever the
saved node has this name and the control plane is not known to be another
one.

`mesh-enroll` then decides, before it reserves anything:

| Mesh key held by           | Name      | Proof            | Result                                             |
|----------------------------|-----------|------------------|----------------------------------------------------|
| no node                    | —         | —                | new enrollment (ADR 055/056), unchanged            |
| a node                     | another   | any              | refused: "WireGuard Key In Use", unchanged message |
| a node                     | same      | valid            | **re-join**: no pairing; the node keeps its mesh address; a fresh single-use token bound to its name; `rejoined_node_id` in the answer |
| a node                     | same      | missing          | refused (409), the token's use given back, message says how to send it |
| a node                     | same      | wrong            | refused (409), **the token's use is kept**: every guess costs a join-token use |

A different key under an existing name follows the new-enrollment path and is
refused there as before. The refusals change nothing on the control plane.

The worker then registers over the mesh node API, from its own mesh address,
as an outbound worker always does. Two things make that registration an
in-place update:

- **The address exemption.** The mesh-pool guard refuses a registration
  naming a pool address, except the one its pairing reserved. A re-join has
  no pairing. It is exempt for the address the node of that name already
  holds, only when the registration arrives over the node API *from that
  address*. WireGuard's cryptokey routing only delivers a packet from that
  source if it was sealed with the key that holds the address, so this is
  proof of the key (`MeshAddressOwner::Node`).
- **The existing ADR-020 guard.** A live node's identity changes only with
  its `prior_token`. `temps join` sends the agent token there, over the
  channel pinned to the cluster CA, when the control plane recognised the
  proof (`rejoined_node_id`) or the preflight recognised it.

Each re-join start is audited as `NODE_REJOIN_STARTED`.

### An empty node is removed in one step

`DELETE /internal/nodes/{id}` on a reachable `active` node:

- **it has containers a deployment still uses:** the request is refused with
  `StillServing`, which names the count. The node's status is not touched;
- **it has none:** the node is marked `draining` with a conditional update
  (only from `active`), so nothing is placed on it while the removal runs.
  The existing removal then runs; `remove_with` counts again under the node's
  row lock. If the removal does not happen, the node is made `active` again.

Sandboxes still refuse the removal (ADR-048). An unreachable node's `force`
removal (ADR 057) is unchanged.

`NodeInfoResponse` carries `serving_containers` and `live_sandboxes`. They
are counted for the whole list in two grouped queries; a count that cannot be
read is left unset. The console decides from them:

- an empty node gets Remove, behind a confirmation;
- a node with workloads gets "Drain first — it has N containers";
- an unset count keeps the previous drain-first flow.

`drain_status` reports `can_remove` for an empty active node.

## Consequences

- A worker can re-run `temps join` against its own cluster, under any URL,
  and keep its node ID, placements and mesh address.
- A worker whose `agent.json` has no CA and no snapshot, joining under a new
  URL, is told identity is unknown and why. With `--force` it still re-joins
  if its proof is valid.
- A control plane that is reinstalled at the same URL has a new CA. It is now
  reported as another control plane, where the URL check let it through.
- Direct (`--private-address`) joins send the agent token before anything is
  pinned, so they keep deciding by address. Only a control plane known to be
  another one is now excluded.
- An older control plane answers 404 for `plane-identity`. The preflight
  then says it cannot tell, and why.
