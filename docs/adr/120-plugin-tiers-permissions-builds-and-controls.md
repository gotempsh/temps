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

## Decisions taken without a precedent in the code

These were chosen conservatively and are for the owner to confirm:

1. **Existing plugin rows become `community`**, the tier with the least access.
2. **A stored tier this build does not know is read as `community`.**
3. **A plugin's database is linked by the administrator**, following the existing link between a service and a managed database, instead of being provisioned automatically when a plugin is added.
