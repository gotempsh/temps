<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-115: The control plane serves private addresses

**Status:** Proposed
**Date:** 2026-11-24
**Amends:** [ADR 064](./064-making-a-project-reachable.md) (D5: "a worker
ingress"), [ADR 065](./065-tailnet-names-and-signed-in-addresses.md) (D1–D5:
Serve and Services "on the worker")

Related: ADR-107 (the control plane runs tunnel connectors), ADR 058 D7
(ingress modes), ADR 068 (environment addresses).

## Context

ADR 064 D5 and ADR 065 give an environment a private address: one only
devices on a tailnet (or a private range) can open. Both are built for
workers:

- **Your domain on the tailnet** (`private_via = listener`): the worker's
  agent binds a private listener on its Tailscale address, Temps points an
  `A` record at that address and issues the certificate by DNS-01.
- **The machine's tailnet name** (`tailscale_serve`) and **a Tailscale
  Service** (`tailscale_service`): the agent writes Tailscale Serve entries
  through tailscaled's LocalAPI, pointing at per-address unix sockets.

The control plane runs no agent, so neither works for an environment it
serves. The planner refuses with "`<env>` runs on the control plane, and a
private address is served by a worker's private listener … Deploy it to a
worker first" (`temps-ingress/src/reachability/plan.rs`, three places).

The install that most wants a private address is a single machine: a home
server or a small VPS on a tailnet, with no workers. Today its only private
answer is "enroll a second machine". The first-run flow (the onboarding plan,
step 2 "Reach your apps") shows *Only my devices* and has to say it is not
available.

ADR-107 met the same gap for tunnels: it made the control plane node `0` for
connectors and ran the agent's connector supervisor inside `temps serve`.
This ADR does the same for private addresses, and keeps every boundary ADR
064 and 065 drew.

## Decision

### D1. The control plane may serve private addresses, as node `0`

Node `0` is the control plane (`temps_core::CONTROL_PLANE_NODE_ID`). It
becomes a valid `private_node_id` on `environment_domains` and
`project_custom_domains`, a valid `node_id` in `node_public_ingress`, and a
valid `node_id` in `tailnet_serve_entries` (which has no foreign key already).

`private_node_id` and `node_public_ingress.node_id` reference `nodes(id)`,
and the control plane has no `nodes` row. As in ADR-107 D1, each foreign key
is replaced by triggers that keep what it guaranteed for every other id: a
non-zero id must name a `nodes` row (`FOR KEY SHARE`), and deleting a node
does what the foreign key did (`SET NULL` for `private_node_id`, `CASCADE`
for `node_public_ingress`). `0` stays an integer, never `NULL`, so a binary
rolled back after the upgrade still decodes every row. The downgrade refuses
while any address is served privately by node `0`.

Which node serves a private address follows where the environment runs: an
environment on the control plane is served by node `0`; one on workers keeps
ADR 064's choice. An environment on both is served by its workers, as today.

### D2. Consent is the operator's, stored and audited

On a worker the owner of the machine consents in `agent.json`, because the
worker's owner and the control plane's operator may be different people. On
the control plane they are the same person, so the consent is a record, not
a file or an environment variable:

- `settings.private_ingress`: `{ "listen": "tailscale" | "<IP>",
  "admit": ["<CIDR>", …], "tailscale_serve": bool }`, absent by default.
  `admit` is who counts as inside (D8); empty means the listen address's own
  network.
