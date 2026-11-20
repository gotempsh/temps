# ADR-090: macOS Workers

- **Status:** Accepted
- **Date:** 2026-11-05
- **Builds on:** ADR-088 (workers join with no public address), ADR-080 (WireGuard mesh), ADR-020 (agent mTLS), ADR-059 (join preflight)

## Context

A developer with a MacBook should be able to install Temps and the Docker app
they already use (Docker Desktop, OrbStack, colima or Rancher Desktop), paste
the console's join command, and get a worker for a control plane, hosted or
self-hosted, without creating, entering or managing a VM.

Today that fails in three places:

- `temps join <url> <token>` (the outbound join of ADR-088) refuses anything
  but Linux: the worker brings up a kernel WireGuard interface itself.
- `--private-address` is no answer: a hosted plane cannot reach a laptop
  behind a home router. The worker must dial out.
- `temps agent service install` writes a systemd unit and warns that `nft`
  is missing.

### The hard constraint

On macOS every container runs in a Linux VM that the Docker app owns. The
addresses the mesh is about live in that VM: the mesh address where published
ports bind (the proxy dials `<mesh address>:<host port>`, see
`build_container_backend_addr`), the container bridges, and the per-container
network namespaces. macOS has no route to any of them. A WireGuard interface on
the Mac (a `utun`) would terminate the tunnel in the wrong machine.

### What a Docker app VM offers (verified on Apple silicon, colima 0.10, Ubuntu 24.04 VM, kernel 6.8)

- A container with `--network host` is in the VM's network namespace: it sees
  `docker0` and every bridge, and a WireGuard link it creates stays in the
  VM's kernel after the container exits.
- The VM kernel creates WireGuard links (`ip link add … type wireguard`) and
  runs nftables, with only `CAP_NET_ADMIN`.
- A port published on an address that exists only in the VM
  (`-p 10.250.77.1:18087:8080`, the address on a WireGuard link) starts and
  answers inside the VM.
- From the VM, `host-gateway` (`host.lima.internal`, 192.168.5.2) reaches
  listeners on the Mac's loopback, from host-network and bridge containers
  alike.
- colima forwards both a published `127.0.0.1:<port>` and a host-network
  listener on the VM's `127.0.0.1:<port>` to the Mac's `127.0.0.1:<port>`
  only, never to the LAN.

## Options

**(a) Native agent on the Mac, mesh sidecar in the VM.** The agent runs on
macOS and drives the Docker app as it does on Linux. One small container in the
VM's host network holds the worker's end of the mesh; the agent configures it
through `docker exec`. Two forwarders in that container connect the Mac to the
mesh: the control plane's calls to the agent, and the agent's calls to the
control plane.

**(b) Userspace WireGuard on the Mac, forwarded into the VM.** `wireguard-go`
or boringtun on a `utun` would hold the mesh on the Mac. Traffic for published
ports would then have to be relayed from the Mac into the VM for every
container and port, and the VM-side addresses would not exist, so ports bound
to the mesh address could not be published. It needs root on the Mac for the
`utun`, a per-port relay, and gives up the property that published ports bind
only to the mesh address. Rejected.

**(c) The whole Linux agent in a privileged container in the VM.** The Linux
`temps agent` would run unchanged in a `--privileged --network host --pid
host` container with the Docker socket. Everything works as on Linux, but the
agent becomes root in the VM with the Docker socket, and the VM mounts the
user's home directory (`/Users` is shared into every Docker app VM): a
compromised agent, or a control plane that tells it to bind-mount a path, can
read and write the user's files. It also needs a published Linux agent image
whose digest the macOS binary pins, and the user can no longer see or manage
the worker as a Mac process. Rejected for this ADR.

## Decision

Option (a).

### 1. The mesh sidecar

`temps-mac-mesh`, one per VM, created by `temps join` and kept running by
`temps agent`:

- **Scope:** `--network host` (the VM's namespace), `--cap-drop ALL --cap-add
  NET_ADMIN`, `--security-opt no-new-privileges`, `--read-only` with a 1 MB
  `noexec` tmpfs, 64 MB memory, 128 pids, `--restart unless-stopped`. Not
  privileged, no host path, no device, no Docker socket.
- **Image:** `temps-mac-mesh:<sha256 of its Dockerfile>`, built in the VM from
  a Dockerfile embedded in the binary: `alpine:3.22` pinned by digest plus
  `wireguard-tools-wg`, `iproute2`, `nftables` and `socat`. A changed Dockerfile
  is a new tag; an unchanged one is never rebuilt.
- **Interface:** the agent runs `wg`/`ip` in it through `docker exec`
  (`temps_wireguard::vm`), implementing the same four operations as the Linux
  netlink backend (`ensure_interface`, `reconcile_peers`, `peer_status`,
  `interface_state`) and reusing the same peer-change planner. The private key
  is passed on standard input, never on a command line, which every process on
  the Mac and in the VM can read.
- **Lockdown:** the mesh lockdown nftables table (`temps_mesh`) is the Linux
  one, rendered by the same function (`temps_network::mesh::render_lockdown`)
  and installed in the VM with `nft -f -`. It is installed before the
  interface on every tick, exactly as on Linux.
- **Forwarders** (`socat`, restarted if they exit):
  - *inbound:* `<mesh address>:<agent port>` → `host-gateway:<agent port>`,
    the agent on the Mac's loopback. The lockdown admits that port only from
    the control plane's mesh address.
  - *egress:* the VM's `127.0.0.1:3190` → `<control plane mesh
    address>:<node API port>`. The Docker app forwards the VM's loopback port
    to the Mac's loopback, where the agent calls it.

The agent pins the Docker endpoint (`docker --host unix://…`) recorded at join
in `mac-worker.json`, so a later `docker context use` cannot move a running
worker to another VM.

### 2. The agent on the Mac

- Listens on `127.0.0.1:3100`, with mutual TLS as on Linux. It reports its mesh
  listener as `<mesh address>:3100` ("covered" by the sidecar) instead of
  binding the mesh address, which does not exist on the Mac.
- Calls the control plane at `https://127.0.0.1:3190`. TLS still verifies the
  control plane against the pinned cluster CA and its reserved name, never the
  URL's address, so the forwarder is not trusted with anything.
- Publishes workload ports on its mesh address through the Docker API, as on
  Linux: the address exists in the VM, where Docker binds it.
- Runs runtime health probes from the sidecar: macOS has no route to the
  mesh address, so each probe connection is a `docker exec -i temps-mac-mesh
  socat … STDIO TCP4:<ip>:<port>` to the endpoint the inspector derived from
  the container's own published port (one argv element, no shell). The probe
  starts once `socat` reports the connection established, and dropping it
  kills the exec.
- Reports `capacity.host = {"os": "macos", "docker_app": …}` in its heartbeat.

### 3. `temps join` on a Mac

Before the join token is spent:

1. Find the Docker app: `DOCKER_HOST`, else the current Docker context, which
   must be a local `unix://` socket. Classify it by socket path (`~/.colima/<profile>`,
   `~/.orbstack`, `~/.docker/run`, `~/.rd`) and `docker info`, and read the
   VM's architecture. If the daemon does not answer, say which app to start and
   how (`colima start <profile>`, `open -a OrbStack`, …).
2. Build the sidecar image and probe the VM with a throwaway container:
   kernel WireGuard and nftables. Either missing stops the join with what to
   update; nothing is set up. A refusal to run the probe names Docker Desktop's
   Enhanced Container Isolation, which forbids it.
3. Check that the app forwards a listener on its VM's loopback to the Mac's
   loopback, the path the agent's calls to the control plane take: an
   unprivileged throwaway container answers on the VM's loopback and the join
   reads its reply on the Mac. If nothing answers, Docker Desktop needs
   *Settings > Resources > Network > Enable host networking*, the error says
   so, and the token is not spent.

Then, after mesh enrollment (ADR-088 §1): start the sidecar, prove it reaches
the Mac's loopback through `host-gateway`, bring the mesh up through it, wait
until the egress port is forwarded to the Mac, and register over it (ADR-088
§2). If any of that fails, the join removes the sidecar and the mesh interface
it created (a sidecar that was already there belongs to a worker joined
before, and stays). `--agent-address` must be `127.0.0.1:<port>`: the sidecar
reaches the Mac over IPv4.

### 4. The service: a LaunchAgent

`temps agent service install` on macOS writes
`~/Library/LaunchAgents/sh.temps.agent.plist` (`RunAtLoad`, `KeepAlive`, 10 s
throttle, `Umask` 077, logs in `~/Library/Logs/Temps/agent.log`) and loads it
with `launchctl bootstrap gui/<uid>`. Not a LaunchDaemon: the Docker app runs in
the user's login session with its socket in the user's home, so before login
there is nothing to drive, and a root daemon would reach into a user's home for
a socket it does not own. The agent runs as the user, no sudo. Install refuses
root, a binary or data directory another user could change, and a plist Temps
did not write. `uninstall --remove-mesh` also removes the sidecar and the mesh
interface from the VM.

At login the agent may start before the Docker app: it retries the sidecar with
backoff (up to a minute) instead of exiting.

### 5. Sleep and network changes

No new mechanism is needed, and none is added:

- While the Mac sleeps the VM is paused. On wake, WireGuard's 25 s keepalive
  re-handshakes; the control plane learns the new source address from it
  (roaming), whether the NAT mapping or the Wi-Fi network changed. The agent's
  loops retry on their own, and every sidecar call has a 30 s timeout so a
  paused VM fails a tick instead of hanging it.
- If the Docker app restarted its VM, the interface is gone: the next peer
  reconcile fails, the agent forgets the interface, and the tick after
  recreates it. `--restart unless-stopped` restarts the sidecar with the VM.
- The control plane marks a silent node offline after 90 s as for any worker.
  For a Mac worker the alarm is a *warning*, "Mac worker '…' is asleep or
  offline … reconnects on its own when the Mac wakes", not the critical "node
  is offline". Failover is unchanged: a sleeping Mac really does not serve.

### 6. Console

- *Add a worker node* asks "What does the new machine run? Linux / Mac" instead
  of saying the machine must run Linux. For a Mac the command installs into
  `~/.temps` and runs `~/.temps/bin/temps join …` and `… agent service install`
  as the user, no sudo, and never `--private-address`.
- The Worker Nodes list labels a Mac worker **Mac · development** (suited to
  development and personal use; it goes offline while the Mac sleeps) and
  shows an offline one as **Asleep or offline** instead of a bare *offline*.

### 7. Network policy

Workload network policy uses the nft datapath, executed in the Docker VM by the
existing `DockerVmNft` helper; the eBPF datapath is Linux only. **What is
missing on a Mac: the boot guard.** On Linux a systemd unit loads the last
policy before Docker starts. A Docker app's VM restarts containers before
anything of ours runs, so after a VM restart a restricted workload runs
unguarded until the agent completes a successful policy pass. Passes run every
15 s, but that is a polling interval, not a deadline: a control-plane outage
extends the window. The agent on a Mac neither writes nor lifts a boot guard
(there is nothing to load it).

## Security

- **Privileged scope.** The only thing in the VM with elevated rights is the
  sidecar, with `CAP_NET_ADMIN` in the VM's network namespace: it can change the
  VM's networking, nothing else. It has no Docker socket, no host path (so not
  the shared `/Users`), no other capability, a read-only root, and cannot gain
  privileges. The agent runs as the user on the Mac, not root.
