# ADR-050: microsandbox (libkrun) MicroVM Sandbox Backend

**Status:** Proposed
**Date:** 2026-10-08

## Context

Sandboxes run the least-trusted code in Temps (ADR-013 threat model). Two isolation backends exist behind the `SandboxProvider` seam (ADR-010): Docker containers (`sandbox/docker.rs`) and Firecracker microVMs (ADR-029, `sandbox/firecracker.rs`), dispatched per sandbox by `RoutingSandboxProvider`.

The Firecracker backend is the right ceiling for hostile multi-tenant code, but it still has gaps that are expensive to close in-house:

1. **No jailer.** The VMM runs as the server's own user (`firecracker.rs` module docs). KVM still isolates the guest, but a VMM escape lands with the server's privileges.
2. **No egress credential proxy.** ADR-013 is unimplemented, so `network_mode: "restricted"` fails closed to *no network at all* — there is no middle ground between "open internet" and "nothing".
3. **No memory-snapshot warm boot.** Snapshots (ADR-037) rebuild a sanitized ext4; every boot is a cold boot.
4. **Docker is still the image toolchain.** Pull/export go through bollard; a Firecracker-only host can't exist.
5. **Root network setup.** TAP pool + bridge + NAT need `sudo temps firecracker setup --network-only`.
6. **Linux-only.** Developer machines (macOS) can only exercise the Docker path, so microVM behaviour is untested where most development happens.