- `PUT /settings/private-ingress` and `DELETE /settings/private-ingress`,
  `SettingsWrite`, audit-logged as `PRIVATE_INGRESS_UPDATED` with the before
  and after values. `bunx @temps-sdk/cli ingress private --listen
  <tailscale|IP> [--admit <CIDR>…] [--tailscale-serve]` calls it from the CLI (the API client; the server
  binary's `temps agent ingress` configures a worker).
- `listen` accepts the same addresses ADR 064 D5 allows a worker: an address
  in `100.64.0.0/10`, RFC 1918 or `fd00::/8`, or `tailscale`, which resolves
  to the Tailscale interface's address. A public address, `0.0.0.0` and `::`
  are refused with the reason.
- `tailscale_serve: true` is the control-plane form of ADR 065 D2's
  `--tailscale-serve`: Temps may write its own entries in tailscaled's Serve
  config and `AdvertiseServices`. Without it Temps never opens the LocalAPI
  for writing (ADR 065 D2; the host-facts read in the onboarding flow stays
  without the LocalAPI).

The planner's command for a missing consent becomes this action ("Serve
private addresses from this machine", one click in the console) instead of
`temps agent ingress private --listen tailscale`.

### D3. The proxy serves a private address only on the private address it arrived at

No second listener is bound on the control plane for the `listener` way in.
The proxy already listens on 443 and 80; when its listener covers the
private address (`--tls-address` is `0.0.0.0:443`, `[::]:443`, or exactly
the private address), a connection's **local** address tells where it
arrived. Pingora exposes it per connection (the socket digest).

- **Private routes are a separate map**, as for workers (ADR 064 phase 2
  notes): the route table keeps every non-public address out of every map a
  public connection is matched against, and holds node `0`'s private routes
  in their own map.
- **That map is consulted only when** the connection's local address is the
  consented private address **and** the TCP peer is inside the address's
  range: the ranges the node admits (D8), `100.64.0.0/10` for Tailscale.
  The peer is always the TCP peer,
  never a forwarded header. Anything else gets the private listener's
  answers: `404` with `x-temps-private-ingress: no-route` for an unknown
  host, `403` with `outside-range` for a peer outside the range.
- **Why both checks.** Linux accepts a packet for any of its own addresses
  on any interface (the weak host model), so a machine on the LAN can route
  a connection to the Tailscale address through the box's LAN address. That
  connection's local address is the private one, but its peer is a LAN
  address, and the range check refuses it. A spoofed `100.64.0.0/10` source
  cannot finish the TCP handshake, because the reply is routed into the
  tailnet.
- **Port 80 only redirects** private hosts to `https://`, after the range
  check, as on a worker.
- **Certificates** are issued by DNS-01 through a DNS zone Temps manages,
  exactly as for a worker's private address; the proxy loads them like any
  other certificate. HTTP-01 is never offered for a private address.

When the proxy's listener does **not** cover the private address (bound to
another specific address), node `0` reports its private listener as not
bound, with the fix: "The proxy listens on 203.0.113.7:443 only. Restart
`temps serve` with `--tls-address=0.0.0.0:443`, or with the Tailscale
address, to serve private addresses." Pingora binds listeners only at
startup, so Temps cannot add one later (ADR-107 D3). Binding the proxy to the
wildcard address is what lets a Tailscale interface that comes up after
Temps, or changes address, work without a restart.

### D4. Tailscale Serve runs inside `temps serve`

For `tailscale_serve` and `tailscale_service` on node `0`, `temps serve` runs
the agent's Serve code (`temps-node-agent/src/tailscale_serve.rs`) in its own
process, as ADR-107 D2 runs the connector supervisor: the same LocalAPI
client, minimum tailscaled version, owned-entry rules, `If-Match` writes,
Funnel refusal and reports. There is no second implementation.

- **A loop** (`ControlPlanePrivateIngress`, next to ADR-107's
  `ControlPlaneConnectors`) reads node `0`'s Serve entries from the database
  every 15 seconds and right after one changes, applies them and writes the
  outcomes to `tailnet_serve_entries` and `node_public_ingress` row `0`, as
  the agent's heartbeat does for a worker.
- **The Serve sockets** stay `/run/temps/serve/<kind>-<id>.sock` in a `0700
  root` directory, one per address, and the socket still names the address
  (ADR 065 D3). The loop accepts on them and hands each request to the
  proxy over one more unix socket the proxy binds at startup,
  `/run/temps/private-origin.sock` (`0600 root`). Pingora can bind a unix
  socket at startup like any listener, and this one is always bound, as
  ADR-107 D3 binds the tunnel origin whether a connector exists or not.
- **On the private origin**, and only there, the proxy trusts one header the
  loop sets, `x-temps-private-address: <kind>:<id>`, and serves only that
  address's private route. `Host` is taken from the address, never from the
  request. Identity headers (`Tailscale-User-*`) are stripped there too:
  reading them for `tailnet_users` on the control plane is not in this ADR
  (D7).
- **tailscaled lets only root point Serve at a socket.** A `temps serve` not
  running as root reports it on node `0` and the plan says so, as for a
  worker.
- **The machine name and the listener are exclusive** on node `0` as on a
  worker (ADR 065 D1): Serve on the machine's `:443` takes the tailnet's
  traffic to that address, so the plan refuses one while the other serves.
  A Service has its own address and collides with neither.

### D5. The console stays off app hostnames

- The console's host is never a private route: a private address equal to
  the console's host, or to `external_url`'s host, is refused when planned.
- Temps never writes a Serve entry for the console's port. An operator who
  wants the console on the tailnet name sets that up outside Temps or uses
  the console's own listener.
- An environment's private address never makes the console reachable on the
  private path: the console is served on its own listener
  (`--console-address`), not from the proxy's route table.

### D6. Checks, reports and the planner for node `0`

- **Inside checks** (ADR 064 D7) run in the control plane's process with the
  agent's `private_check` code, every 5 minutes and after node `0`'s private
  routes change, stamped with the control plane's own time, and stored in
  `node_public_ingress` row `0`'s `private_checks`.
- **The report** for node `0` (mode, bound address and error, interfaces
  with the Tailscale one marked, the `tailscale` object of ADR 065) is
  written to row `0` by the same loop. `GET /platform/host-facts` reads its
  Tailscale facts from there once it exists.
- **The outside check** is unchanged: no tunnel publishes the hostname, it
  resolves only to private addresses, and node `0`'s private address is
  among them.
