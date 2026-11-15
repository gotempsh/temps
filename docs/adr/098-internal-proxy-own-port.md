<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-098: The worker's internal proxy listens on its own port, and containers' port 80 is redirected to it

**Status:** Proposed — 2026-11-09

**Amends:** [ADR-095](./095-internal-dns-observability.md)

Related: ADR-011 and ADR-024 (`*.temps.local` names point at the node's bridge
gateway), ADR-059 (the overlay's host-side names), ADR-064 (the worker's
private listener).

## Context

Every `<environment>.<project>.temps.local` name resolves, on a worker, to the
gateway address of the node's overlay bridge (`10.205.0.1` in the examples
below; each node has its own). Containers open plain HTTP on port 80 of that
address, and `temps agent` answered there by binding `<bridge address>:80`
itself (`crates/temps-node-agent/src/internal_proxy.rs`).

Port 80 of a worker belongs to whatever the operator runs there. A web server
or another Temps install listening on `0.0.0.0:80` owns port 80 of every
address, the bridge's included, so the agent's bind failed and internal routing
stayed down for as long as that program ran. The agent retried with backoff and
told the operator to free port 80 (ADR-095 §5). On a host that shares Docker
with a separate Temps control plane, that advice is wrong: the operator cannot
and should not give up port 80. The reverse also happened: while the agent held
`<bridge address>:80`, a web server that started later could not bind
`0.0.0.0:80`.

The worker must not depend on host port 80 at all, and containers must keep
calling `http://<environment>.<project>.temps.local` unchanged: their images,
configuration and the zone's records all assume port 80.

## Decision

### D1. The proxy listens on port 591 of the bridge address

`INTERNAL_PROXY_PORT = 591`, bound on the bridge address only, never on
`0.0.0.0`.

- **Below 1024.** Only root (or `CAP_NET_BIND_SERVICE`) may bind it. The
  redirect (D2) outlives an agent restart by design (D3), and whatever holds the
  redirected port receives every internal request, with whatever credentials
  the apps send each other. With a high port, any unprivileged local user could
  bind it during the restart and receive them.
- **Not a high port for a second reason.** Temps publishes host ports itself:
  deployment ports come from the kernel's ephemeral range (`0.0.0.0:0`), and
  database cluster blocks start at 6000 and climb ten at a time with no upper
  bound. Any fixed high port is one Temps can eventually take on its own host.
- **591 specifically.** IANA registers it as `http-alt` (FileMaker web
  publishing). No Linux daemon defaults to it, nothing in Temps uses it, and the
  name says what it carries: HTTP on another port.

If it is held, the agent says by what to look for and what to do
(`sudo ss -ltnp 'sport = :591'`, stop it or move it), and falls back (D4).

### D2. One nftables rule redirects containers' port 80 to it

The agent installs its own table, `ip temps_internal_proxy`, replaced in one
transaction:

```nft
add table ip temps_internal_proxy
delete table ip temps_internal_proxy
add table ip temps_internal_proxy
add set ip temps_internal_proxy agent_alive { type ipv4_addr; flags timeout; }
add element ip temps_internal_proxy agent_alive { 10.205.0.1 timeout 90s }
add chain ip temps_internal_proxy prerouting { type nat hook prerouting priority -110; policy accept; }
add rule ip temps_internal_proxy prerouting ip daddr 10.205.0.1 tcp dport 80 ip daddr @agent_alive counter dnat to 10.205.0.1:591 comment "temps-internal-proxy-redirect-v1"
```

**Scope.** Both must hold: the packet is addressed to this node's bridge
address, and is TCP to port 80. That is exactly the set of connections the old
direct bind on `<bridge address>:80` accepted, so who can reach the proxy does
not change. Nothing else is touched:

- *Host-originated traffic* is not redirected: its connection starts in
  `output`, and by the time a packet loops back through `lo` into `prerouting`
  that connection is already tracked and its NAT already decided, so no
  `prerouting` NAT rule applies to it. A process on the host that calls
  `10.205.0.1:80` still reaches whatever holds port 80 there (the netns test
  asserts it).
- *Other addresses and ports* of the host, including the bridge address on any
  other port (the resolver on 53) and port 80 of every other address, are not
  matched.