- **mTLS for the agent API.** The control plane's calls go mesh → sidecar →
  `host-gateway` → the agent, with TLS end to end: the sidecar relays
  ciphertext. The agent requires the control plane's client certificate (cluster
  CA) and its bearer token, as on Linux. Any container in the VM can open a TCP
  connection to the Mac's loopback through `host-gateway` (that is the Docker
  apps' design), so the agent port is reachable from workloads at the TCP level;
  without the control plane's client certificate the TLS handshake fails. The
  same holds for any other agent listener on the Mac's loopback: one that
  trusts loopback callers (the tunnel origin listener trusts their forwarded
  visitor address) is reachable from workloads on a Mac worker, so a Mac worker
  should not also run a tunnel connector for untrusted workloads.
- **Calls to the control plane.** The egress forwarder listens on the VM's
  loopback, which containers cannot reach directly, and the mesh lockdown keeps
  dropping forwarded traffic from containers into the tunnel, as on Linux. But
  the Docker app forwards that loopback port to the Mac's loopback, and
  workloads reach the Mac's loopback through `host-gateway`: **unlike on Linux,
  a workload on a Mac worker has a network path to the control plane's node
  API** (through the Mac, the forwarder and the mesh). The barrier is the node
  API's own authentication (the agent token, and TLS to the cluster CA), not
  the network. The agent verifies the control plane by the cluster CA and
  reserved name, so a workload cannot stand in for it either.
