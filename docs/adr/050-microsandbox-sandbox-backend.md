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

- **Feature set:** `default-features = false, features = ["local", "net"]`, pinned `=0.7.8`. Defaults are off because `download-binaries` downloads the runtime from the network *inside `build.rs`*, and `cloud`/`keyring` are unused.
- **ORM duplication — accepted, no conflict.** The crate depends on `sea-orm 2.0.4` / `sqlx 0.9.0` (SQLite) for its own state database; we are on `sea-orm 1.1.20` / `sqlx 0.8.6`. Both resolve side by side. `sqlx 0.9.0` was *already* in our lockfile (via `temps-proxy`), and both sqlx-sqlite versions share a single `libsqlite3-sys 0.30.1`, so there is **no `links = "sqlite3"` conflict**.
- **Size:** `Cargo.lock` grows by 100 packages (1259 → 1359). `temps-agents`' normal (non-dev) dependency graph grows by 76 unique packages (916 → 992, +8%): the `microsandbox-*` crates, the second sea-orm stack, `oci-client`/`oci-spec`, and libkrun device crates (`msb_krun_*`, `vmm-sys-util`, `kvm-*`/`hvf`) pulled in through `microsandbox-filesystem`.
- **Compile:** no new native build tools. `cmake` and `protoc` are pre-existing workspace requirements (pingora's `zlib-ng`, `temps-otel`), not added by this crate.
- **Edition:** the crate is Rust 2024. Our toolchain (1.99) builds it; the workspace stays on 2021.
- **Targets:** the crate does not build for musl. Its VMM crate (`msb_krun_vmm` 0.1.40, unchanged in 0.1.41) moves a `pthread_t` into a `Send + Sync` closure; musl defines `pthread_t` as a raw pointer. The dependency is therefore declared under `cfg(not(target_env = "musl"))` and the provider compiles only there. The glibc Linux and macOS release binaries carry the backend. The musl build — the Alpine container image — reports it unavailable with that reason and offers no setup command. Running microVMs inside that container would also need `/dev/kvm` passed through, so the image loses little.

Option (b) would avoid the second ORM, but trades it for a stringly-typed process interface, a separately-installed CLI whose flags must stay compatible, and output parsing for exec/fs streams. The duplicated ORM is a compile-time and binary-size cost, not a runtime or correctness risk, so (a) wins.

### 3. State and runtime ownership

All SDK state lives under a Temps-owned home, never the user's `~/.microsandbox`:

```
<data_dir>/microsandbox/      0700
  config.json                 Temps-owned SDK config (absent = SDK defaults)
  bin/msb, lib/libkrunfw.*    pinned runtime pair (v0.7.8)
  db/ sandboxes/ cache/ …     SDK-managed
```

- Every SDK call is bound to an explicitly built `LocalBackend` (`home`, `config_path`, `deployment_profile`). The SDK's *ambient* default backend also honours `MSB_BACKEND` / `MSB_API_KEY` / `MSB_PROFILE`, any of which could otherwise route sandboxes to a remote service; we never use the ambient default.
- The runtime pair is installed only by an explicit operator action, `temps microsandbox setup`. It calls the SDK's `setup::ensure_runtime`, which downloads the official release archive for the pinned version and verifies the installed pair. The command then boots a smoke-test VM. Server startup never downloads anything. Setup's fallback guest-agent release fetch has a five-minute deadline covering metadata, archive streaming and checksum bodies; a stalled response reaches the warning-and-continue path.
- **The runtime must equal the SDK version.** The SDK launches only a runtime whose embedded `.msbver` section equals its own crate version (`launch_contract.rs`), and `ensure_runtime` keeps any complete existing pair without comparing versions. A host that ran an older build's setup therefore keeps a runtime this build refuses ("no tested sandbox launch contract for runtime X"). Temps guards this three ways: the availability probe reads the installed `msb`'s version through the SDK's own `resolve_runtime_version` and reports `RuntimeVersionMismatch` (installed vs. expected, with the setup command) instead of registering a backend that cannot launch; `temps microsandbox setup` replaces a mismatched pair that lives under the Temps home (`install_runtime` with `force`, which backs up and rolls back on failure) and refuses, naming the variable, when the pair comes from `MSB_PATH` / `MSB_LIBKRUNFW_PATH`, which Temps never overwrites; and the unit test `pinned_runtime_version_matches_the_sdk_this_build_links` fails the build if `MICROSANDBOX_VERSION` and the Cargo pin drift apart on a future bump.
- No new Temps environment variables. Backend selection is the existing `agent_sandbox.sandbox_backend` setting plus the per-sandbox `backend` field. The SDK's own `MSB_PATH`/`MSB_LIBKRUNFW_PATH` overrides still apply, since they are runtime-resolution inputs the SDK reads, not Temps configuration.

#### Which data directory a setup command acts on

The server reads state from its own data directory (`TEMPS_DATA_DIR`, else `~/.temps`), and a shell inherits none of that. The legacy `deploy.sh` layout keeps the instance's keys under `~/.temps/data`, so a bare `temps microsandbox setup` or `temps firecracker setup` used to provision `~/.temps` and leave the server (which reads `~/.temps/data`) reporting the backend as unavailable. Both commands now resolve the directory the same way (`commands/data_dir_discovery.rs`):

1. `--data-dir` / `TEMPS_DATA_DIR` in the shell.
2. A running `temps serve` on this host (Linux): its `--data-dir` argument, else `TEMPS_DATA_DIR` or `$HOME/.temps` from `/proc/<pid>/environ`. If several servers use different directories, nothing is guessed.
3. The layout `temps doctor` recognises (`~/.temps/data` when it holds `auth_secret`/`encryption_key`, else `~/.temps`).

The banner prints the directory and where it came from. If an explicit directory differs from a running server's, setup warns that the server will not see what is installed there. If the resolved directory holds no Temps data (no `encryption_key`/`auth_secret`), setup says so and names `--data-dir`; it still proceeds, because a machine that has not started Temps yet legitimately has none. A runtime or Firecracker state found under another candidate directory is reported the same way. The server only reads backends at startup, so a newly provisioned backend needs a restart of `temps serve`; the unavailability messages say so and print the exact `setup --data-dir …` command for the server's directory.

### 4. Availability gating and selection

- `SandboxBackend::Microsandbox` (`"microsandbox"`), handle prefix `temps-msbsandbox-`, handles stamped `backend: Microsandbox` so the router dispatches without name parsing (ADR-029 §2).
- **Registered only when ready:** the platform is Linux (any arch) or macOS/aarch64, the hypervisor is usable (`/dev/kvm` read/write, or `sysctl kern.hv_support == 1`), and the runtime pair resolves offline under the Temps home. Probing never creates files or touches the network. The plugin now builds the routing provider when *either* Firecracker or microsandbox is available; with only Docker it still registers the bare Docker provider (unchanged).
- **Never a silent fallback.** An explicit `backend: "microsandbox"` on a host without it fails with 400 and the precise reason (missing hypervisor, unsupported platform, or runtime not installed under `<path>`, plus the fix). `RoutingSandboxProvider` now implements `supports_backend` (answering for registered backends only) instead of inheriting the permissive trait default, so the check happens before any work starts. `provider.create` re-probes and fails with `SandboxCreationFailed` if the host changed underneath it.
- **Host default:** the settings validator accepts `"microsandbox"`. If it is configured but unavailable at startup, agent runs use Docker *and a warning is logged with the setup path*. That matches how a configured-but-unavailable Firecracker default already behaves.
- **Discoverability:** `GET /settings/sandbox-status` and the project-scoped status endpoint return `microsandbox: { configured, reason, setup_path, setup_command, runtime_version }`. The console always renders the option. When unconfigured it shows the reason and a copyable `temps microsandbox setup`; that command is only offered when installing would actually fix the problem (not for a missing hypervisor).

### Docker inside a microsandbox

Create a sandbox with an explicit backend and guest privilege opt-in:

```bash
temps sandbox create --backend microsandbox --allow-docker --memory-mb 1024 --json
```

The API equivalent is `POST /api/v1/sandboxes` with
`{"backend":"microsandbox","allow_docker":true,"memory_limit_mb":1024}`.
The option defaults to false, is recorded in the sandbox's metadata and create
audit event, and is returned as `sandbox.allow_docker` on create/get/list.
Omitting the backend or selecting Docker/Firecracker fails validation before
creating any sandbox. It is a creation-time option: create a replacement to
change it. Existing microsandbox guests retain the profile in their SDK state
across stop/start and server restarts.

The flag permits installing and running Docker; it does not install packages
or automatically start a daemon. With the default Alpine guest, use the sandbox
shell (as guest root):

```sh
apk add --no-cache docker
mkdir -p /sys/fs/cgroup
mountpoint -q /sys/fs/cgroup || mount -t cgroup2 none /sys/fs/cgroup
dockerd --storage-driver=vfs --iptables=false --bridge=none
```

In a second sandbox terminal, run
`docker run --rm --network none alpine:3.20 echo hello`.
This conservative daemon configuration avoids depending on overlayfs or guest
bridge/netfilter support. `vfs` copies image layers and can consume substantial
disk space. Nested containers have no network in this example; Docker Compose
stacks using bridges or published ports require additional guest networking
validation. Image downloads still need sandbox egress. Daemon startup after
resume is the guest workload's responsibility, and microsandbox snapshots remain
unsupported. This does not enable Docker inside Firecracker: that backend needs
separate kernel/init work.

Resource enforcement remains at the VM boundary: Docker and all nested containers
share the sandbox's CPU/memory/root-disk budget. The flag adds no daemon or
background loop to Temps and does not change its proxy path. Use explicit resource
limits and account for image storage when allocating the sandbox.

### 5. v1 feature mapping

| `SandboxProvider` | microsandbox v1 |
|---|---|
| `create` | `Sandbox::builder(name).image(..).cpus(..).memory(..).security(Restricted by default; Default with allow_docker).replace().create_detached()`; rootfs patch creates `/workspace`, then a non-empty `host_work_dir` (e.g. a workflow's cloned repository, which Docker bind-mounts) is streamed into it in 256 KiB chunks before the handle is returned — symlinks skipped, modes kept, a failed copy destroys the VM; `disk_size_mb` → `root_disk`; `pids_limit` → `RLIMIT_NPROC`; labels `sh.temps.*` |
| `exec` / `exec_as_root` / `exec_as_user` / `exec_streamed` | `exec_stream_with` (args, cwd, env, user); stdout/stderr split; line-buffered callbacks that never split a record — a line over 8 MiB is withheld whole and reported in the result; captured output bounded at 16 MiB per stream |
| `read_file` / `write_file` / `read_file_bounded` / `write_directory` | agent fs channel (`fs().read/write/mkdir/set_stat/stat`). The bounded read stats first to reject an oversized file without reading it, then streams and stops at the limit, so host memory never exceeds the bound even if the file grows in between. Directory upload skips symlinks |
| `sync_workspace_to_host` (new trait method; default no-op, which is right for Docker's bind mount) | after an agent run, the executor and autofixer call it before reading changes from the host checkout. microsandbox mirrors the guest `/workspace` back with streamed reads: edits and new files are copied with their mode, deletions are applied, guest symlinks and special files are never materialized on the host, and host symlinks are never written through or removed. `.git` entries are never synced at any depth, because the host runs `git` in this checkout and a guest-written Git config could name a command (`core.fsmonitor`, hooks, filter drivers). The guest is case-sensitive but the host may not be, so a name counts as `.git` under Git's own folding rules (ASCII case, HFS+-ignorable code points, trailing dots and spaces, `git~1`). Any entry whose host path resolves to the host repository's device and inode is skipped too. Edits and commits still arrive as working-tree changes against the untouched host repository. Guest-reported names must be single path components |
| `kill_processes` | `pkill -<sig> -f` as root (best-effort, per contract) |
| `stop` / `start` / `destroy` / `is_alive` | SDK handle `stop` / `connect_or_start_detached` / `destroy` / status |
| `recover` / `recover_by_name` | SDK registry lookup by name; accepts bare labels |
| `image_status` / `rebuild_image` | readiness + default image (`alpine:3.20`); no build step (images are pulled per sandbox) |

VMs are created **detached**: they outlive the server process, like Firecracker VMs, and are recovered by name after a restart. "Outlive the process" does not cover a service manager that kills the unit's whole control group (systemd's default `KillMode=control-group` on `systemctl restart`) or a host reboot; in that case startup reconciliation moves the `running` rows whose VM is down to `stopped` (with a `vm_lost` event), `resume` boots them again, and exec/files/terminal on a down VM answer `409 Sandbox VM Not Running` instead of `404` (see ADR-029 §7).

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
- `SecurityProfile::Restricted` in the guest by default: `no_new_privs`, `CAP_SYS_ADMIN` dropped, `nosuid,nodev` user mounts. An explicit per-sandbox `allow_docker: true` selects `SecurityProfile::Default`, preserving normal guest-root privileges (including mount administration and setuid elevation). This is broader than permission to execute a Docker binary. The host-side `DeploymentProfile::MultiTenant` isolation floor remains enforced; no host Docker socket, host mount, or host capability is granted.
- **No host filesystem is shared with the guest.** No bind mounts and no virtiofs shares of host paths; files are copied in over the agent channel.
- Network policy enforced in the VMM process, deny-by-default, with `host`/`private`/`metadata` excluded from `full`.
- The SDK home is `0700`. The SDK database records each sandbox's environment, so injected credentials sit at the same at-rest exposure as Firecracker's `env.json` (0600).

Deferred (required before this backend could be recommended for untrusted multi-tenant code):

- Running the `msb` VMM under a dedicated unprivileged uid with no access to the Temps data dir (Linux), or a `sandbox-exec` profile (macOS).
- seccomp/landlock confinement of the VMM process on Linux.
- Moving injected credentials to the SDK's host-side secret substitution (ADR-013 parity), so plaintext secrets never enter the guest or the SDK database.

### 7. Out of scope for v1 (phase 2)

- **Snapshots / fork (ADR-037).** The SDK has snapshot, fork and CoW-memory restore, but ADR-037's surface is a content-addressed, credential-scrubbed artifact file with a digest and a cross-backend 422. Mapping SDK snapshots onto that (export archive → digest → scrub → restore) is not a straight translation, so `take_snapshot` / `create_from_snapshot` keep the trait default ("not supported"). The snapshot service refuses a microsandbox sandbox with 501 `Snapshot is not supported by backend 'microsandbox'` *before* it scrubs credentials or stops the sandbox, so asking for a snapshot never interrupts a running workload. Use a `firecracker` or `docker` sandbox when snapshots are needed.
- **Worker nodes (ADR-048).** `"microsandbox"` on a worker-placed sandbox is rejected with a validation error, the same as Firecracker today.
- **Preview URLs / port publishing.** No ports are published. The preview gateway resolves Docker container names only.
- **Retained agent runtime transport** (`connect_agent_runtime`): trait default (unsupported).
- **Operator-configurable egress allowlists** (domains on top of `restricted`) and host-side secret substitution: see §6.
- **Disk resize:** trait default (unsupported).
- **Windows hosts.** The SDK supports them in preview; Temps' sandbox code is Unix-only.

### 8. Interactive terminals (`attach_pty`) — shipped

Terminals run the same `temps-pty-agent` as the Docker and Firecracker backends (ADR-008), so tabs, scrollback replay and detach/reattach are identical. Nothing in the image is assumed: the default `alpine:3.20` has no `bash`, `socat` or `dtach`.

- **Guest agent.** `temps microsandbox setup` stages a static Linux `temps-pty-agent` at `<data_dir>/microsandbox/guest/` (`--pty-agent-bin`, or a file next to the `temps` executable; it must match the guest architecture — the host's on Linux, aarch64 on Apple Silicon — and is built with `cargo build --release --target <arch>-unknown-linux-musl -p temps-pty-agent`; release archives ship the matching static `temps-pty-agent` next to `temps`, `scripts/install.sh` and `temps upgrade` install it beside the binary, and setup finds it there without `--pty-agent-bin`). On the first terminal for a sandbox the provider writes it into the guest through the SDK file API at `/usr/local/lib/temps/pty-agent-<sha256 prefix>`, root-owned `0755` in a root-owned directory (content-addressed, so a new build installs beside the old one). A VM that is recreated or restarted keeps it on its root disk.
- **Staying current.** At startup the server compares the `temps-pty-agent` beside its own executable with the staged copy of every backend that has already been set up (its state directory exists) and replaces the staged copy atomically when the bytes differ (`sync_pty_agent_binary`). So `deploy.sh`, `install.sh` and `temps upgrade` — which all land the new agent beside `temps` — are enough; no `setup` re-run is needed after an upgrade. The comparison is by content, so a rollback also restores the older agent. Each writer uses a unique temporary file in the destination directory, so setup and startup refresh can safely overlap; only a complete file is renamed into place. A missing, foreign (non-Linux) or wrong-architecture source is rejected and the working staged copy is left alone; failures are logged and never block startup. Hosts that never ran a `setup` command get no new directories. The same sync keeps Firecracker's `temps-vm-agent` (ADR-029) current, and runs before backends are probed so a host set up before a release that ships the VM agent becomes available on the first start after upgrading.
- **Fetching when absent.** When `temps microsandbox setup` / `temps firecracker setup` find no agent via the flag, beside the executable, or from an earlier staging, they download the release tarball that matches the running binary's own version, verify its published SHA-256 (fail closed, same pipeline as `temps upgrade`), and extract the missing agents from it. The step is announced, only fetches what is missing, and a failure (offline host, a release that predates the agents) falls back to the previous warning naming `--pty-agent-bin` / `--vm-agent-bin`. The version is pinned to the binary because the VM agent must agree with the host on the wire protocol.
- **Outdated-agent warning.** Each guest agent (`temps-vm-agent`, `temps-pty-agent`) embeds a `TEMPS_GUEST_AGENT_BUILD=<commit>` marker recorded by its build script, and the server reads it out of the file (nothing is executed) after the startup sync, comparing it with the commit its own copy of the agent crate was built from. A different commit -- or an agent from before the marker existed -- logs a `warn` naming the agent, its path and both builds, with the fix; matching builds log at `debug`, and a build without git information (`unknown`) is never reported. The copy beside the `temps` executable is the one checked when it exists, since that is what `temps upgrade` replaces and the sync mirrors it into the data dir. The check is advisory: it never blocks startup and does not change which agent is used.
- **Start.** The agent is started detached (`setsid`, `nohup` fallback) as the sandbox's **default exec user** — the same identity every `exec` runs as — with `/run/temps-pty` created `0700` for that uid. Every step is idempotent: a probe (`temps-pty-agent --probe`, which connects rather than testing for the socket file, since a crashed agent leaves a stale one) gates both installation and start, and the agent itself refuses to bind over a live socket, so concurrent first attaches cannot orphan each other's tabs.
- **Attach.** `attach_pty` is an SDK streaming exec of `temps-pty-agent --relay` with stdin piped (`exec_stream_with(.., stdin_pipe())`). Exec stdout is the `output` stream; `input` is a bounded 64 KiB pipe pumped into the SDK's stdin sink and closed on drop, which ends the relay but not the agent. Relay stderr is surfaced as the attach error, never mixed into the frame stream.
- **Shell.** The default tab command uses `bash -l` when the guest has bash and `sh -l` otherwise.
- **Requirements and limits.** The guest needs `/bin/sh` (the agent spawns tabs with `sh -c`); shell-less images (distroless) cannot open terminals. The status API reports `microsandbox.terminal` (`supported`, `reason`, `setup_command`) independently of `configured`, so the console shows "terminals unavailable" with the fix instead of a bare 409. The staged binary must be a Linux ELF; a macOS build is rejected at staging and at attach.
- **Verification status.** The relay, the bootstrap scripts, the argument/ELF validation and the stream plumbing are unit-tested without a hypervisor; `sandbox::microsandbox::tests::e2e_terminal_attach_runs_a_command_and_reattaches` boots a real VM and skips at runtime when the backend or a staged agent is absent.

## Consequences

### Positive

- A microVM backend that runs on developer Macs and on Linux hosts without root setup or Docker, behind the same API.
- `restricted` networking that keeps a host allowlist instead of dropping to no network.
- A clear path to ADR-013 secrets and fork/snapshot, through SDK features that already exist.
- No change for existing hosts: Docker-only installs register the same provider as before, and Firecracker selection and defaults are untouched.

### Negative

- +76 crates in the agents graph, including a second ORM stack (sea-orm 2 / sqlx 0.9 + SQLite) that the SDK uses only for its own bookkeeping. Compile time and binary size grow.
- A pre-1.0 dependency (`=0.7.8`) whose crate version, runtime binary and `libkrunfw` ABI move in lockstep. Upgrades are deliberate: bump the pin, `MICROSANDBOX_VERSION`, and re-run setup.
- libkrun's shared guest/VMM security context means this backend is weaker than Firecracker against VMM-escape attacks. Mitigated by positioning and the confinement in §6, but not eliminated.
- Two microVM backends double the microVM test matrix.

## Implementation

Landed with this ADR:

1. `sandbox/microsandbox/`: `mod.rs` holds the target-independent parts — availability probe (`MicrosandboxUnavailable` typed reasons), `MicrosandboxCapability`, configuration; `sdk.rs` holds the SDK-backed provider, network/resource mapping, unit tests, and e2e tests that boot real VMs and skip at runtime when the hypervisor or runtime is absent.
2. `SandboxBackend::Microsandbox`; routing registration in `plugin.rs`; `RoutingSandboxProvider::supports_backend`.
3. `temps-sandbox`: accepts `"microsandbox"`, rejects it on worker nodes, explains unavailability.
4. `temps-config`: settings validation accepts `"microsandbox"`.
5. Status API `microsandbox` capability; console backend option with onboarding state.
6. `temps microsandbox setup [--check] [--skip-smoke]`.
7. Interactive terminals (§8): `attach_pty`, guest agent staging (`--pty-agent-bin`), `microsandbox.terminal` capability.

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

### Known limitations

- **musl builds** (the container image) don't include the backend; see §2.
- The backend registers only on the path where Docker is reachable (the same place Firecracker registers). A host without Docker still falls back to the `TEMPS_ALLOW_LOCAL_SANDBOX` path unchanged. Making microsandbox a Docker-less default (e.g. a Mac without Docker) is a follow-up, because it changes what a Docker-less startup does today.

## References

- ADR-008: In-Sandbox PTY Agent
- ADR-010: Provider Boundary Traits
- ADR-013: Sandbox Egress Credential Proxy
- ADR-029: Firecracker MicroVM Sandbox Backend
- ADR-036: Persistent Workspace Sandboxes
- ADR-037: Sandbox Snapshots
- ADR-048: Multi-Node Sandboxes
- microsandbox: https://github.com/superradcompany/microsandbox (SDK crate `microsandbox` 0.7.8)
- libkrun: https://github.com/containers/libkrun (security model section)
