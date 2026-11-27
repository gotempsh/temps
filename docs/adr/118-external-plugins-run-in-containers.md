<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-118: External plugins run in containers

**Status:** Proposed
**Date:** 2026-11-27

**Superseded in part by:** [ADR-119](./119-a-plugin-is-a-temps-service.md)

Related: ADR-054 (plugin builds on worker nodes), ADR-092 (plugin AI tools), ADR-055 (workload plane and runtime drivers).

## Context

An external plugin is a native executable that Temps starts as a child process. Until now it ran as the Temps operating-system account: it could read the instance data directory (encryption and authentication keys included), the control-plane database credentials in the process environment of its parent, and every other plugin's files. The permission system (`ApiRead`, `ApiWrite`, host permissions, grants) decided what a plugin could ask the platform API for, but nothing stopped the binary from going around the API. A plugin was therefore exactly as trustworthy as Temps itself, and installing one written by someone else meant trusting that person with the server.

That is what keeps plugins from being easy to share. The goal is that a plugin can come from anyone: once the only way a plugin reaches the platform is the API, with the permissions an administrator granted, who wrote it matters much less.

Temps already launches hardened containers for deployments (`temps_workload_docker::hardened_host_config`: every capability dropped, `no-new-privileges`, a process limit, an init process). The same machinery can hold a plugin.

Constraints that shaped the decision:

- Installed plugins must keep working across an upgrade.
- The reference server has 4 GB of memory, so a container per plugin has to be cheap and bounded.
- Runtime configuration is a column on the row it configures, changed through the API and audit-logged, never an environment variable.
- A capability that is unavailable has to say why in the console instead of disappearing.

## Decision

### D1. A plugin's isolation is a column, and a new installation is a container

`external_plugin_actors.isolation` is `container` or `process` (a `CHECK` constraint refuses anything else). It is what an administrator asked for. What actually separates a running plugin is reported separately as `container`, `separate_user` or `process`, because a server started with the plugin launcher (`--plugin-launcher`) isolates every plugin as another operating-system user and that takes precedence over the column.

A new installation is stored as `container` unless the administrator explicitly asks for `process` at install time. An update from the same source keeps the stored value, whatever the update request says, so an update can never move a plugin out of its container.

### D2. What the container is

One container per plugin, created from the deployment hardening and tightened:

| | |
|---|---|
| Image | `debian:bookworm-slim`, pinned by digest: the same base the release image uses, so a binary built for a glibc Temps runs unchanged. It is pulled once and shared by every plugin. |
| Executable | A copy made from the file descriptor that was hashed, re-hashed after the copy, bind-mounted read-only at `/plugin`. The container cannot run anything but the verified build. |
| Storage | The plugin's own data directory at `/data`, a private socket directory at `/run/temps-plugin`, and a 64 MiB `tmpfs` at `/tmp`. Nothing else is writable: the root filesystem is read-only. |
| Identity | A non-root user: the Temps account's uid and gid, or 65532 when Temps runs as root (the bound directories are then handed to that user without following symlinks). |
| Limits | Memory with no additional swap, CPU and process limits; no restart policy; removed when it exits. |
| Network | The default Docker bridge: outbound only, no published port. |
| Never present | The control-plane database URL, the instance data directory, the Docker socket, other plugins' directories. |

Temps keeps talking to the plugin exactly as before: the staged handshake over the container's stdin and stdout, then the Unix socket the plugin binds in its socket directory. The plugin SDKs need no change.

Containers carry a label derived from the installation's data directory. On startup Temps removes containers carrying its own label that a previous run left behind, and only those.

### D3. A container is never given privileged access

A manifest that sets `requires_db` or `requires_host_data_access` is refused in a container, after the plugin identifies itself and before it is sent any launch configuration. The refusal names the plugin and what it asked for. Such a plugin runs only as `process`, which is an explicit, audited administrator choice.

This is the seed of the two tiers planned next: a community plugin is always contained and cannot ask for these; a verified plugin may, after review.

### D4. Upgrades change nothing until an administrator acts

The migration marks every existing plugin row `process`. The chosen isolation is also written to the installation's on-disk activation record, and at start the order is: the row, else the activation record, else `process`. An installation with neither predates this change and keeps running as it always has; an installation whose database commit was lost keeps the isolation it was activated with rather than being downgraded.

The console shows such a plugin as **Not contained** and offers **Move to container**. Today's signed-registry plugins are the future verified tier; this is their migration path, and it is taken per plugin, when the administrator chooses.

### D5. Moving a plugin is one audited, reversible operation

`PUT /x/plugins/{name}/isolation` requires `SystemAdmin` and recent sensitive-action verification. The request is audit-logged before it runs (leaving a container is a grant of everything the Temps account can reach), and its outcome afterwards. The plugin is restarted with the new isolation; if it does not start, the previous isolation is restored and the plugin started again, so a failed move never leaves a working plugin stopped.

### D6. Unavailable is explained, not hidden

A container needs Temps running directly on a Linux host, a glibc build, and a reachable Docker daemon. A Unix socket does not cross the virtual machine Docker runs in on other operating systems, and when Temps itself runs in a container the Docker daemon does not see plugin files at the paths Temps uses.

Where that does not hold, `GET /x/plugins/sandbox` reports `configured: false` with the reason and a `setup_path`. A plugin stored as `container` fails closed: it does not start, and the reason is shown next to it. A new installation is refused before it is downloaded, built or executed, unless the administrator chose `process`. Temps never silently runs a plugin without the isolation that was asked for.

### D7. Errors that reach an administrator carry no local paths

A refusal (unavailable sandbox, privileged access) explains itself in full. A storage or Docker failure can name host paths, so its public text is generic and the detail stays in the server log.

## Consequences

- A newly installed plugin can no longer read Temps' keys, database or other plugins' data. Its reach is the platform API and its own outbound network access.
- Each running plugin costs one container. The base image is shared and the limits are ceilings, not reservations. Stopping idle plugins and starting them on demand is decided separately.
- Plugins that need the database or the data directory cannot be contained. They remain possible, as an explicit choice with a visible warning.
- A Temps server that itself runs in a container (the Compose release) cannot contain plugins yet. It says so. Making that work needs the plugin's files in a volume the daemon can mount by name; it is not part of this decision.
- Outbound network access is not yet declared or restricted per plugin, and resource limits are one built-in default. Both are intended to become per-plugin settings shown at install time.

## Decisions taken without a precedent in the code

These were chosen conservatively and are for the owner to confirm:

1. **Default limits**: 512 MiB of memory, one CPU, 256 processes per plugin container.
2. **Base image**: `debian:bookworm-slim` by digest, rather than a purpose-built minimal image.
3. **Network**: the default bridge with unrestricted outbound access, so plugins that call external APIs keep working, until outbound access is declared per plugin.
4. **Fail closed**: a plugin stored as `container` does not start on a server that cannot contain it, rather than falling back to a process.
5. **Step-up action**: changing isolation reuses the `change_external_plugin_grants` sensitive action, as plugin AI consent does, instead of adding a new one.