- *Any interface.* The rule does not match the inbound interface. Most
  callers arrive on the overlay bridge, but not all: a managed service whose
  overlay attach failed still gets the node's resolver and calls the bridge
  address through its own Docker bridge, and a node without an overlay name
  has no `br-<overlay name>` to match. An interface match would leave those
  calls on port 80 while the node reported *Redirected*.
- *microVM replicas* (ADR-055) are not served. They skip Docker networks,
  resolve through public resolvers, and the Firecracker bridge drops every
  guest-to-host packet in `INPUT`, whatever the port, so a guest's packet to
  the bridge address is dropped after the rewrite as it was before. The proxy
  identifies callers through the local Docker daemon and would refuse a guest
  anyway.
- *IPv6*: the bridge's internal names are IPv4 only, as before.
- *Callers that are not this node's containers* (another node's containers
  over the VXLAN, a mesh peer, the LAN) reach the proxy exactly as they reached
  the old bind, and it refuses them as it always did: they are unknown to this
  node's Docker daemon.

**Its own table, so Docker cannot clobber it and it cannot clobber Docker.**
Docker never touches a table it did not create (with iptables-nft, iptables
legacy or its nftables backend), and this table holds nothing but the one rule.

**Ordering against Docker's NAT.** Docker's DNAT for published ports runs at
nat `prerouting` priority -100 (`dstnat`) in all three backends. This chain
runs at -110, first. The kernel's NAT core binds a connection to the first
mapping a NAT chain gives it and skips later NAT chains for it, so a Docker
DNAT for a container that publishes port 80 cannot take these connections.

**The source address is preserved.** DNAT rewrites only the destination, so
the proxy's caller identity (the source address, matched to a container
through the local Docker daemon) is unchanged.

### D3. Lifecycle

1. The agent binds `<bridge address>:591`.
2. Only then does it install the redirect; only after the redirect is in
   place does it report internal routing healthy. It never installs a redirect
   to a port it does not hold: if the bind fails, the table is removed.
3. Every 30 s it replaces the table again. That restores the rule after a
   ruleset flush, and refreshes the liveness element. The rule matches the
   bridge's address, not the interface, so a bridge recreated with the same
   address is covered with no reinstall. A listener that exits on its own
   wakes the supervisor at once and is started again; while the proxy's cannot
   be, the table is removed (step 2). An agent whose bridge address cannot carry
   the redirect (IPv6) removes a table an earlier agent left, then falls back.
4. **Liveness.** The rule matches only while the bridge address is in
   `agent_alive`, whose element expires 90 s after the last refresh. An agent
   that stops refreshing (stopped, crashed, downgraded to a version without this
   ADR, or its binary deleted without an uninstall) stops redirecting by itself.
   Port 80 of the bridge then reaches whatever the host routes there, which is
   what an older agent binds directly, so a rollback needs no operator action.
   Within those 90 s nothing holds port 591 and the calls are refused, rather
   than delivered to the program on port 80: an agent restart never sends
   internal requests to a program that is not Temps.
5. `temps agent service uninstall` deletes the table after stopping the
   agent and before removing its unit, and gives `nft` 10 s.

### D4. Fallback: the old direct bind

When the redirect cannot be installed (no `nft`, a kernel without nftables NAT,
no permission) or port 591 is held, the agent tries the old direct bind on
`<bridge address>:80`. While that serves, internal routing works and the node
reports that the redirect is missing and why. As soon as a later pass installs
the redirect, the agent releases port 80. When both fail, routing is down and
the error says what to fix first: install nftables, or stop what holds port
591. It never says to free port 80.

An agent on another OS than Linux (macOS workers, ADR-090) keeps the direct
bind only, as before.

### D5. What the node reports

`capacity.internal_proxy` in the heartbeat keeps its fields with their meaning
(`address` is what containers call, `<bridge>:80`; `listening` says whether
those calls are served; `error` says why not), so an older control plane reads
a new agent correctly. It adds:

| Field | Values |
|---|---|
| `mode` | `redirect` (port 591 and the redirect), `direct` (the fallback bind on port 80), `down` |
| `proxy_address` | `<bridge>:591` while the agent holds it |
| `redirect` | `installed`, `missing`, `unsupported` (not Linux) |
| `redirect_error` | why the redirect is missing, also while `direct` serves |

