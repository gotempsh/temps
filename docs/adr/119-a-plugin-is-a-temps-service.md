<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-119: A plugin is a Temps service

**Status:** Proposed
**Date:** 2026-11-27

**Supersedes in part:** [ADR-118](./118-external-plugins-run-in-containers.md)

Related: ADR-083 and ADR-112 (a service is one deployable unit), ADR-054 (plugin builds on worker nodes), ADR-092 (plugin AI tools).

## Context

ADR-118 put a plugin's executable in a hardened container that Temps starts itself. That closed the hole (a plugin could read everything the Temps account could), but it left Temps running a second, private container runtime next to the one it already has for deployments: its own image, storage layout, limits, start and stop, and no logs, previews or rollback. It also could not work where Temps itself runs in a container, and an idle plugin still held memory.

Temps already runs other people's code safely and conveniently: that is what a deployment is. A service (a `projects` row) is built from a repository, runs in a hardened container with its own storage and limits, has logs, previews and rollback, and can stop when idle and start on the next request.

So the direction is: **a plugin is an ordinary Temps service, and running as one is the only way a plugin runs.** The owner accepted that this is a breaking change.

## Decision

### D1. Temps never runs plugin code

A plugin is a service someone deploys like any app. An administrator then adds that service as a plugin. Temps attaches to the service's running container over the network; it starts no executable, mounts nothing and passes nothing on a command line.

Nothing about the deploy pipeline changes, and a plugin needs no configuration to be reachable: the SDK reads what every deployment already receives (`PORT`, `TEMPS_API_TOKEN`, `TEMPS_API_URL`).

### D2. How Temps and a plugin recognise each other

Temps reads the manifest with an authenticated `GET /_temps/plugin/hello`, then opens the platform channel (`/_temps/channel`) to the same address. Every request carries a secret in `x-temps-auth-signature`.

The secret is the lowercase hex SHA-256 of the context string `temps-plugin-service-auth-v1`, a NUL byte and the service's deployment token. The deployment token is the one the pipeline already gives the container as `TEMPS_API_TOKEN`; Temps reads the same token back, as it does for a service's cron endpoints. Nothing is exchanged, stored twice or added to the pipeline, and the token itself never travels to the plugin's port.

A service that does not answer the hello is not a plugin and is refused with that reason. A service started with another instance's token refuses Temps, and the refusal says to redeploy.

### D3. What a plugin can reach

A plugin that runs as a service is never given the control-plane database or the instance data directory. A manifest that asks for either is refused, by the SDK at start and by Temps at attach. The plugin's own data goes in the service's volume (`/data` by default) or a database linked to the service. Everything else goes through the platform API with the permissions an administrator granted. A newly added plugin has none.

### D4. The plugin row names its service

`external_plugin_actors` gains `project_id`, `environment_id` and `manifest`, and `isolation` accepts `service`. The references are `ON DELETE SET NULL`: deleting the service keeps the plugin row, which then says its service is gone, instead of silently removing something an administrator granted permissions to.

A plugin name belongs to whatever was installed under it first. A service never takes over the name, and with it the grants, of a plugin from another source, and a service that starts declaring a different plugin name is refused until an administrator removes the plugin and adds the service again.

### D5. Deployments, restarts and idle stops are followed, not configured

The address of a plugin is resolved from the deployment's containers by the rules the route table uses (another node's data address and published port; the published port when Temps runs on the host; the container name when Temps runs in a container), for every request.

- **Redeploy or container restart**: the channel closes; Temps attaches to whatever runs now, retrying for under a minute, then leaves it to the next request.
- **Temps restart**: the plugin keeps running and accepts a new channel. Temps attaches again at start from the stored rows.
- **Idle stop**: the plugin keeps its routes, navigation and subscriptions from the manifest stored on its row and is reported as stopped, not failed. The next authenticated request, or an event the plugin is subscribed to and granted, starts the environment through the existing on-demand manager and is then served. An anonymous request never starts a plugin. A start that just failed is not repeated for ten seconds; an event that cannot be delivered is dropped with a warning naming the plugin.

A plugin Temps is not attached to is listed with the reason. It never disappears.

### D6. Removing a plugin leaves the service alone

Removing a plugin revokes its actor and its grants and stops routing to it. The service keeps running; stopping or deleting it is done from the service's own page, with the confirmation that already exists there.

### D7. Surfaces

`GET /x/plugins/services` lists service plugins with their state and is the capability endpoint (`configured`, `reason`, `setup_path`). `POST /x/plugins/services` adds one; it requires `SystemAdmin` and recent sensitive-action verification, and is audit-logged before Temps connects and with its outcome. The console has **Add from service** on Settings > Plugins and shows each plugin's state and service; the CLI has `temps plugin services` and `temps plugin add-service`.

## Sequencing

1. Additive: the service runtime described above, next to the executable runtimes of ADR-118.
2. Breaking: the executable runtimes (process, plugin launcher, plugin container) and the binary install paths are removed. After that upgrade, a plugin installed as an executable does not start; Settings > Plugins lists it as needing to be added again from a service, with the reason. Its data and its row are kept. Nothing is migrated or redeployed on the administrator's behalf.

ADR-118's reasoning about what a plugin must not reach stands. Its container, image, limits and isolation column are what step 2 removes.

## Consequences

- Publishing a plugin is sharing a Git link. Logs, previews, rollback, resource limits, volumes and idle stop come from the deployment machinery, with no plugin-specific copy.
- Plugins work wherever deployments work, including a Temps that itself runs in a container and services on worker nodes.
- An idle plugin holds no memory when its environment stops when idle, which is what a 4 GB server needs.
- A plugin has one channel, so Temps attaches to one container. A plugin service runs with one replica.
- The signed registry publishes native executables today. Until its entries name something deployable (a pinned image or a Git commit), a registry plugin cannot be installed once step 2 lands, and the console says why.

## Decisions taken without a precedent in the code

These were chosen conservatively and are for the owner to confirm:

1. **"Service" means a Temps deployment** (a `projects` row), per ADR-083.
2. **Stop and explain on upgrade**: existing executable plugins are not migrated automatically.
3. **The secret is derived from the deployment token**, following the `CRON_SECRET` precedent, rather than a new per-plugin credential. Rotating the deployment token therefore needs a redeploy of the plugin, as it does for cron.
4. **One replica**: Temps attaches to the deployment's first reachable container.
5. **AI-tool consent is per build**: a new deployment is a new build, so consent given to a plugin's AI tools is asked for again after each deployment.
6. **Events start an idle plugin** only when the plugin is granted events; otherwise they are not delivered.
7. **Default data directory `/data`**, falling back to a temporary directory with a warning when no volume is mounted.