[microsandbox](https://github.com/superradcompany/microsandbox) (Apache-2.0) is a libkrun-based microVM runtime with a Rust SDK (`microsandbox` crate). Relevant properties, verified against the v0.7.7 crate source:

- **Native OCI pull** (`microsandbox-image`) — no Docker daemon in the path.
- **Userspace networking** (smoltcp-based) with a **host-side policy engine**: deny-by-default egress, destination groups (`public`, `private`, `host`, `metadata`, …), domain rules, DNS filtering. No TAP devices, no root.
- **Host-side secrets** (`secret_env(name, value, allowed_host)`): the guest only ever sees a placeholder; the real value is substituted on the host for allowed destinations. This is the ADR-013 model.
- **Snapshot / fork / restore**, including copy-on-write memory restore.
- **Platforms:** Linux with KVM, macOS on Apple Silicon (Hypervisor.framework), Windows (preview).

What libkrun does **not** give us is Firecracker's VMM hardening story. libkrun's documented security model is that the guest and the VMM share one security context: anything the VMM process can reach on the host is potentially reachable by a guest that escapes into the VMM. Firecracker's minimal device model plus jailer is designed for exactly the case libkrun explicitly does not claim.

## Decision

Add `MicrosandboxSandboxProvider` (`crates/temps-agents/src/sandbox/microsandbox.rs`) as a **third, experimental** backend, selectable per sandbox as `"microsandbox"`. It **does not replace Firecracker**, and Firecracker's behaviour is unchanged.

### 1. Positioning: additional, experimental, not for hostile multi-tenancy

| Workload | Recommended backend |
|---|---|
| Hostile / multi-tenant code on Linux servers | **Firecracker** (hardware boundary + minimal VMM; jailer planned) |
| Developer machines (macOS), CI without root | **microsandbox** |
| Trusted / first-party agent workloads, fork-heavy experimentation | **microsandbox** |
| Everything else, default | Docker |

The console labels the option "Experimental" and says, at the point of choice, to use Firecracker for hostile multi-tenant code.

### 2. Integration mode: link the SDK crate (decided by spike)

Two options were evaluated: (a) link the `microsandbox` crate, (b) drive the `msb` CLI as a child process. We chose **(a)**. The SDK itself still runs each VM in a separate `msb` runtime process, the VMM, so (a) gives the same process boundary as (b) without parsing CLI output or depending on CLI flag stability.

The spike (adding the crate to `temps-agents` and running `cargo check`):

- **Feature set:** `default-features = false, features = ["local", "net"]`, pinned `=0.7.7`. Defaults are off because `download-binaries` downloads the runtime from the network *inside `build.rs`*, and `cloud`/`keyring` are unused.
- **ORM duplication — accepted, no conflict.** The crate depends on `sea-orm 2.0.4` / `sqlx 0.9.0` (SQLite) for its own state database; we are on `sea-orm 1.1.20` / `sqlx 0.8.6`. Both resolve side by side. `sqlx 0.9.0` was *already* in our lockfile (via `temps-proxy`), and both sqlx-sqlite versions share a single `libsqlite3-sys 0.30.1`, so there is **no `links = "sqlite3"` conflict**.
- **Size:** `Cargo.lock` grows by 100 packages (1259 → 1359). `temps-agents`' normal (non-dev) dependency graph grows by 76 unique packages (916 → 992, +8%): the `microsandbox-*` crates, the second sea-orm stack, `oci-client`/`oci-spec`, and libkrun device crates (`msb_krun_*`, `vmm-sys-util`, `kvm-*`/`hvf`) pulled in through `microsandbox-filesystem`.
- **Compile:** no new native build tools. `cmake` and `protoc` are pre-existing workspace requirements (pingora's `zlib-ng`, `temps-otel`), not added by this crate.
- **Edition:** the crate is Rust 2024. Our toolchain (1.99) builds it; the workspace stays on 2021.

Option (b) would avoid the second ORM, but trades it for a stringly-typed process interface, a separately-installed CLI whose flags must stay compatible, and output parsing for exec/fs streams. The duplicated ORM is a compile-time and binary-size cost, not a runtime or correctness risk, so (a) wins.

### 3. State and runtime ownership

All SDK state lives under a Temps-owned home, never the user's `~/.microsandbox`:

```
<data_dir>/microsandbox/      0700
  config.json                 Temps-owned SDK config (absent = SDK defaults)
  bin/msb, lib/libkrunfw.*    pinned runtime pair (v0.7.7)
  db/ sandboxes/ cache/ …     SDK-managed
```

- Every SDK call is bound to an explicitly built `LocalBackend` (`home`, `config_path`, `deployment_profile`). The SDK's *ambient* default backend also honours `MSB_BACKEND` / `MSB_API_KEY` / `MSB_PROFILE`, any of which could otherwise route sandboxes to a remote service; we never use the ambient default.
- The runtime pair is installed only by an explicit operator action, `temps microsandbox setup`. It calls the SDK's `setup::ensure_runtime`, which downloads the official release archive for the pinned version and verifies the installed pair. The command then boots a smoke-test VM. Server startup never downloads anything.
- No new Temps environment variables. Backend selection is the existing `agent_sandbox.sandbox_backend` setting plus the per-sandbox `backend` field. The SDK's own `MSB_PATH`/`MSB_LIBKRUNFW_PATH` overrides still apply, since they are runtime-resolution inputs the SDK reads, not Temps configuration.

### 4. Availability gating and selection

- `SandboxBackend::Microsandbox` (`"microsandbox"`), handle prefix `temps-msbsandbox-`, handles stamped `backend: Microsandbox` so the router dispatches without name parsing (ADR-029 §2).
- **Registered only when ready:** the platform is Linux (any arch) or macOS/aarch64, the hypervisor is usable (`/dev/kvm` read/write, or `sysctl kern.hv_support == 1`), and the runtime pair resolves offline under the Temps home. Probing never creates files or touches the network. The plugin now builds the routing provider when *either* Firecracker or microsandbox is available; with only Docker it still registers the bare Docker provider (unchanged).
- **Never a silent fallback.** An explicit `backend: "microsandbox"` on a host without it fails with 400 and the precise reason (missing hypervisor, unsupported platform, or runtime not installed under `<path>`, plus the fix). `RoutingSandboxProvider` now implements `supports_backend` (answering for registered backends only) instead of inheriting the permissive trait default, so the check happens before any work starts. `provider.create` re-probes and fails with `SandboxCreationFailed` if the host changed underneath it.
- **Host default:** the settings validator accepts `"microsandbox"`. If it is configured but unavailable at startup, agent runs use Docker *and a warning is logged with the setup path*. That matches how a configured-but-unavailable Firecracker default already behaves.
- **Discoverability:** `GET /settings/sandbox-status` and the project-scoped status endpoint return `microsandbox: { configured, reason, setup_path, setup_command, runtime_version }`. The console always renders the option. When unconfigured it shows the reason and a copyable `temps microsandbox setup`; that command is only offered when installing would actually fix the problem (not for a missing hypervisor).

### 5. v1 feature mapping

| `SandboxProvider` | microsandbox v1 |
|---|---|
| `create` | `Sandbox::builder(name).image(..).cpus(..).memory(..).security(Restricted).replace().create_detached()`; rootfs patch creates `/workspace`, then a non-empty `host_work_dir` (e.g. a workflow's cloned repository, which Docker bind-mounts) is copied into it before the handle is returned — symlinks skipped, modes kept, a failed copy destroys the VM; `disk_size_mb` → `root_disk`; `pids_limit` → `RLIMIT_NPROC`; labels `sh.temps.*` |
| `exec` / `exec_as_root` / `exec_as_user` / `exec_streamed` | `exec_stream_with` (args, cwd, env, user); stdout/stderr split; line-buffered callbacks with partial lines capped at 64 KiB; captured output bounded at 16 MiB per stream |
| `read_file` / `write_file` / `read_file_bounded` / `write_directory` | agent fs channel (`fs().read/write/mkdir/set_stat/stat`). The bounded read stats first, so oversized files are never buffered. Directory upload skips symlinks |
| `kill_processes` | `pkill -<sig> -f` as root (best-effort, per contract) |
| `stop` / `start` / `destroy` / `is_alive` | SDK handle `stop` / `connect_or_start_detached` / `destroy` / status |
| `recover` / `recover_by_name` | SDK registry lookup by name; accepts bare labels |
| `image_status` / `rebuild_image` | readiness + default image (`alpine:3.20`); no build step (images are pulled per sandbox) |

VMs are created **detached**: they outlive the server process, like Firecracker VMs, and are recovered by name after a restart.

**`network_mode`** maps onto the host-side policy engine:

| mode | policy |
|---|---|
| `none` | no network interface at all (`disable_network`) |
| `full` (default) / `open` | deny-by-default; DNS + **public internet only**. Private/LAN, host, loopback, link-local and cloud-metadata ranges are denied |
| `restricted` ("Temps network only") | deny-by-default; DNS + **the sandbox host only**. No public internet |

`restricted` therefore no longer fails closed to "no network" on this backend: the host allowlist is enforced outside the guest. Unknown modes are a validation error.

### 6. Host-side confinement of the VMM

Applied in v1:

- `DeploymentProfile::MultiTenant` is forced on the backend (operator-level, overrides any per-sandbox request): no host vsock routes, bounded per-sandbox connection tables.
- `SecurityProfile::Restricted` in the guest: `no_new_privs`, `CAP_SYS_ADMIN` dropped, `nosuid,nodev` user mounts.
- **No host filesystem is shared with the guest.** No bind mounts and no virtiofs shares of host paths; files are copied in over the agent channel.
- Network policy enforced in the VMM process, deny-by-default, with `host`/`private`/`metadata` excluded from `full`.
- The SDK home is `0700`. The SDK database records each sandbox's environment, so injected credentials sit at the same at-rest exposure as Firecracker's `env.json` (0600).

Deferred (required before this backend could be recommended for untrusted multi-tenant code):

- Running the `msb` VMM under a dedicated unprivileged uid with no access to the Temps data dir (Linux), or a `sandbox-exec` profile (macOS).
- seccomp/landlock confinement of the VMM process on Linux.
- Moving injected credentials to the SDK's host-side secret substitution (ADR-013 parity), so plaintext secrets never enter the guest or the SDK database.

### 7. Out of scope for v1 (phase 2)

- **Snapshots / fork (ADR-037).** The SDK has snapshot, fork and CoW-memory restore, but ADR-037's surface is a content-addressed, credential-scrubbed artifact file with a digest and a cross-backend 422. Mapping SDK snapshots onto that (export archive → digest → scrub → restore) is not a straight translation, so `take_snapshot` / `create_from_snapshot` keep the trait default ("not supported").
- **Worker nodes (ADR-048).** `"microsandbox"` on a worker-placed sandbox is rejected with a validation error, the same as Firecracker today.
- **Preview URLs / port publishing.** No ports are published. The preview gateway resolves Docker container names only.
- **Interactive terminals (`attach_pty`)** and the retained agent runtime transport: trait default (unsupported). The SDK has TTY exec; wiring it to the ADR-008 frame protocol is follow-up work.
- **Operator-configurable egress allowlists** (domains on top of `restricted`) and host-side secret substitution: see §6.
- **Disk resize:** trait default (unsupported).
- **Windows hosts.** The SDK supports them in preview; Temps' sandbox code is Unix-only.

## Consequences

### Positive

- A microVM backend that runs on developer Macs and on Linux hosts without root setup or Docker, behind the same API.
- `restricted` networking that keeps a host allowlist instead of dropping to no network.
- A clear path to ADR-013 secrets and fork/snapshot, through SDK features that already exist.
- No change for existing hosts: Docker-only installs register the same provider as before, and Firecracker selection and defaults are untouched.

### Negative

- +76 crates in the agents graph, including a second ORM stack (sea-orm 2 / sqlx 0.9 + SQLite) that the SDK uses only for its own bookkeeping. Compile time and binary size grow.
- A pre-1.0 dependency (`=0.7.7`) whose crate version, runtime binary and `libkrunfw` ABI move in lockstep. Upgrades are deliberate: bump the pin, `MICROSANDBOX_VERSION`, and re-run setup.
- libkrun's shared guest/VMM security context means this backend is weaker than Firecracker against VMM-escape attacks. Mitigated by positioning and the confinement in §6, but not eliminated.
- Two microVM backends double the microVM test matrix.

## Implementation

Landed with this ADR:

1. `sandbox/microsandbox.rs`: provider, availability probe (`MicrosandboxUnavailable` typed reasons), `MicrosandboxCapability`, network/resource mapping, unit tests, and e2e tests that boot real VMs and skip at runtime when the hypervisor or runtime is absent.
2. `SandboxBackend::Microsandbox`; routing registration in `plugin.rs`; `RoutingSandboxProvider::supports_backend`.
3. `temps-sandbox`: accepts `"microsandbox"`, rejects it on worker nodes, explains unavailability.
4. `temps-config`: settings validation accepts `"microsandbox"`.
5. Status API `microsandbox` capability; console backend option with onboarding state.
6. `temps microsandbox setup [--check] [--skip-smoke]`.

### Measured (macOS 27, Apple Silicon, runtime v0.7.7, `alpine:3.20`, 1 vCPU / 256 MiB)

| | |
|---|---|
| `temps microsandbox setup` runtime install (download + verify) | 1.1 s |
| First create, including image pull | 2.19 s |
| Create with cached image (provider `create` to agent ready) | 142–194 ms |
| Create via `POST /v1/sandboxes`, end to end | 0.27 s |
| Restart (stop → start, root disk preserved) | 142 ms |
| Per-VM memory (runtime-reported RSS / host-resident guest) | 36 MiB / 42 MiB |
| Guest kernel | Linux 6.12.111 aarch64 |

E2E tests (`TEMPS_DATA_DIR=<dir with runtime> cargo test --lib -p temps-agents sandbox::microsandbox::tests::e2e -- --nocapture`) boot real VMs. They cover create, exec (split streams, exit codes, env layering, root exec), file write/read with mode, bounded reads, stop/start persistence, recovery by label, destroy, `none` (no routes, no egress), `restricted` (routed, public egress denied) and `full` (public egress allowed). The guest kernel always creates an inert `dummy0` device, so "no network" is asserted on the routing table, not the interface list.

### Known limitation

The backend registers only on the path where Docker is reachable (the same place Firecracker registers). A host without Docker still falls back to the `TEMPS_ALLOW_LOCAL_SANDBOX` path unchanged. Making microsandbox a Docker-less default (e.g. a Mac without Docker) is a follow-up, because it changes what a Docker-less startup does today.

## References

- ADR-008: In-Sandbox PTY Agent
- ADR-010: Provider Boundary Traits
- ADR-013: Sandbox Egress Credential Proxy
- ADR-029: Firecracker MicroVM Sandbox Backend
- ADR-036: Persistent Workspace Sandboxes
- ADR-037: Sandbox Snapshots
- ADR-048: Multi-Node Sandboxes
- microsandbox: https://github.com/superradcompany/microsandbox (SDK crate `microsandbox` 0.7.7)
- libkrun: https://github.com/containers/libkrun (security model section)
