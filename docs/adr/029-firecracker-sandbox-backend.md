# ADR-029: Firecracker MicroVM Sandbox Backend Alongside Docker

**Status:** Proposed
**Date:** 2026-07-19
**Author:** David Viejo

## Context

Sandboxes run the least-trusted code in Temps: agent-driven builds, arbitrary user commands via `/v1/sandboxes/*/exec`, and AI CLIs that execute instructions which may be attacker-influenced (see the threat model in ADR-013). Today every sandbox is a Docker container (`crates/temps-agents/src/sandbox/docker.rs`, `DockerSandboxProvider`), which means every sandbox shares the host kernel. The isolation boundary is namespaces + cgroups + a capability drop (`CapDrop=ALL`, `CapAdd=[CHOWN,FOWNER]`). That is a good container posture, but a single kernel LPE or container-escape CVE compromises the host and every co-tenant sandbox on it. For multi-tenant standalone sandboxes (`crates/temps-sandbox`, the Vercel-SDK-compatible API), that is the wrong ceiling.

Firecracker gives us a hardware-virtualized boundary (KVM) with microVM economics: ~125 ms boot, ~5 MiB overhead per VM, a minimal device model (virtio net/block/vsock only), a built-in jailer, and snapshot/restore. It is the isolation layer behind Fly.io machines, Vercel sandboxes, and AWS Lambda — the exact workload shape we serve.

We deliberately prepared for this. ADR-010 (provider boundary traits) established exactly one `SandboxProvider` trait (`crates/temps-agents/src/sandbox/mod.rs`) with consumers holding only `Arc<dyn SandboxProvider>`, and names Firecracker explicitly as a future backend. Two impls exist today: `DockerSandboxProvider` and `LocalSandboxProvider` (`local.rs`, dev-only, gated behind `TEMPS_ALLOW_LOCAL_SANDBOX=1`). The HTTP layer (`temps-sandbox`), the executor, and the autofixer are already backend-agnostic — `temps-sandbox/Cargo.toml` has no `bollard` dependency, and `scripts/check-provider-boundary.sh` enforces the boundary in CI.

What is *not* in place:

1. **Backend selection is hardcoded.** `temps-agents/src/plugin.rs::register_services` pings Docker via `bollard::Docker::connect_with_local_defaults()`; on success it registers `DockerSandboxProvider`, otherwise it falls back to local (if allowed). Exactly one provider is ever registered; there is no way to run two side by side.
2. **Exec and filesystem operations assume a container runtime.** `exec`/`exec_streamed` use bollard `create_exec`; `read_file`/`write_file` use bollard tar streams (`put_archive`). Firecracker has no exec API — a microVM is a black box unless we put an agent inside it and give it a transport.
3. **Networking assumes Docker's bridge.** Sandboxes join `temps-sandbox-net` and the preview gateway resolves `temps-sandbox-<sid>:<port>` via Docker's embedded DNS. Firecracker VMs use TAP devices; nothing resolves their names.
4. **No requirement checks.** Firecracker needs `/dev/kvm`, a Linux host, and a guest kernel image. None of our hosts are validated for this today, and macOS dev machines can never run it.

This ADR decides how a Firecracker backend joins the codebase such that Docker and Firecracker sandboxes coexist on the same host, behind the same `/v1/sandboxes` API and the same `SandboxProvider` trait, selectable per sandbox.

## Decision

Add a third `SandboxProvider` implementation, `FirecrackerSandboxProvider`, plus a thin routing provider that lets both backends be live simultaneously. Docker remains the default; Firecracker is opt-in per host and selectable per sandbox.

### 1. Crate layout: a new impl, not a new abstraction

- `crates/temps-agents/src/sandbox/firecracker/` — `FirecrackerSandboxProvider` implementing `SandboxProvider`, sibling to `docker.rs` and `local.rs`, per ADR-010 ("new backends add an impl"). Submodules: `vm.rs` (Firecracker process + API socket lifecycle), `rootfs.rs` (image → ext4 conversion and cache), `net.rs` (TAP/bridge/DNS), `vsock.rs` (guest agent transport).
- The trait does not change shape. Every method maps:

| `SandboxProvider` method | Docker today | Firecracker |
|---|---|---|
| `create` | `create_container` + start | build rootfs overlay, spawn `firecracker` via jailer, boot |
| `exec` / `exec_streamed` | bollard `create_exec` | RPC to in-guest agent over vsock |
| `read_file` / `write_file` / `write_directory` | bollard tar streams | same RPC channel, chunked frames |
| `is_alive` | container inspect | VM process alive + agent `PING` |
| `stop` / `start` / `restart` | stop/start container | graceful shutdown via agent, re-boot from persisted disks |
| `destroy(purge_volumes)` | remove container (+ volumes) | kill VM, delete jail dir (+ per-run disk) |
| `recover` / `recover_by_name` | find container by name | find state dir under `$TEMPS_DATA_DIR/firecracker/vms/<label>` |
| `kill_processes(signal)` | exec `kill` | agent `KILL` RPC |
| `image_status` / `rebuild_image*` | BuildKit build | BuildKit build **+ rootfs conversion step** |
| `is_available` | Docker ping | `/dev/kvm` rw, `firecracker` binary, guest kernel present |

### 2. Side-by-side dispatch: `RoutingSandboxProvider`

ADR-010's invariant — consumers hold exactly one `Arc<dyn SandboxProvider>` — stays intact. We register a `RoutingSandboxProvider` that itself implements `SandboxProvider` and owns the concrete backends:

```rust
pub struct RoutingSandboxProvider {
    backends: HashMap<SandboxBackend, Arc<dyn SandboxProvider>>, // docker, firecracker, local
    default: SandboxBackend,
}
```

- `SandboxCreateConfig` gains `backend: Option<SandboxBackend>` (`#[serde(default)]`, `None` = use the configured default). `create` dispatches on it and fails with a clear `AgentError` if the requested backend is not registered/available on this host.
- Handle-based methods (`exec`, `read_file`, `stop`, …) must know which backend owns a handle. We encode it in `SandboxHandle.sandbox_name`: Docker keeps `temps-sandbox-<label>`; Firecracker uses `temps-fcsandbox-<label>`. The router dispatches on prefix — no trait change, no per-call DB lookup, and `StandaloneSandboxRegistry`'s in-memory map keeps working unchanged.
- `recover_by_name` tries the owning backend by prefix; bare labels (legacy callers) fan out to each backend in order. This preserves the registry's restart-recovery path (`recover_active()` on startup).
- The backend is also persisted in the `sandboxes` row (`metadata` JSON, key `backend`) so `list_for_user` can display it and so recovery after data loss is diagnosable. The JSON column means no migration.

### 3. Selection surface

- **Host config:** `AgentSandboxSettings` (`crates/temps-core/src/app_settings.rs`, JSON under the `agent_sandbox` settings key — no migration) gains:
  - `sandbox_backend: "docker" | "firecracker"` — default backend for new sandboxes (default `"docker"`).
  - `firecracker: FirecrackerSettings { kernel_image, rootfs_cache_dir, enable_snapshots, jailer_uid_range }` — all optional with sane defaults under `$TEMPS_DATA_DIR/firecracker/`.
- **Registration** (`temps-agents/src/plugin.rs::register_services`): probe Docker as today; additionally probe Firecracker (`is_available`: `/dev/kvm` accessible, binary on path or bundled, kernel image present). Register every backend that probes healthy into the router. If Firecracker is configured as default but unavailable, log loudly and fall back to Docker rather than failing startup.
- **Per-sandbox API:** `POST /v1/sandboxes` gains an optional `backend` field on `CreateSandboxRequest`. Per ADR-009 this is a non-breaking addition (optional request field); DTOs keep `#[serde(deny_unknown_fields)]`, and the Vercel-compat surface (`tests/vercel_compat.rs`) is untouched because the field is optional. Requesting `"firecracker"` on a host without KVM returns 422 with a descriptive error, not a silent downgrade — isolation level is a security property the caller asked for.

### 4. Guest images: one build pipeline, two artifacts

We keep the existing runtime images (`ghcr.io/gotempsh/temps-sandbox-{runtime}:{version}`, built via bollard BuildKit from `dockerfile_for_runtime`) as the single source of truth, and derive Firecracker rootfs images from them:

1. Build (or pull) the OCI image exactly as today — same Dockerfile, same embedded `temps-pty-agent` bundle, same tools (`dtach`, `socat`, git-credential shim).
2. Flatten it: `docker create` + `export` → directory tree, then `mkfs.ext4 -d <dir> rootfs.ext4` (e2fsprogs populates without loop mounts or root).
3. Overlay an init: a static `temps-vm-init` (tiny Rust binary, PID 1) that mounts `/proc` `/sys` `/dev`, brings up `eth0` from kernel boot args, mounts the per-run home disk at `/home/temps`, then execs the image's existing `sandbox-entrypoint.sh`.
4. Cache the result keyed by image digest under `firecracker.rootfs_cache_dir`. Conversion happens lazily on first use per digest; `image_status`/`rebuild_image_with_progress` report a `converting` phase so the UI shows honest progress.

