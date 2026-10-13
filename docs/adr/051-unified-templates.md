# ADR-051: One template schema for single- and multi-service apps

**Status:** Proposed
**Date:** 2026-09-29
**Related:** ADR-049 (monorepo path-filtered deploys); ADR-050 (path routes); `crates/temps-core/src/templates.rs`; `crates/temps-core/templates/`; `crates/temps-projects/src/handlers/templates.rs`; `crates/temps-project-groups`

---

## Context

In this ADR, a **project** (`project_groups`) gathers **services** (`projects`).
A project can hold a repository, shared variables and path routes (ADR-050).

Templates today are a single `ProjectTemplate` that always creates one service.
There are two kinds:

- **`starter`**: source code the user takes over. Examples: `nextjs-saas-starter`
  and `observability-starter`. Creating one forks the repository, or deploys
  straight from the public source. After that, the template has nothing more to
  say about the service.
- **`service`**: an upstream application pinned to a release. Examples:
  `keycloak` and `browserless`. The resolved definition is snapshotted on the
  service (`ServiceTemplateInstance`, schema `"2"`), and
  `/projects/{id}/service-template/upgrade` moves it to a newer release.

Both kinds describe the same things: a source (a git ref or an image), a preset,
resources, a port, a health check, variables with generators, and managed
databases with `managed_service_bindings`. They differ only in who owns the code
afterwards.

Many real apps are several services. Deploying Infisical by hand on this branch
took all of the following:

- a backend service (`backend/`, Dockerfile, port 8080, `/api/status`, 2 GB);
- a frontend service (`frontend/`, a static Vite build);
- a managed Postgres and a managed Redis, each bound to the backend;
- two generated secrets (`ENCRYPTION_KEY`, `AUTH_SECRET`);
- `SITE_URL` set to the frontend's URL;
- a path route sending `/api` on the frontend's hostnames to the backend.

None of that fits in a template. The alternative is a Compose template, but that
runs every container as one service: one deployment, one set of logs, one scale
setting. It also leaves the frontend's static build inside a container instead
of the static server.

Repository import has a related problem. The import dialog and
`project-groups import` create each app with three client calls: create the
service, add it to the project, make it follow the repository. Until this branch
fixed the clients to resume, a failure after the first call left a service
outside the project, and retrying created a duplicate. The clients now resume
from the failed step. The real fix is to stop making the client orchestrate.

## Decision

### 1. One schema; `services` is a list

A template describes an app made of 1..N services. The shared parts sit at the
top level. Everything that is per-service today moves into `services[]`.

```yaml
slug: infisical
name: Infisical
version: 1.0.0
kind: app                  # starter | app  (`service` is accepted as an alias of `app`)
description: Open-source secrets management.
git:                       # shared by services that don't set their own
  url: https://github.com/Infisical/infisical.git
  ref: v0.95.0
databases:                 # managed databases, from the existing catalog
  - postgres
  - redis
shared_env_vars:           # become project variables (the project's shared variables)
  - name: ENCRYPTION_KEY
    secret: true
    default_generator: random_hex_32
  - name: AUTH_SECRET
    secret: true
    default_generator: random_secret
  - name: SITE_URL
    value: "{{ services.frontend.url }}"
services:
  - name: backend
    directory: backend
    preset: dockerfile
    exposed_port: 8080
    health_check_path: /api/status
    resources: { memory_limit: 2048 }
    database_bindings:
      postgres: { DB_CONNECTION_URI: POSTGRES_URL }
      redis:    { REDIS_URL: REDIS_URL }
  - name: frontend
    directory: frontend
    preset: vite
routes:                    # ADR-050 path routes
  - source: frontend
    prefix: /api
    target: backend
    strip_prefix: false
```

Service fields are today's per-template fields, unchanged in meaning:

- `name`, plus source overrides: `directory` (from the repository root), or
  the service's own `git` / `image`;