The console's DNS page (ADR-095 §5) shows, under **Internal routing**:
*Redirected* (healthy), *Listening* (an agent older than this ADR, or one off Linux), *Direct on port 80* (a warning: routing works, but
the agent holds port 80 of the bridge address and the reason the redirect is
missing is shown), *Down* with the error, or *Not reported*. The node API
returns the same fields. `dns_internal_proxy_down` (ADR-095 §6) still fires
only when calls are not served (`listening: false`), and its message no longer
names a port to free.

### D6. Join and service-install checks

`temps join` and `temps agent service install` warn when port 591 is held, and
check port 80 only for ingress modes that listen on it.

## Consequences

- A worker whose port 80 is taken by another program serves internal routing
  with no operator action, and that program keeps port 80.
- A worker whose port 80 was free changes only in that the bridge address's
  port 80 is now redirected: the agent holds `<bridge>:591`, not
  `<bridge>:80`.
- One nft transaction every 30 s per worker, a handful of milliseconds of CPU;
  no per-request cost (the rule is evaluated once per new connection by
  conntrack).

### Not covered

- **Host firewalls that admit container-to-host traffic by port.** The
  `INPUT` hook sees the rewritten port, 591. A firewall that admits traffic
  arriving on the overlay bridge by interface (`ufw allow in on br-temps0`) is
  unaffected; one that admits only ports 53 and 80 from it now drops the
  redirected calls. The agent cannot see another table's verdict, so it
  reports the redirect installed.
- **Port rules of the nftables policy backend toward the bridge address.** A
  workload's egress chain runs in `INPUT` too and matches the rewritten port,
  591, both ways:
  - a restricted workload whose policy allows the bridge address on port 80
    explicitly must allow 591 as well;
  - an explicit *deny* of TCP port 80 (to the bridge address, or to everyone)
    under an open egress profile no longer stops that workload's internal
    calls through the proxy. The proxy still serves a caller only names of its
    own project, and the same workload can reach those apps' own ports over
    the overlay unless the policy denies them too.

  The fix belongs in the policy compiler: match a connection's original
  destination port (`ct original proto-dst`) for egress toward the node's
  own addresses. It is a follow-up, not part of this ADR. The eBPF backend
  admits the node's own addresses on every port and is unaffected.
- **A bridge address that changes** after an allocation move: the proxy keeps
  the first address it bound, as before this ADR. Before the first network
  sync (a cold start with the control plane unreachable) the agent reads the
  address of the overlay bridge named in its `agent.json`, never the default
  `br-temps0` when `temps join` chose another name: on a host shared with
  another Temps install that bridge is the other install's, and redirecting
  its port 80 would break its routing.
- **Two agents on one host.** The table is per host, as the old bind on
  `<bridge address>:80` effectively was; a second agent would replace the
  first one's table. One agent per host is the supported setup.
- **`net.ipv4.ip_unprivileged_port_start` lowered below 592** (some rootless
  container setups set it to 0) removes D1's privileged-port property.

## Alternatives considered

- **A high port.** Rejected for the two reasons in D1.
- **Keep binding port 80 when it is free.** Holding `<bridge>:80` is what keeps
  an operator's later `0.0.0.0:80` from binding; the worker must not depend on
  it.
- **TPROXY instead of DNAT.** It would leave the destination port at 80, so
  port-based `INPUT` rules and network policies would see what they see today.
  It needs the `nft_tproxy` module (missing from some enterprise kernels) and
  an `IP_TRANSPARENT` listener, and is unfamiliar to most operators reading the
  ruleset. DNAT works wherever nftables NAT does; the two limitations it brings
  are listed above.
- **DNAT to loopback.** Needs `route_localnet=1` on the bridge, which lets
  anything on that interface reach every loopback-only service of the host.
- **A rule in the overlay's `temps_network` table.** That table belongs to the
  overlay reconciler, which replaces it on its own schedule and marker; the
  redirect's lifetime is the proxy's, and its failures must not fail the
  overlay's.
- **Match only the overlay bridge (`iifname "br-<overlay name>"`).** Misses
  containers that call the bridge address from another Docker network (D2,
  *Any interface*) while the node reports *Redirected*, and depends on a name
  the agent may only learn from the control plane. Matching the address
  alone accepts exactly what the old bind accepted.