The guest kernel is a pinned `vmlinux` (minimal config: virtio-net/blk/vsock, ext4, no module loading) built in CI and published alongside the sandbox images; downloaded to `$TEMPS_DATA_DIR/firecracker/kernel/` on first use, digest-verified.

**Arbitrary OCI images are supported, same as Docker.** The Docker backend already accepts custom images two ways — `runtime: "custom"` + `custom_image` in `AgentSandboxSettings`, and per-request `image` on `POST /v1/sandboxes` (pulled on demand, `docker.rs:1314–1335`). The Firecracker backend keeps that contract; the conversion pipeline is image-agnostic:

- **The Docker daemon is the image toolchain in v1.** Pull, build, inspect, and `export` all go through the already-present bollard client. This costs nothing new — a side-by-side host has Docker by definition — and avoids reimplementing registry auth, since the flatten step works on any image Docker can materialize (private registries included, using the credentials Docker already holds). A Docker-less OCI pull path (`oci-distribution` crate) is explicitly future work for Firecracker-only hosts.
- **OCI config metadata survives the flatten.** `docker export` discards `ENV`, `USER`, `WORKDIR`, `ENTRYPOINT`/`CMD`. Conversion captures them from image inspect into `/etc/temps/oci-config.json` inside the rootfs; `temps-vm-init` applies them (env, setuid to the image's user, chdir, exec entrypoint) so an image behaves identically under both backends.
- **The agent is always injected, never assumed.** Temps runtime images embed `temps-pty-agent`, but arbitrary images don't — and unlike Docker (where `docker exec` works regardless), the agent is the *only* transport into a microVM. Conversion therefore overlays a **statically linked musl build** of the agent + `temps-vm-init` into every rootfs, so glibc-less, musl, and distroless images all work. If the image defines its own entrypoint, init runs the agent as a supervised sidecar and the entrypoint as the main process, mirroring `sandbox-entrypoint.sh` semantics.
- **Architecture must match.** Conversion rejects images whose platform differs from the host (`x86_64`/`aarch64`) with a descriptive error — there is no emulation path in a microVM.
- **Cache and invalidation.** Rootfs artifacts are keyed by image *digest*, not tag, so mutable tags (`:latest`) reconvert when the pulled digest changes; the digest check happens at the same point `ensure_image_for_runtime` decides to pull today. A size cap + LRU eviction on `rootfs_cache_dir` keeps custom-image churn from filling the disk, and `destroy`-time accounting reports cache usage through `image_status`.

Per-sandbox writable state mirrors Docker's volume semantics: the rootfs is attached as a read-only base with a copy-on-write overlay (per-VM sparse ext4 upper layer), and the per-run home volume (`temps-sandbox-home-{run_id}` in Docker) becomes a per-run `home-{run_id}.ext4` sparse disk attached as a second block device — retained across stop/start, deleted only on `destroy(purge_volumes: true)`. `host_work_dir` bind mounts have no microVM equivalent; the workdir is seeded by the same tar-over-agent path used for `SandboxSource::{Git,Tarball}` today, and `workspace_volume` maps to a third disk.

### 5. In-guest agent over vsock

Non-interactive exec, file I/O, and the interactive PTY protocol all need a transport into the VM. We reuse what exists:

- `temps-pty-agent` already speaks a length-prefixed binary framing (`u32 len | u8 type | payload`, 4 MiB max frame) over `/run/temps-pty/agent.sock` inside the sandbox, supervised by `sandbox-entrypoint.sh`. In the Firecracker guest, the same agent additionally listens on **vsock** (guest CID, port 5000 for PTY, port 5001 for a new exec/fs service). The protocol module (`temps-pty-agent/src/protocol.rs`) is transport-agnostic already; this adds a listener, not a dialect.
- The exec/fs service is new but small: `EXEC` (argv, env, uid, stream stdout/stderr frames, exit code), `WRITE_FILE`/`READ_FILE`/`MKDIR` (chunked), `KILL`, `PING`. `FirecrackerSandboxProvider` implements the `SandboxProvider` exec/fs methods as RPCs over the Firecracker host-side vsock Unix socket (`v.sock` in the jail dir, `CONNECT <port>` handshake per Firecracker's hybrid vsock spec).
- Host-side terminal bridging connects to the same vsock instead of the `docker exec` + `socat` hijack (ADR-008). The frame protocol upstream of the transport is byte-identical, so the workspace terminal UI does not change.

### 6. Networking and preview routing

- Each VM gets a TAP device enslaved to a dedicated Linux bridge `temps-fc-br0` with a private subnet, created/managed by `firecracker/net.rs` (address allocation persisted in the VM state dir). Guest IP/gateway/DNS are passed via kernel boot args and applied by `temps-vm-init` — no DHCP daemon.
- **Preview routing:** the preview gateway currently resolves `temps-sandbox-<sid>` via Docker's embedded DNS on `temps-sandbox-net`. We do not try to make Firecracker VMs visible to Docker DNS. Instead the control-plane DNS resolver (ADR-024, `temps-dns-resolver`) serves `temps-fcsandbox-<sid>` records from the router's address table, and the preview gateway consults it. Docker sandboxes keep the existing path; the gateway needs one additional resolver, not a new routing model.
- **Egress parity with ADR-013:** the TAP bridge is NAT'd through the host, and when the sandbox egress credential proxy ships, the same chokepoint applies — the bridge's forward chain allows traffic only to `temps-sandbox-proxy`, and guests receive the same `HTTPS_PROXY`/phantom-credential environment. `network_mode` maps as: `"full"` → NAT'd bridge, `"none"` → no TAP device at all (stronger than Docker's `none`), `"host"` → rejected (meaningless and dangerous for a VM; explicit error).

### 7. Lifecycle mapping and jailer

- Every VM runs under Firecracker's `jailer`: chroot into `$TEMPS_DATA_DIR/firecracker/vms/<label>/`, dedicated unprivileged uid/gid from `jailer_uid_range`, cgroup v2 limits mirroring `cpu_limit`/`memory_limit_mb`/`pids_limit` from `SandboxCreateConfig`, seccomp on (Firecracker default).
- `stop` = graceful shutdown RPC (agent syncs disks, guest powers off) with SIGKILL-the-VMM timeout; `start` = re-boot from the persisted overlay + home disks; `restart` = the two composed. This matches the Docker backend's stop/start semantics as observed by the `sandboxes.status` state machine (`running` → `stopped` → `running` → `destroyed`) with zero handler changes.
- **Pause/resume v2:** Firecracker snapshot/restore (memory + device state) can make `pause`/`resume` near-instant and is the door to warm-pool boot times, but snapshot compatibility is pinned to Firecracker version and requires re-seeding guest entropy and clock on restore. v1 implements pause as `stop`; snapshots ship behind `firecracker.enable_snapshots` once we have restore tests.
- The `SandboxExpirationSweeper` and `JobTracker` operate purely through the trait and the DB — no changes.

### 8. Operator setup: `temps firecracker setup`, doctor checks, settings UI

Firecracker must be as close to one-command enablement as the rest of Temps (`curl | bash`, `temps setup --auto`). An operator should never hand-assemble a VMM toolchain. Three surfaces, reusing existing CLI machinery:

**`temps firecracker setup`** — a new `temps-cli` command (sibling of `commands/setup.rs`) that provisions everything, idempotently:

1. **Preflight.** CPU virtualization flags (`vmx`/`svm` in `/proc/cpuinfo`), `/dev/kvm` exists and is read-writable by the temps user (if not: print the exact remediation — `modprobe kvm_intel|kvm_amd`, udev rule / `kvm` group membership, or "enable nested virtualization on this cloud instance" with per-provider hints). Kernel ≥ 4.14, `x86_64` or `aarch64`. Fail here with actionable text; never half-provision.
2. **Binaries.** Download the pinned Firecracker release (`firecracker` + `jailer`) from the official `firecracker-microvm/firecracker` GitHub releases into `$TEMPS_DATA_DIR/firecracker/bin/`, sha256-verified — the same download-and-verify flow `commands/upgrade.rs` already implements against `GITHUB_RELEASES_API`. The supported Firecracker version is a pinned constant in `temps-agents` (like `SANDBOX_IMAGE_VERSION`), bumped deliberately since snapshot format and API are version-coupled. We do not use a distro package or whatever `$PATH` happens to contain.
3. **Guest kernel.** Download the CI-built `vmlinux` for the host arch from Temps release assets, digest-verified, into `$TEMPS_DATA_DIR/firecracker/kernel/`.
4. **Network.** Create `temps-fc-br0`, assign the private subnet, install NAT/forward rules (nftables, tagged for idempotent re-application), enable `net.ipv4.ip_forward`. Requires root — `setup` detects non-root and prints the `sudo temps firecracker setup --network-only` step rather than failing opaquely.
5. **Jailer identities.** Allocate the unprivileged uid/gid range (`jailer_uid_range`), verify no collision with existing system users.
6. **Smoke test.** Boot a minimal VM end-to-end: kernel + stock rootfs, agent `PING` over vsock, exec `true`, teardown. Only on success does setup write `sandbox_backend` availability into `AgentSandboxSettings` and report the backend as enabled. A failed smoke test leaves Docker as the active default and prints the failing stage.

`--check` runs stages 1 and 6's probes without mutating anything; `--uninstall` removes rules, bridge, binaries, and cache.

**`temps doctor`** gains a Firecracker section (same `CheckResult` report as the existing Docker checks in `commands/doctor.rs`): KVM access, binary presence + version-pin match, kernel image digest, bridge + NAT rules present, rootfs cache size, count of running/orphaned VMs (state dirs without live processes). Doctor is read-only; it points at `temps firecracker setup` for every failure it finds.

**Settings UI.** The sandbox settings page grows a backend selector (Docker / Firecracker) that renders the live probe detail from `is_available` — not just a boolean, but which check failed and why, mirroring doctor's output via a small `GET /v1/sandboxes/backends` endpoint (per-backend: `available`, `version`, `checks[]`). An "Enable Firecracker" action runs the server-side equivalent of `setup` stages 2–6 with streamed progress (the `rebuild_image_with_progress` pattern). Hosts that can never run it (macOS, no-KVM VMs) show the selector disabled with the reason inline instead of hiding it.

`register_services` stays passive: it probes and registers what is already provisioned but never downloads or mutates the host at boot — provisioning happens only through the explicit setup paths above.

## Consequences

### Positive

- Hardware-virtualized isolation for untrusted sandbox workloads; a kernel exploit inside a sandbox no longer means host compromise. This materially upgrades the multi-tenant story for the standalone sandbox API.
- ADR-010 pays off: `temps-sandbox` handlers, services, registry, executor, and autofixer are untouched except one optional DTO field. The diff is almost entirely inside `temps-agents`.
- One image pipeline. Runtime images, the pty-agent bundle, entrypoint, and credential shims stay single-sourced; the rootfs is a derived, cached artifact.
- Docker remains the default and the dev-machine path (macOS, no-KVM CI). Nothing about existing deployments changes until an operator opts in.
- The vsock exec/fs agent removes the Docker-specific tar-stream and exec-hijack quirks from the hot path for Firecracker sandboxes, and gives us a second, cleaner transport implementation of the same protocol.

### Negative

- **Linux + KVM only.** Bare-metal or nested-virt-enabled hosts. Many cloud VMs (default EC2 non-`*.metal`, most budget VPSes) cannot run it; `is_available` and the settings UI must communicate this clearly.
- A real VMM fleet to operate: guest kernel builds/pinning, rootfs cache invalidation, TAP/bridge lifecycle, jailer uid allocation, orphaned-VM cleanup on crash. `recover_by_name` for VMs is state-dir-based and needs careful crash-consistency (a stale dir must not resurrect a dead VM as "alive").
- Boot (~125 ms + agent readiness) and image conversion (first use per digest, seconds-to-tens-of-seconds) are new latencies Docker doesn't have. Conversion is cached and reported, but the first Firecracker sandbox per image is visibly slower.
- Two backends double the lifecycle test matrix. The Docker-gated eval harness in `temps-agents/tests/` grows a KVM-gated twin, and CI needs at least one KVM runner.
- File I/O over vsock RPC is a new protocol surface to fuzz and version (frames are capped at 4 MiB; large files chunk, and the exec/fs service must be strict about frame validation since its peer is the untrusted guest).
- The Docker daemon remains a hard dependency even for Firecracker sandboxes in v1 — it is the image toolchain (pull/build/inspect/export). A Firecracker-only host without Docker is not supported until a native OCI pull path ships.
- The rootfs cache trades disk for latency: every distinct image digest costs a flattened ext4 copy on top of Docker's own image store, bounded by the LRU cap but still a doubling for heavily-customized fleets.
- Setup mutates host state (bridge, nftables, uid range, sysctl) and therefore needs root for the network stage; `--uninstall` and idempotent re-application must be first-class or doctor will drown in drift reports.

### Not solved by this ADR

- **Warm pools / snapshot-based instant boot.** Deliberately deferred behind `enable_snapshots`; requires restore-safety work (entropy, clock, network re-attach).
- **Live migration or multi-node placement of VMs.** Sandboxes remain single-host; the multi-node story (ADR-020 hardening line) is orthogonal.
- **Replacing Docker for deployments.** `temps-deployer` and app runtime containers are out of scope; this ADR covers the sandbox boundary only.
- **GPU or device passthrough.** Firecracker does not support it; workloads needing GPUs stay on Docker.
- **Egress credential proxy itself.** ADR-013 ships independently; this ADR only guarantees the Firecracker network path has the same chokepoint shape.

## Implementation

Phased, each phase landable and dark-launchable:

1. **Routing seam.** `SandboxBackend` enum, `backend` on `SandboxCreateConfig`, `RoutingSandboxProvider`, prefix-based dispatch, registration refactor in `temps-agents/src/plugin.rs`. Docker-only hosts see zero behavior change; the router with one backend is a pass-through. Unit tests for dispatch and `recover_by_name` fan-out.
2. **Setup tooling.** `temps firecracker setup` (preflight, pinned binary + kernel download with sha256 verification à la `upgrade.rs`, bridge/NAT, jailer uids, smoke test), `temps doctor` checks, `--check`/`--uninstall`. Landable before the provider exists — the smoke test doubles as the first integration test.
3. **Guest artifacts.** `temps-vm-init`, CI-built pinned `vmlinux`, rootfs conversion + digest cache (`mkfs.ext4 -d` pipeline, OCI config capture, static-musl agent injection, arch check, LRU eviction), `image_status`/`rebuild_image` conversion phase. Conversion is testable against Docker alone — no KVM needed.
4. **VMM lifecycle.** `FirecrackerSandboxProvider::{create, destroy, is_alive, recover*}` via jailer; state dirs; TAP/bridge allocation; cgroup limits. Gated by `AgentSandboxSettings.sandbox_backend` and `is_available`.
5. **Agent over vsock.** vsock listeners in `temps-pty-agent`, exec/fs RPC service, host-side vsock client; implement the remaining trait methods. KVM-gated integration tests mirroring the Docker eval harness (create → exec → fs → stop/start → destroy, plus recovery after provider restart), run for both temps runtime images and a custom distroless image.
6. **API + UI.** Optional `backend` on `CreateSandboxRequest` (422 on unavailable), `GET /v1/sandboxes/backends` probe endpoint, backend column in sandbox list UI, settings-UI backend selector + server-side "Enable Firecracker" with streamed progress, docs.
7. **Preview + egress.** `temps-dns-resolver` records for VM sandboxes, preview gateway resolution, NAT/forward rules shaped for the ADR-013 proxy.

Feature flag: the entire backend is inert unless `firecracker` probes available *and* an operator selects it (host default or per-request). No environment variable escape hatches à la `TEMPS_ALLOW_LOCAL_SANDBOX` — availability is probed, selection is explicit.

## References

- ADR-008: In-Sandbox PTY Agent (frame protocol reused over vsock)
- ADR-009: Sandbox API Versioning (optional-field addition is non-breaking)
- ADR-010: Provider Boundary Traits (the seam this ADR fills; names Firecracker as intended backend)
- ADR-013: Sandbox Egress Credential Proxy (egress chokepoint parity)
- ADR-024: Control Plane DNS Resolver (preview name resolution for VM sandboxes)
- Firecracker: https://firecracker-microvm.github.io/ — design docs, jailer, vsock (hybrid Unix-socket mode), snapshot/restore
- Firecracker releases (pinned binary + jailer downloads): https://github.com/firecracker-microvm/firecracker/releases
- `crates/temps-cli/src/commands/{doctor,upgrade,setup}.rs` — existing check-report, verified-download, and provisioning patterns reused by `temps firecracker setup`
- `crates/temps-agents/src/sandbox/mod.rs` — `SandboxProvider` trait
- `crates/temps-agents/src/sandbox/docker.rs` — reference backend implementation
