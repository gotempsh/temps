<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Docker Compose security policy: analysis and proposal

**Status:** proposal for review. No code has changed.
**Date:** 2026-11-15

## Why this matters

Anonymous telemetry for the 30 days to 2026-11-15:

- `compose_policy_rejected` is the third most common deploy failure: 78 failed deploys across 23 installations. No other failure that is clearly ours (not a bug in the user's code) reaches as many installations.
- Related codes: `compose_variable_missing` 40 failures (4 installations) and `compose_up_failed` 23 (6).
- Compose deploys succeed 77.7% of the time (1,399 attempts, 39 installations). Dockerfile builds succeed about 85% of the time.

A rejection usually comes on the first deploy, before the user has seen anything work, so it is a likely point where people give up.

## Top recommendations

1. **Rewrite `ports:` to loopback automatically and say so.** This one rule rejects 86 of the 93 sample files (92%) and is the first error for 54 of the 90 files that fail. Rewriting `"8080:80"` to `"127.0.0.1::80"` only ever narrows exposure, and it matches the fix the error message already prints. Together with a few other strictly-narrowing rewrites (below), the sample pass rate goes from **3 / 93 to 62 / 93**.
2. **Report every violation at once, before the first deploy, with the line and the fix.** Today a deploy stops at the first violation in the first service. A failing file has 2 distinct rules on average, and an affected file has 3.4 service-level `ports`/`container_name` violations, so a user fixing them from the messages needs several redeploys. The failure banner's "How to fix" text is generic and does not mention ports, the most common cause. The project-creation screen never runs the check.
3. **Send the rule ID in `deploy_failed` telemetry** as a fixed, allowlisted value (`compose_policy_check: published_ports`). Today telemetry carries only `compose_policy_rejected`, so we cannot see which rule fires. The numbers below are estimates from a sample.

What not to change: per-project exceptions already exist. They are a column (`compose_security_policies.policy`), changed only by an admin, with an explicit risk acknowledgement, and recorded in an audit log and a history table (#1057). Nothing here proposes an environment variable or turning off by default a rule that protects the host or other tenants.

---

## How the policy works today

**Where it runs.** The deploy job loads the project's policy row (`crates/temps-deployments/src/services/workflow_execution_service/job_registry/deploy_compose.rs:116`). It then calls `ComposeExecutor::check_configuration` (`crates/temps-deployer/src/compose/resolve.rs:520`), which runs these steps in order:

1. Resolve `${VAR}` from the project environment and the repository `.env`.
2. Remove services the user excluded.
3. Run the policy, `validate_compose_security_policy` (`crates/temps-deployer/src/compose/security_policy.rs:204`).
4. Check filesystem confinement (`crates/temps-deployer/src/compose/fs_confinement.rs`).
5. When the file uses `include`, `extends`, an inline override or merge tags, run `docker compose config` and validate the merged model again (`crates/temps-deployer/src/compose/resolve.rs:923`).

The check runs before the old stack is torn down, so a rejection causes no downtime. The settings preview and the "Run compose check" action call the same function, so they reject the same files.

**What it does on a violation.** It returns the first one and stops. The error is `ComposeError::SecurityPolicyViolation { service, field, reason }` (`crates/temps-deployer/src/compose/mod.rs:157`). There is no file name, no line, and no check ID.

**Hardening that is injected, not rejected.** Temps writes `docker-compose.temps-security.yml`, which applies `cap_drop: ALL` plus CHOWN, DAC_OVERRIDE, FOWNER, SETUID and SETGID, `no-new-privileges`, `pids_limit: 512`, a 4 GiB memory limit, bounded logs and `init` (`crates/temps-deployer/src/compose/overrides.rs:520-632`). Those catalog entries (`NoNewPrivileges`, `DropCapabilities`, `Init`, `Pids`, `Memory`, `Logging`) never reject a file. They matter for `compose_up_failed`, not for this code.

**Exceptions.** There are 80 checks in `ComposeSecurityCheck` (`crates/temps-entities/src/compose_security.rs`), with labels in `crates/temps-presets/src/compose_security.rs`. An admin can turn any of them off for one project in *Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings*. The update path (`crates/temps-projects/src/services/compose_security.rs:93` and `crates/temps-projects/src/handlers/compose_security.rs`) requires all of the following:

- the `Admin` role plus `ProjectsWrite`;
- `acknowledge_risks` for every newly disabled check;
- an optimistic-concurrency token (`expected_policy`).

Each update writes a history row and an audit event.

**Classification.** `crates/temps-deployments/src/services/failure_classifier.rs:1068` matches `"security policy rejected"`, `"invalid compose override"`, `"check rejects"` and `"rejected for field"` and returns `compose_policy_rejected`. The banner shows this static text (`crates/temps-deployments/src/services/failure_classifier.rs:626`):

> "A Compose setting is not allowed by Temps' sandbox (for example privileged, host bind mounts, host networking or extra capabilities). Remove it from the Compose file, or ask an instance administrator to relax the specific check…"

**Why the rules exist.** The rules come from a series of hardening and review fixes:

| Commit | What it added |
|---|---|
| `9d915b5e0` (2026-06-24) | Base deny-list: `privileged`, `cap_add`, `devices`, `security_opt`, absolute binds |
| `4822ce71f`, `86a559fc7` | Interpolation bypasses and path confinement |
| `c2b3948cd`, `693dbd27c`, `7f95e57d4` (2026-07) | `include`, `volumes_from`, `sysctls`, `group_add`, `cgroup_parent`, `runtime` |
| `458791194` (2026-07-13) | Symlink escapes, `tmpfs` |
| `99bfb1a0a` (#855, 2026-09-04) | Loopback ports, `container_name`, `pull_policy`, replicas, image references |
| `3bd971256` (#1057) | Per-project exceptions |
| `95aa038e9` (#1093) | One check per rule, so each can be relaxed on its own |
| `26ebdad8e` (#1540) | Variables resolved before checks, `docker compose config` for include/extends, merged-model validation |

ADR 007 (Compose as a preset) explains the routing model: the proxy reaches services through Temps, not through host ports. ADR 079 explains why the Docker socket is never given to a workload by default.

## Method

- **Code.** I read every rule in `security_policy.rs`, `fs_confinement.rs` and the inline-override allow-list, and the history of each.
- **Sample.** 93 public compose files fetched on 2026-11-15 from default branches:
  - 56 from well-known self-hosted applications: photo libraries, analytics, error tracking, chat, automation, LLM tools, CRM, wikis, finance, identity, BI, observability, a reverse proxy, a container updater, and more;
  - 37 from an official collection of example stacks for common frameworks: Flask, Django, React with Express, Spring with Postgres, WordPress with MySQL, and so on.
- **Harness.** A scratch binary outside the repository called the real `ComposeExecutor::check_report`, the same code path as the settings preview and the deploy. Each file ran:
  1. with the default policy;
  2. with every check disabled;
  3. with each check enabled on its own. A check "fires" for a file when that run differs from run 2.
- **Fixtures.** The files are kept outside the repository and are not committed. Step 7 of the plan adds them back as fixtures named by category only.
- **Limits.**
  - The environment was empty, so `${VAR}` with no default resolves to an empty string. Some bind-mount hits may be artifacts of that.
  - Filesystem confinement (symlinks, paths that must exist) is not exercised, because there is no checkout.
  - Each file counts once, whatever its popularity.
  - The sample overstates real traffic. Only 78 of 1,399 real deploys were rejected, so most deployed files already comply (templates, or users who adapted). The useful number is the *distribution among rejections*, not the absolute rate.

### Results

| Policy | Files that pass (of 93) |
|---|---|
| Today's default | **3** (self-hosted 3 / 56, official examples 0 / 37) |
| Default, `ports` fixed | 46 (43 more files fail on nothing else) |
| Default + the safe rewrites in this proposal | **62** |
| … + the narrowed false positives in this proposal | ~67–70 (estimate; the remaining ~23 need the Docker socket, privileged mode, host paths, the host network or a GPU) |
| Every check disabled | 92 (the last one references a file that was not fetched) |

First error under the default policy (this is what the user reads), for the 90 failing files:

| First error | Files | Estimated share of real `compose_policy_rejected` |
|---|---|---|
| `ports` | 54 | ~60% |
| `container_name` | 21 | ~23% |
| everything else (`extends`, `cap_add`, `security_opt`, `ulimits`, `shm_size`, `tmpfs`, network/volume names, external volumes, IPAM, volume driver options) | 15 | ~17% |

Of the 90 failing files, 45 trip two or more distinct rules (mean 1.96, max 9). Since one deploy reports one violation, fixing a file from the messages takes several deploys.

---

## Summary table: every rule

**Hit rate** is the share of the 93 sample files where the check fires.

**Message quality:**

- **B**: names the service and field and gives the exact fix.
- **C**: names the service and field and says why, but gives no fix.
- **D**: vague, shared between rules, or misleading.

No message today names the file or the line, and every message ends with the same boilerplate ("only instance administrators can change security checks…"), so the best grade any of them gets is B.

**Recommendation:**

- **Keep**: keep as is.
- **Explain**: keep, but explain better.
- **Rewrite**: rewrite automatically and safely, and tell the user.
- **Opt-in**: allow with the existing admin, audit-logged per-project exception (already available for every check).
- **Narrow**: drop the rule for a named low-risk subset and keep it for the rest.

### Composition

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `Extends`: `services.*.extends` | Importing settings from a file that was not reviewed | 2.2% | B | **Narrow**: allow local files, since the merged model is validated after #1540; keep remote ones behind Opt-in |
| `Include`: top-level `include` | Same, for whole files | 0% | B | **Narrow**: same as `Extends` |
| `Interpolation`: `${…}` left unresolved in `privileged`, `network_mode`, `pid`, `ipc`, `uts`, `cgroup`, `userns_mode`, `cap_add`, `devices`, `volumes`, `security_opt`, `group_add`, `device_cgroup_rules`, `volumes_from`, `runtime`, `oom_kill_disable`, `shm_size`, `tmpfs`, `ulimits`, bind sources | A value that passes validation and then resolves to `host` / `true` / `/` | 0% (env-dependent) | C | **Explain**: name the variable and where to set it |
| `InlineServices`: override adds a service | Services that were never reviewed | n/a | B | Keep |
| `InlineSections`: override sets top-level keys | Networks, volumes or secrets added outside the reviewed file | n/a | B | Keep |
| `InlineFields`: override sets `network_mode`, `pid`, `ipc`, `uts`, `cgroup`, `userns_mode`, `cap_drop`, `volumes`, `shm_size`, `labels`, `build`, `env_file` | Escalation through the console override | n/a | B | **Explain**; also allow *removal* (`!reset`) of restricted keys, see §Workarounds |
| *Structural, no check*: more than 256 services; service names outside `[A-Za-z0-9][A-Za-z0-9._-]*`; non-string service keys | Corrupting the generated security override | 0% | C | Keep |
| *Structural, no check*: literal types (`privileged` is a bool, `network_mode` is a string, `build` is a path or mapping, `image` is a string, long-form volumes have a type) | Values that read differently to our parser and to Compose | 0% | C | Keep; **Explain** |
| *Structural, no check*: `image` still contains a variable after resolution | An image that cannot be checked | 0% here (26% of files use a variable in `image`; they pass when it is set) | B | Keep |

### Runtime

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `Privileged`: `privileged: true` | Full host escape | 2.2% | C | Keep / Opt-in; **Explain** |
| `DockerSocket`: `use_api_socket`, any bind exposing a Docker socket (including parent directories, symlinks, `DOCKER_HOST`, Colima) | Root on the host and every tenant's data | 8.6% | C | Keep / Opt-in; **Explain**: when the service is a reverse proxy or updater sidecar, suggest excluding it, since Temps does the routing |
| `Capabilities`: `cap_add` | Kernel attack surface, network or namespace escape | 4.3% | C | **Narrow**: allow Docker-default, low-risk capabilities (`NET_BIND_SERVICE`, `KILL`, `FSETID`, `SETPCAP`, `SETFCAP`, `SYS_CHROOT`, `AUDIT_WRITE`, plus the five baseline ones, which are already granted); keep `NET_ADMIN`, `SYS_ADMIN`, `SYS_PTRACE`, `SYS_MODULE`, `NET_RAW` and the rest behind Opt-in |
| `SecurityOptions`: `security_opt` | Turning off seccomp, AppArmor or no-new-privileges | 3.2% (**every value in the sample was `no-new-privileges[:true]`**) | D (says it "can disable no-new-privileges" to a user who is enabling it) | **Rewrite**: drop `no-new-privileges[:true]`, since Temps injects it already; keep rejecting every other value |
| `Devices`: `devices` | Host device access | 0% | C | Keep / Opt-in |
| `DeviceRules`: `device_cgroup_rules` | Host device access | 0% | C | Keep / Opt-in |
| `Gpu`: `gpus`, `deploy.resources.reservations.devices` | Host accelerators | 1.1% | C | Keep / Opt-in; **Explain** (GPU needs an admin exception) |
| `Sysctls`: `sysctls` | Kernel parameters | 1.1% | C | **Narrow**: allow namespaced `net.*` when the service does not share the host network (`HostNetwork` is enforced); keep `kernel.*` behind Opt-in |
| `Groups`: `group_add` | Joining the docker group or the host's device groups | 0% | C | Keep |
| `CgroupParent`: `cgroup_parent` | Escaping the resource limits | 0% | C | Keep |
| `Runtime`: `runtime` | Swapping in a different OCI runtime | 0% | C | Keep |
| `LifecycleHooks`: `post_start`, `pre_stop` | Privileged hooks | 0% | C | **Narrow**: allow hooks without `privileged: true`; they behave like `docker exec` inside the same sandbox |
| `Provider`: `provider` | Host plugins | 0% | C | Keep |
| `ContainerName`: `container_name` | Collisions in the daemon-wide namespace, and impersonating another project's container | **31.2%** | C | **Rewrite**: drop it and add the same name as a network alias on the service's project networks, so in-stack hostnames keep working |
| `DropCapabilities`, `NoNewPrivileges`, `Init` | *Injected hardening, never a rejection* | n/a | n/a | Keep; out of scope for this code (`compose_up_failed`) |

### Networking

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `HostNetwork`: `network_mode: host` | Bypassing the proxy and firewall; reaching host services | 1.1% | C | Keep / Opt-in; **Explain** |
| `HostPid`, `HostIpc`, `HostUts`, `HostCgroup`, `HostUser`: `pid` / `ipc` / `uts` / `cgroup` / `userns_mode: host` | Host namespaces | 0% | C | Keep / Opt-in |
| `ContainerNamespace`: `*: container:<x>` | Joining another tenant's container | 0% | C | Keep |
| `NetworkMode`: any other `network_mode` (`bridge`, unknown values) | The default bridge shared with other tenants | 0% | B− | Keep |
| `ExternalNetworks`: `networks.*.external` | Joining another stack's network | 0% (2 files had `external: false`, which passes) | C | Keep; **Explain** |
| `NetworkNames`: `networks.*.name` | Daemon-wide network names | 3.2% | C | **Rewrite**: drop `name:` so Compose prefixes it with the project name |
| `NetworkDrivers`: a driver other than `bridge` | `macvlan`/`ipvlan` bypassing host filtering | 0% | C | Keep |
| `NetworkOptions`: `networks.*.driver_opts` | Changes to host routing | 0% | C | Keep |
| `NetworkIpam`: `networks.*.ipam` | Subnet clashes, routing | 1.1% | C | Keep; **Explain** (usually there for static IPs) |
| `ExternalLinks`: `external_links` | Linking to containers outside the project | 0% | C | Keep |
| `PublishedPorts`: `ports` not bound to `127.0.0.1` | Bypassing the proxy (TLS, auth, IP rules) and taking public host ports | **92.5%** | B (gives the exact replacement, but the banner text contradicts it) | **Rewrite**: `"[ip:]H:C[/p]"` → `"127.0.0.1::C[/p]"` (long form: set `host_ip`, drop `published`); preselect the public service |

### Storage

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `BindMounts`: bind source that is absolute, starts with `~`, or escapes through `..` | Reading and writing host or other-tenant files | **11.8%** | C (no fix given) | Split into four: **(a) Narrow**: allow `..` that stays inside the repository checkout (`fs_confinement` already does a canonical check; today the lexical check rejects any `..`); **(b) Narrow**: allow `/etc/localtime` and `/etc/timezone` read-only; **(c) Explain**: for home or absolute data directories, suggest a named volume and offer a one-click rewrite in the preview; **(d)** Keep / Opt-in for `/`, `/proc`, `/sys`, `/run`, `/var/run`, `/dev` |
| `VolumeDrivers`: a volume driver other than `local` | Plugins that mount host or remote filesystems | 0% | C | Keep |
| `VolumeNetworkFilesystems`: `driver_opts.type` NFS, CIFS and similar | Mounting a network share from the host daemon | 0% | D (same text for three checks) | Keep; **Explain** (name the check) |
| `VolumeHostPaths`: `driver_opts` with `o: bind` / `type: none` | Bind-mounting a host path through a named volume | 0% | D | Keep; **Explain** |
| `VolumeOptions`: other `driver_opts`, e.g. `type: tmpfs` | Unbounded options | 1.1% | D | Keep; **Explain** |
| `ExternalVolumes`: `volumes.*.external` | Another project's data | 1.1% | C | **Rewrite**: drop `external`, so Temps creates a volume for this project (a fresh install has no prior volume anyway); tell the user |
| `VolumeNames`: `volumes.*.name` | Daemon-wide volume names, reading another project's volume | 1.1% | C | **Rewrite**: drop `name:` |
| `VolumesFrom`: `volumes_from` | Another container's mounts | 0% | C | Keep |
| `ConfigPaths`, `SecretPaths`: `configs`/`secrets.*.file` outside the project | Host files | 0% (9.7% use relative files, which pass) | C | Keep; **Explain** |
| `ExternalConfigs`, `ExternalSecrets`: `external` / `name` on configs and secrets | Daemon-wide objects | 0% | C | Keep |
| `EnvFiles`: `env_file` outside the project or using a variable | Host files | 1.1% (`../.env`) | C | **Narrow**: same `..`-inside-the-repository fix as `BindMounts` (a) |
| `LabelFiles`: `label_file` | Reading host files | 0% | C | Keep |
| `StorageOptions`: `storage_opt` | Host storage | 0% | C | Keep |

### Resources

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `OomKiller`: `oom_kill_disable` | Host-wide memory denial of service | 0% | C | Keep |
| `ServiceShm`: `shm_size` over 512 MiB per service | Host memory exhaustion | 3.2% | B | Keep |
| `AggregateShm`: total `shm_size` over 1 GiB | Same | 0% | B | Keep |
| `Tmpfs`: `tmpfs`, `type: tmpfs` | Unbounded RAM-backed mounts | 3.2% | C | **Narrow**: allow when the `Memory` limit is enforced (tmpfs pages count against the container's memory cgroup); add `size=64m` when none is given, and tell the user |
| `Ulimits`: `ulimits` | Raising kernel resource limits | 5.4% (`nofile` 10 times, `memlock` 4 times) | C | **Narrow**: allow `nofile` up to 1,048,576 and `nproc` up to the PID limit; keep `memlock` and the rest behind Opt-in |
| `Blkio`: `blkio_config` | Host I/O scheduling | 0% | C | Keep |
| `Swap`: `memswap_limit` | Bypassing memory pressure | 0% | C | Keep |
| `Replicas`: `scale` ≠ 1, `deploy.replicas` ≠ 1, `deploy.mode` ≠ `replicated` | Multiplying resources; recreate-in-place semantics | 3.2% (`replicas: 0` "to disable", `5`, `mode: global`) | C | **Rewrite** `replicas: 0` / `scale: 0` into "excluded service" and tell the user; keep rejecting more than 1; **Explain** |
| `Pids`, `Memory`, `Logging` | *Injected limits, never a rejection* | n/a | n/a | Keep |

### Build

| Rule (keys) | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| `RemoteBuild`: Git or URL build contexts | Fetching from the host network | 0% | C | Keep |
| `BuildContext`: `build` / `build.context` absolute, `..` or using a variable | Sending host directories into the build | 1.1% (`context: ../.` in a monorepo) | C | **Narrow**: the same `..`-inside-the-repository fix |
| `Dockerfile`: `build.dockerfile` | Same | 0% | C | **Narrow**: the same fix |
| `BuildPrivileged`, `BuildEntitlements` | Escaping the build sandbox | 0% | C | Keep |
| `BuildNetwork`: `build.network` other than `default` / `none` | Builds reaching other deployments | 0% | C | Keep |
| `BuildSsh`: `build.ssh` | Leaking host keys | 0% | C | Keep |
| `BuildShm`, `BuildUlimits` | Unbounded build resources | 0% | C | Keep |
| `BuildAdditionalContexts` | Extra local or remote contexts | 0% | C | Keep; **Explain** |
| `BuildCacheFrom`, `BuildCacheTo`: `build.cache_from` / `cache_to` | Reading or writing host or registry cache locations | 1.1% | C | **Rewrite**: drop them (only affects speed) and tell the user |
| `BuildTags`: `build.tags` | Daemon-wide tags that collide with another project's | 0% | C | **Rewrite**: drop them |
| `ImageReferences`: `sha256:…` IDs, `temps.internal/…` | Using another deployment's local image | 0% | B | Keep |
| `BuildImage`: `image:` together with `build:` | Tag collision or squatting across deployments | 3.2% | B | **Rewrite**: drop `image:` when no other service refers to that tag; otherwise Explain |
| `PullPolicy`: `pull_policy` other than `always`, or any value on a build service | Running a cached local image that another tenant tagged | 1.1% (`never`) | C | **Rewrite**: drop it, since Temps always pulls image services and builds build services |

### Filesystem confinement (`fs_confinement.rs`, with a checkout)

| Rule | Blocks | Hit rate | Msg | Recommendation |
|---|---|---|---|---|
| Bind, build, env and config paths must exist (or be creatable) and canonicalize inside the project; no symlink escapes; Temps only writes files inside the project | Symlink and TOCTOU escapes | not measured (needs a checkout) | C | Keep. The confinement root should be the **repository checkout**, which also enables the `..` fixes above |

---

## Details

### 1. `ports` (`PublishedPorts`), the main cause

**What the rule does.** It rejects any `ports` entry that is not literally `127.0.0.1:…` (`crates/temps-deployer/src/compose/security_policy.rs:1239`). The routed proxy dials the loopback-published port (`crates/temps-deployments/src/jobs/deploy_compose.rs:39`). On a worker node the loopback binding is rebound to the node's private address (`crates/temps-deployer/src/compose/files.rs:550`). The rule exists so that Compose cannot publish `0.0.0.0:5432` and skip the proxy's TLS, authentication and IP rules.

**Why it fires so often.** Almost every compose file in the wild publishes `"H:C"`. The current message is precise ("change "8080:80" to "127.0.0.1::80"…"), but:

- The banner above it says the cause is something like "privileged, host bind mounts, host networking or extra capabilities", and that the fix is to "Remove it".
- The message ends by saying only administrators can change checks, which suggests the fix needs an admin when it doesn't.
- It reports one service per deploy. Files with several published services need one deploy per service.

**Proposal: rewrite automatically.**

- Short form `"[ip:]H:C[/proto]"` becomes `"127.0.0.1::C[/proto]"`. Long form gets `host_ip: 127.0.0.1` and loses `published`.
- If the user turned the check off, there is nothing to rewrite.
- Log one deploy-log warning per rewritten entry.
- Show the rewrite in the settings preview.
- When no public service is selected yet, preselect the service whose original host port was 80/443/8080/3000, or the only one with ports, and show that in the configurator.

A code comment says "Temps does not rewrite the file itself: which ports a stack exposes is the author's decision" (`crates/temps-deployer/src/compose/security_policy.rs:1285`). The rewrite does not take that decision away. It can only *reduce* exposure, and whether a service is public is still the author's explicit choice through the public-service setting.

**Trade-off.** Nothing is published that was not before, so there is no security cost. What the user loses is a fixed host port: `:8080` on the host no longer answers, which is what the proxy-only model intends.

### 2. `container_name` (`ContainerName`)

Container names are global across the Docker daemon. A fixed name collides between environments and projects, and it lets one stack take another's DNS name on a shared network.

Removing the key alone would break stacks whose services address each other by the container name (for example `DB_HOST=app_postgres`). So the rewrite is: remove `container_name: X` and add `X` as a network alias on each network the service joins. Aliases are scoped to the project's network, so the collision and impersonation risk goes away.

**Trade-off.** None for security. External tooling that relied on `docker exec X` loses the name.

### 3. One report, with lines and fixes, before the first deploy

This is the change that helps with every rule.

- **Collect, don't return early.** `validate_compose_security_policy` gathers every violation into a `Vec<PolicyViolation { check: Option<ComposeSecurityCheck>, service, field, value_excerpt, fix }>`. The deploy still fails if the list is not empty.
- **Line numbers.** `yaml_serde` has no node spans. A small text locator in this repository can find the line: the `services:` block, then the service key, then the field. Write it here rather than adding a dependency. When the value came from `include`, `extends` or an anchor, say so instead of guessing a line.
- **Use the console label, not the Rust name.** Messages say "the ContainerName check rejects" (`crates/temps-deployer/src/compose/security_policy.rs:2108`). They should use the catalog label, "Block custom container names".
- **Use one settings path.** Deploy messages say *Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings*. Preview messages in `crates/temps-presets/src/docker_compose.rs:402-630` say *Project Settings → Git → Advanced security settings*. Use one string, kept in one constant.
- **Per-check remediation in the banner.** The failure classifier keeps a single code, but the "How to fix" text should come from the check: file fix first, admin exception second, and only for checks where an exception makes sense.
- **Run it at project creation.** `ProjectConfigurator` already lists compose services through `getRepositoryComposeServicesLive`, but never runs the policy. Call the existing compose-check service there. Show each violation with its line and fix, mark the ones Temps will rewrite, and do this before the first deploy, so the first deploy is not where the user finds out. This follows the *Feature Discoverability* rule: show what is missing and link to the fix.

### 4. Narrowing false positives

Each of these removes rejections where the setting cannot reach the host or another tenant. Every one stays covered by the existing check for the risky subset.

- **`..` inside the repository** (`BindMounts`, `EnvFiles`, `BuildContext`, `Dockerfile`). A compose file in `docker/` often refers to `../.env` or uses `context: ..`. `is_dangerous_host_path` rejects `..` on the text alone (`crates/temps-deployer/src/compose/security_policy.rs:1848`), but `fs_confinement` already canonicalizes against the project root. Normalizing against the compose file's directory and then requiring the result to stay inside the checkout keeps the protection and allows monorepos.
- **`/etc/localtime` and `/etc/timezone` read-only.** These are non-secret files that every host has. Allow exactly these two, `:ro` only.
- **`security_opt: no-new-privileges[:true]`.** It tightens the container and is already injected. Drop it instead of rejecting.
- **`cap_add` within Docker's default set**, `net.*` sysctls without the host network, `nofile` ulimits, tmpfs with a size limit, and non-privileged lifecycle hooks: all stay inside the container's own namespaces and cgroup limits. Everything else stays behind the check.
- **Local `include`/`extends`.** The threat was "only the top file is validated". Since #1540, Temps snapshots every referenced file, validates each (`validate_source_security`) and validates the merged model with the same policy. Remote references keep needing an exception.

### 5. Telemetry: the rule ID

**Today.** `deploy_failed_telemetry_event` (`crates/temps-deployments/src/services/workflow_execution_service.rs:133`) sends `reason`, `failure_stage`, `failure_code`, `classifier_version`, `source_type`, `preset` and the template slug. **The rule ID is not sent.** It cannot be derived either: the classifier sees only the reason string, and that string carries the field name, not the check.

**Proposal.**

- Add `check: Option<ComposeSecurityCheck>` to `ComposeError::SecurityPolicyViolation` and `InvalidOverride`. `None` means a structural or filesystem rule.
- The error's `Display` gains a fixed token, `[compose check: published_ports]`.
- The classifier extracts the token and **parses it into the enum**. Anything that does not parse becomes `other`, so user text can never pass through.
- New property `compose_policy_check`: the snake_case check ID, or `structural`, `filesystem` or `other`.
- Optional new property `compose_policy_violations`: a count bucket (`1`, `2-3`, `4+`), once violations are collected.
- Bump `FAILURE_CLASSIFIER_VERSION` to 8.
- Once rewrites ship, add `compose_rewrites` to `deploy_succeeded`, using the same fixed IDs, so we can see how many deploys succeed only because of a rewrite.

Every value comes from a closed enum. No user content is sent.

### 6. How users work around a rejection today

I verified the inline-override cases with the harness.

1. **Edit the compose file and push.** This works for every rule, but needs write access to the repository. For an upstream app's file it means forking.
2. **Inline compose override** (project settings). This works for `ports`: an override that sets loopback ports replaces the repository's ports through the port-reset layer (`crates/temps-deployer/src/compose/resolve.rs:820`), and a file that failed on `ports` alone then passes. It **cannot remove a restricted key**: `container_name: !reset null` is itself rejected ("uses 'container_name', which the ContainerName check rejects"). Proposal: allow `!reset` and removal of restricted keys in overrides, since removing one is always tighter.
3. **Exclude a service** (`excluded_services`). This removes a sidecar that needs the socket or host access, such as a bundled reverse proxy or updater. Nothing suggests it today.
4. **Admin exception per check.** This works on single-admin installs, which is most of them. But the user has to find the right toggle among 80 without being told which one, and a non-admin member of a team install hits a dead end.
5. **`unsandboxed_services`.** This relaxes the injected hardening only. It does not help with a rejection.

There is no environment variable bypass, and this proposal keeps it that way.

### 7. Related failure codes, briefly

- **`compose_variable_missing`** (40 failures, 4 installations). The project-creation check in §3 should list unset required variables next to the policy findings. `check_report` already returns `unset_variables`, so the configurator can ask for them before the first deploy.
- **`compose_up_failed`** (23 failures, 6 installations). This is plausibly where the injected hardening shows up: an image that needs a dropped capability or `sudo` under no-new-privileges. Out of scope here. Worth a follow-up once the rule ID ships.

---

## Implementation plan (ordered)

Each step ships with tests in the same PR, per AGENTS.md.

1. **Telemetry rule ID.** Add `check` to the two error variants and the `[compose check: …]` token. Parse it in the classifier into `compose_policy_check`, and bump the classifier version to 8. Tests: each check maps to its ID, unknown tokens map to `other`, and the token never carries user text.
2. **Collect all violations, with clearer messages.** Return a list instead of the first error. Add the line locator, console labels and one settings-path constant. Make the banner's remediation come from the check. Tests: one assertion per rule on the field and fix text; several violations reported in one pass; correct line for nested, anchored and inherited fields.
3. **Check at project creation.** Call the existing compose-check service from the configurator and show violations, rewrites and unset variables before the first deploy. Tests: the web unit test, plus a handler test for the response shape.
4. **Safe rewrites.** A `normalize_for_policy` step runs after interpolation and before validation, in `check_configuration`, so the preview and the deploy agree. It returns the rewritten model plus `Vec<ComposeRewrite { check, service, field, before, after }>`, which are logged as deploy warnings and shown in the preview. The unchanged policy then validates the result as defense in depth. Order: `ports` → `container_name` → `security_opt` no-new-privileges → `pull_policy` → network and volume `name` → `external` volumes → `build.cache_*` / `tags` → `image` with `build` → `replicas: 0` to excluded. Tests: the rewritten output for each rule, and a property-style test that every rewrite produces a model the strict policy accepts.
5. **Narrow the false positives** listed in §4. Each one is a separate small change, with tests at the boundary (for example `../.env` passes, `../../outside` fails, a symlink escape still fails).
6. **Allow removal of restricted keys in inline overrides.** Tests: `!reset` passes; setting a value is still rejected.
7. **Regression corpus.** Add the sample as fixtures named by category only (`self-hosted-photo-library.yml`, `official-example-flask-redis.yml`), with comments and identifying strings removed, and record the expected outcome of each. Tests: a pass-rate guard on the corpus, so a new rule's false-positive cost shows up in review.

## Open questions for the maintainer

- Should the rewrites be always on, or behind a per-project column (`compose_rewrites_enabled`, default on) for users who want a strict failure instead? I lean towards always on: each rewrite only narrows and is reported.
- Is `127.0.0.1::C` (ephemeral host port) acceptable for every case, or should the rewrite keep `127.0.0.1:H:C` when `H` is free, to keep local habits?
- For `BuildImage`, when another service reuses the built tag, is it worth rewriting both references to the Temps-scoped tag, or is "Explain" enough?