- `preset`, `preset_config`, `command`;
- `resources`, `exposed_port`, `health_check_path`;
- `env_vars` (service-only variables);
- `database_bindings` (today's `managed_service_bindings`).

Service names are unique within a template. They become service names in the
project, with a suffix if the name is taken (§3).

**The old format keeps loading.** A document with no `services` list, or whose
`services` is a list of strings (today's managed-database list), is a
one-service template. The loader converts it as follows:

- the top-level fields become `services[0]`, named after the slug;
- the old `services: [postgres]` becomes `databases`;
- `managed_service_bindings` becomes `services[0].database_bindings`.

This is decided by the shape of the document, never by guessing. The five
bundled YAML files keep working without edits, and are rewritten to the new
shape afterwards.

`ServiceTemplateInstance` snapshots move to schema `"3"`. The loader converts a
stored `"2"` snapshot the same way, so services created from `keycloak` today
keep upgrading.

### 2. Two kinds, split by ownership rather than by service count

| | `starter` | `app` (was `service`) |
|---|---|---|
| Code | the user's, from the moment it is created (fork or copy) | upstream, pinned to `version` / `ref` / image digest |
| Snapshot kept | only for display ("created from X") | full resolved definition, used for upgrades |
| Upgrades | never | offered when the catalog has a newer `version` |
| Examples | Next.js SaaS, a monorepo starter | Keycloak, Browserless, Infisical |

`kind: service` stays accepted through a serde alias; the API returns `app`,
and the web client and CLI accept both. The name changes because "service"
now means one deployable unit, and a multi-service Infisical is not a
"service".

Databases are not a template kind. They stay in the managed-database catalog,
and templates reference them by type.

### 3. Every template lands in a project

Creating from a template takes a target:

- **New project** (the default when `services.len() > 1`). The project is named
  after the template, and all services are created inside it.
- **Existing project**. The services are added to a project the caller manages.
  Before anything is written, the server returns a conflict preview:
  - service names already taken, and the suffixed names it will use;
  - `shared_env_vars` keys the project already has (the project's value is kept
    unless the caller overrides it; secrets are never shown);
  - route prefixes already used on the same source service (refused, since
    ADR-050 prefixes are unique per source);
  - for each database, whether to reuse one of the project's databases of that
    type or create a new one.
- **No project**. This is only valid for one-service templates, and matches
  today's behaviour, so creating Keycloak does not force a grouping step.

Adding to an existing project follows the same rules as joining one: the caller
needs `EnvironmentsWrite` on every member, because the new services receive the
project's shared secrets. This is the authz rule fixed on this branch for
`add_project_to_group`.

### 4. The instance belongs to the project

A multi-service `app` is upgraded as a unit: Infisical's frontend and backend
must move to the same release together. The instance record moves from the
service to the project:

```
project_template_instances
  id, group_id → project_groups (CASCADE), UNIQUE (group_id, slug)
  slug, version, schema_version, snapshot JSONB
  created_at, updated_at
project_group_members
  + template_instance_id → project_template_instances (SET NULL)
  + template_service VARCHAR   -- which `services[].name` this member is
```

A project can hold more than one instance, for example Infisical plus a
Browserless used by one of its own services.

An upgrade computes a plan across all services of the instance:

- changed fields per service;
- services added or removed by the new release;
- new required variables;
- new databases.

It then applies the plan and deploys the changed services. Removing a service
never deletes it: the plan marks it as detached, and the user deletes it.

One-service instances created with **no project** keep today's per-service
snapshot columns (`template_slug`, `service_template_version`). They move to the
project table the first time the service joins a project.

### 5. References between services

Variable values may reference other parts of the same instance:

- `{{ services.<name>.url }}`: the public URL of that service in the same
  environment. Environments are paired the way ADR-050 pairs them: same slug,
  or same branch for previews.
- `{{ services.<name>.internal_url }}`: the service-to-service address.
- `{{ databases.<type>.<VAR> }}`: a variable the database exposes.

References are stored unresolved and resolved when a deployment's environment
is assembled, in the same place the project's shared variables are merged
today. Resolving them once, at creation time, would give every preview the
production URL. An unresolvable reference fails the deployment with the name of
the missing service and environment. It never falls back to an empty string.

The existing generators (`random_secret`, `random_hex_32`, `app_url`) run once,
at creation, as they do today.

### 6. Creation is one server-side, resumable operation

A single endpoint replaces the client-side call sequences:

```
POST /templates/{slug}/instantiate   (catalog template)
POST /project-groups/{id}/import     (repository import: an ad-hoc starter built from the scan)
  body: target, service names, variable values, database choices, idempotency_key
  → 202 { id, status, services[], steps[] }
GET  /instantiations                 → the caller's recent ones
GET  /instantiations/{id}            → per-step status
POST /instantiations/{id}/retry      → resumes at the first step not done
```

The request is checked in full before anything is stored: permissions, the
template, database types and access, required variables, and every planned
slug (reserved slugs included). The accepted plan is stored in
`template_instantiations`; variable values the steps need are stored
encrypted beside it and cleared on success. The plan never holds a value.

The steps then run in the background, in order: the project, the repository
(fork or not), new databases, each service, joining the project, shared
variables, routes, and finally every first deployment. Each step's result is
stored before the next starts, and a finished step is never run again.

Creating a project, a service and the rows around it goes through the
services that already own those rows (project groups, projects, providers),
so one transaction across all of them is not possible without moving that
logic. Instead a step that finds its planned slug taken on resume plans
another one; it never takes over an existing project or service, because
neither records who created it, and a service joining a project receives the
project's secrets. Only a managed database, which records its creator, is
adopted on resume (same name, type and creator, created after the
instantiation began). The cost: if the server stops in the instant between
creating a project or service and recording it, that row is left behind,
empty and never deployed, for the user to delete. A service is
created with its first deployment held back, and only deployed once the
project, its variables and its routes exist, so the first build never runs
without them.

Replaying the same `idempotency_key` returns the existing instantiation. It
never creates a second one. A key reused for a different request is a 409. An
instantiation that was running when the server stopped reports
`interrupted`, and retrying it continues where it stopped.

The web UI and `bunx @temps-sdk/cli project-groups import` both call this
endpoint; the client-side resume is gone. The CLI gains `templates create`,
plus `instantiations list`, `instantiations show` and `instantiations retry`,
per the CLI-parity rule. `POST /projects/from-template` stays for one-service
templates, and refuses multi-service ones with a pointer to this endpoint.

### 7. Monorepo templates depend on ADR-049

A template whose services share one repository gives the project that
repository, and each service follows it (the mechanism this branch built). A
push then fans out to every service.

Until ADR-049's path filtering ships, a one-line frontend change rebuilds the
backend too, and every build clones the repository again. Multi-service
templates from one repository therefore ship after ADR-049. Templates whose
services use separate repositories or images do not wait for it.

### 8. Where templates come from

`origin` is recorded on the instance:

- **`catalog`**: the bundled YAML (today's files) and any operator-configured
  catalog.
- **`saved`**: "Save as template" on a project. It captures the project's
  services, shared variable names (never values), database types and routes.
  Pinned refs are captured for `app`, directories for `starter`.
- **`imported`**: a template file or URL.
- **`compose`**: converted from a Compose file (below).
- **`importer`**: produced by the existing platform importers. Today they create
  loose services. They will instead produce one project per source app, which
  is how users already think of those apps.

### 9. Compose files convert to a project

A Compose file can be converted to a template, which the user reviews before
creating anything:

- Containers with `build:` or an `image:` and published ports become services.
- Containers running well-known stateful images (postgres, redis, mysql,
  mariadb, mongo) become managed databases. Bindings are inferred from the
  variables the other containers use to reach them, and the user confirms each
  one. This gets them backups and restores that a container volume does not
  have.
- Containers that share a volume with another container, or that are only
  reachable over the Compose network with no port of their own, stay together
  as one Compose service inside the project. Splitting them would break the
  volume or network they depend on.

Deploying a Compose file as a single service keeps working. Conversion is
offered, never forced.

## Alternatives considered

**Keep the two families separate and add a third "multi-service" template
type.** This was rejected because it gives three schemas that share most of
their fields, with the single-service case written twice.

**Multi-container apps only through Compose.** This was rejected for the
reasons in Context: one deployment, one log stream and one scale setting for
the whole app, no static serving, and no managed databases.

**Always create a project, even for one service.** This was rejected because
creating Keycloak or Browserless on its own should not need a grouping step.
"No project" costs nothing: the service can join one later, and its instance
moves with it (§4).

**Keep the instance per service and upgrade services one by one.** This was
rejected because a frontend and backend on different releases of the same app
is a broken state, and nothing would prevent it.

## Consequences

### Positive

- Infisical-shaped apps become a one-click template: services, databases,
  secrets, cross-service URLs and routes.
- The five existing templates and every existing `keycloak` or `browserless`
  service keep working through the loader conversion.
- Imports and template creation stop leaving orphaned or duplicated services.
- "Save as template" and the Compose conversion let users turn what they
  already run into something repeatable.

### Negative

- Snapshot schema `"3"`, a new table, and two columns on
  `project_group_members`.
- Variable references add a resolution step to deployment assembly, and a new
  way for a deployment to fail. That failure has to name what is missing.
- The instantiate endpoint is a small state machine to maintain, in place of
  three existing calls.

### Risks

- **Conflict drift.** The project can change between the checks and a step.
  A service slug taken in between is planned again at its step; a route
  prefix taken on the source service fails that step with the route that
  holds it, and the retry continues once it is resolved. A shared variable the
  project already has keeps the project's value.
- **Reference cycles.** Service A's variable can reference B's URL while B's
  references A. URLs do not depend on variables, so this is fine. References
  to another service's variables are therefore not allowed.

## Implementation order

1. Loader: the new schema, conversion of the old format and of `"2"`
   snapshots, and the `app` alias. The bundled YAML is unchanged; conversion
   tests cover all five files.
2. `project_template_instances` and the member columns. Upgrading one-service
   instances goes through the project when there is one.
3. The instantiate endpoint (§6) for one-service templates. Web UI and CLI
   switch to it, and repository import moves onto it.
4. Multi-service templates with `shared_env_vars`, `routes`, references (§5),
   and the Infisical catalog entry. This waits for ADR-049 when the services
   share one repository.
5. Save as template, then Compose conversion, then the importers.