- **Nothing on the LAN.** No listener binds a Mac interface other than
  loopback; the Docker apps forward VM loopback ports to the Mac's loopback
  only. Published workload ports bind the mesh address, inside the VM. The
  WireGuard socket answers only authenticated packets. An app configured to
  give the VM a LAN address (colima `--network-address` in bridged mode) still
  exposes nothing: the sidecar binds the mesh address and the VM's loopback,
  never `0.0.0.0`.
- **Policy window.** After the Docker app's VM restarts, a restricted workload
  runs without network policy until the agent completes a successful policy
  pass, which a control-plane outage can delay: a Mac has no boot guard to
  close that window (see Network policy). Keeping restricted workloads stopped until the policy
  is in place would need a start-control mechanism this ADR does not add.
- **The private key** stays in `~/.temps/wireguard/private.key` (0600) and in
  the VM's kernel; it crosses `docker exec` on standard input and is written to
  a 0600 temporary file in the sidecar's tmpfs only for `wg set`, removed on
  exit.
- **Fails closed.** The lockdown is installed before the interface, every
  tick. No WireGuard or no nftables in the VM stops the join before the token is
  spent. A missing sidecar makes every mesh operation fail with an error naming
  it, and the agent retries: it never falls back to binding `0.0.0.0` or
  calling the control plane outside the mesh. A container named
  `temps-mac-mesh` that Temps did not create is never touched.

## Consequences

- A Mac joins a hosted or self-hosted control plane with the console's command,
  no VM to manage and no sudo.
- Mac workers do not take part in the cross-worker container overlay (VXLAN
  over the mesh): their containers reach other workers through published ports
  and the proxy, not by container address. The agent reports the overlay as
  unavailable.
- A Mac worker cannot be the mesh hub.
- Docker Desktop needs host networking enabled for the egress forwarder; colima,
  OrbStack and Rancher Desktop forward host-network ports already. Only colima
  was tested end to end; that Docker Desktop forwards a host-network listener on
  the VM's loopback to the Mac's loopback only (not to every interface) is
  still to be verified.

## Testing

Unit tests cover everything that parses, plans or renders (the `wg` dump and
`ip` parsers, the configure script, peer arguments, route collision, Docker app
classification, sidecar arguments and spec, the LaunchAgent plist, the console
command and labels). CI has no macOS runner with a Docker VM, so the sidecar,
the VM probes, launchd and the end-to-end join are not covered there;
`scripts/macos-worker-e2e.sh`, added with the implementation (#1824), is the
manual test for a real Mac.

It was run on an Apple silicon Mac with colima: a control plane built from this
change in one colima VM (as a Linux host, its listeners and WireGuard port
forwarded to the Mac's loopback), and the Mac joined with the console's Mac
command, using a second colima VM as its Docker app. The worker reached the
control plane only by dialing out through the Mac (the control plane saw it
behind a NAT mapping and could not reach it directly). Results: the node Active
with heartbeats and `capacity.host = {"os":"macos","docker_app":"colima"}`; an
image deployed to it answered through the control plane's proxy, and the app saw
the control plane's mesh address as the caller; every check of the script
passed, including a LaunchAgent restart and a moved WireGuard source port;
freezing the agent for about five minutes raised the "asleep or offline" warning, and
the node returned to Active on its own when resumed.
