<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-120: Plugin tiers, permissions, builds and controls

**Status:** Proposed
**Date:** 2026-11-28

Related: [ADR-119](./119-a-plugin-is-a-temps-service.md) (a plugin is a Temps service), ADR-092 (plugin AI tools).

## Context

ADR-119 made a plugin an ordinary Temps service, so a plugin written by anyone runs in its own container and reaches Temps only through the platform API. What is left is what an administrator agrees to when adding one, and how they keep control afterwards: who stands behind the plugin, exactly what it may do, which build of it runs, and how to stop it.

Each decision below is a column on the plugin's row or its grants row, changed through the API and audit-logged. None is an environment variable.

## Decision

### D1. Two tiers, recorded on the plugin row

`external_plugin_actors.tier` is `community` or `verified`.

- **Community**: a plugin anyone published. It runs as a Temps service in its own container. It is never given the control-plane database or the instance data directory: a manifest that asks for either is refused by the SDK at start and by Temps at attach. It keeps its data in a database or volume of its own.
- **Verified**: a plugin from the signed registry that was reviewed, which may ask for more.

Every plugin added from a service is `community`. Nothing in the API can set `verified`.

**Verified is reserved, not implemented.** The signed registry lists native executables by digest, and nothing runs those any more (ADR-119). Until a registry entry names something deployable (a pinned image or a Git commit) and Temps can check that a service was built from it, there is no way to know that a running service is the reviewed plugin, so no plugin can be given the tier. That registry format is a separate decision.

### D2. A community plugin's own database

A plugin's own database is a managed database linked to the plugin's service, exactly as for any other service (`project_services`, whose default mode gives each service and environment a database of its own on that server). Temps does not create or link one on the administrator's behalf: the plugin list shows what is linked to each plugin's service, or that nothing is, and links to the service's storage page.

### D3. A permission to change is narrowed by area and by service

`api_write` used to mean every change the delegated API allows, in every service. It is now the outer permission only; how far it reaches is two columns on `external_plugin_grants`:

- `write_areas`: the areas it may change, out of `projects`, `environments` and `deployments` (the three resources a plugin can address through the delegated API). **Empty means nothing may change.**
- `write_project_ids`: the services (projects) its changes are limited to. `NULL` means every service; otherwise 1 to 100 existing services.

The plugin channel checks both before it delegates a mutating call, in addition to the permission itself and to the acting user's own permissions, which the real handler still enforces. The service a call targets is read from the record it names (`/projects/{id}`, or the owning project of an environment or deployment) or, for a collection, from `project_id` in the body. A plugin limited to some services cannot create a service, and a call whose target service cannot be determined is refused. A refusal says which area or service was outside the limit, and is audited like any other denied host operation.

Reads are not narrowed by this decision: `api_read` and the `*_read` permissions keep their instance-wide meaning.

### D4. What a plugin asks for is shown before it is allowed

Adding a service as a plugin opens its permission screen at once; the plugin has no permissions until an administrator allows some there. The screen is built from one response (`GET /x/plugins/{name}/grants`) and lists:

- only the permissions the plugin's manifest asks for, each in plain words, plus any it still holds but no longer asks for, so that it can be taken away;
- for a permission to change, the areas and the services (D3);
- the routes the plugin serves without a Temps session (`public_paths`);
- the platform events it subscribes to;
- what its container may connect to: the outbound profile of the environment it runs in (`open`, `restricted` or `none`, from the network policy), with a link to where that is changed.

Outbound access is reported from the network policy that is actually enforced on the plugin's service, not from something the plugin declares: the manifest has no outbound declaration, and a declaration nothing enforces would be a promise, not a control.

## Decisions taken without a precedent in the code

These were chosen conservatively and are for the owner to confirm:

1. **Existing plugin rows become `community`**, the tier with the least access.
2. **A stored tier this build does not know is read as `community`.**
3. **A plugin's database is linked by the administrator**, following the existing link between a service and a managed database, instead of being provisioned automatically when a plugin is added.
4. **Write areas are named after the delegable API resources**: `projects`, `environments`, `deployments`. The console calls the first one "Services".
5. **An existing `api_write` grant gets no area**, so after the upgrade a plugin that could change things changes nothing until an administrator chooses what. The alternative, giving existing grants every area, would keep them working and leave them as broad as before.
6. **The service limit applies to changes only**, holds at most 100 services, and `NULL` means every service.
7. **A plugin limited to some services cannot create services**, and a collection call must name `project_id` in its body to be attributed to a service.
8. **Outbound access is reported, not declared or changed, on the permission screen.** A new plugin's service keeps the default outbound profile for deployments, which is open; an administrator narrows it in the network policy.