- **The planner** offers `private_worker`, `tailscale_serve` and
  `tailscale_service` with node `0` for an environment on the control plane,
  names it "this machine" in the plan, and drops the three refusals quoted in
  Context. A missing consent blocks the plan with D2's action. A proxy that
  does not cover the address blocks it with D3's message.

### D7. Not in this change

- **`tailnet_users` on the control plane** (ADR 065 D6). It needs `whois`
  for the `listener` way and identity headers on the private origin; both are
  a later amendment.
- **Connector networks** such as Pangolin, where traffic reaches Temps from
  a local connector process rather than from the visitor (D8). They are a
  way in of their own, run like ADR-107's tunnel connectors, and get their
  own ADR.
- **`--role console`.** When the proxy is another process, possibly on
  another machine, node `0` reports private ingress as unavailable with that
  reason, as ADR-107 does for connectors.
- **macOS and Windows.** tailscaled's LocalAPI socket path and the root
  requirement differ; node `0` reports Tailscale Serve as unavailable there.
  The `listener` way in works wherever the proxy runs.

### D8. Any private network, not only Tailscale

Private ingress is a network-neutral mechanism: an address to listen on and
the ranges admitted from it. Tailscale is one preset, not the model.

- **Admitted ranges, not private blocks.** A private address used to admit
  the whole private block its listener sits in, which is safe only for
  Tailscale, whose block belongs to it alone. On a WireGuard, NetBird,
  ZeroTier or LAN address it admitted all of `10.0.0.0/8` or
  `192.168.0.0/16`, and with them the LAN and the machine's container
  networks. Now a node admits:
  - the `admit` ranges the operator gave (each inside a private block), or
  - by default, the subnet of the interface holding the listen address
    (`10.8.0.0/24` for `wg0` at `10.8.0.1/24`), or
  - for Tailscale's interface, `100.64.0.0/10` and `fd7a:115c:a1e0::/48`,
    because `tailscale0` carries a `/32`.

  A point-to-point interface with no subnet and no `admit` is refused with
  the reason.
- **Containers are never inside.** The apps' containers run on the same
  machine, and a container can connect to any of the host's addresses. A
  node refuses to serve when an admitted range overlaps one of its container
  networks (Docker's networks, and container bridge interfaces such as
  `docker0` and `br-*`), naming the network and asking for a narrower range.
- **The node reports what it admits** (`admit` in its listener report), and
  the planner stores those ranges as the address's range. Workers report it
  too; for a worker that predates this, the planner keeps the private block.
- **The console names the network, not the vendor.** The status lists this
  machine's private networks (interface, address, the ranges each would
  admit, and whether it is Tailscale's), and the one-click action asks which
  one when there is more than one.
- **Tailscale-only parts stay Tailscale's**: Serve and Services (D4), the
  machine name, and `tailnet_users` (D7).

## Security

- **Audience stays enforced in routing**, as in ADR 064 D5: private routes
  are never in a map a public connection reaches, and the tunnel publisher
  never publishes them (ADR-107 D4 already reads only public hostnames).
- **Two locks, not one**: the local address and the TCP peer's range (D3).
  Neither reads a header.
- **The private origin** is a unix socket readable only by root. A container,
  a process with host networking or a local non-root user cannot reach it,
  which is why it is not a loopback TCP port (ADR 065 D3).
- **Consent is explicit and audited** (D2). Without it nothing binds and
  nothing touches tailscaled.
- **Funnel** is refused at the socket and in any Serve entry, as on workers.
- **Turning a public address private** keeps ADR 064's sensitive-action
  guard (`ChangeAddressAudience`); nothing here weakens it.

## Consequences

- A single-box install can be private-only. The first-run flow offers *Only
  my devices* on any box with Tailscale, and the planner no longer tells
  someone with one machine to add a second.
- The control plane's process gains a loop and a unix-socket listener, both
  idle until consent is given. No memory or CPU cost for installs that never
  opt in.
- A `temps serve` bound to a specific public address cannot serve private
  addresses until it is restarted with a wildcard or the private address.
- One migration replaces two foreign keys with triggers, as ADR-107 did.

## Phases

| Phase | Scope |
|---|---|
| 1 | D1 node `0`; D2 consent record and API; D3 private map on the proxy, both checks; D5; D6 checks, report, planner for `private_worker` |
| 2 | D4 Serve on node `0`: in-process loop, private origin socket, machine name and Service ways in |
| 3 | The first-run flow's private ways in, using phases 1 and 2 |

## Open questions

1. Should D2's consent also be offered as a `temps serve` flag for installs
   provisioned by scripts, writing the same record on first boot (like
   `TEMPS_ADMIN_EMAIL`), or only through the API and CLI?
2. When the proxy is bound to a specific public address (D3), should Temps
   offer a startup option to add the private address as a second listener,
   instead of asking for the wildcard?
3. Should turning private ingress on require the sensitive-action
   verification? It exposes nothing publicly, but it lets Temps change
   tailscaled's configuration.
