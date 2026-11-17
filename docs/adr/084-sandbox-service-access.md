# ADR-084: Sandbox access to linked services

> **Renumbered:** this ADR was numbered 051 until 2026-10-31, when duplicate ADR numbers were removed. Older references to "ADR-051" may mean this document; the [ADR index](./README.md#renumbered-adrs) lists every renumbered ADR.

**Status:** Proposed
**Date:** 2026-10-17
**Author:** David Viejo
**Builds on:** ADR-013 (sandbox egress credential proxy, "Phase 2: linked-service credentials"), ADR-036 (persistent workspace sandboxes)

---

## Context

A chat that builds an app in its sandbox also creates the project and the
services the app needs (a PostgreSQL database, an object store). Today the two
halves cannot meet:

1. **The network path exists, but chats never use it.** A sandbox sits on an
   isolated bridge with a per-sandbox egress proxy (ADR-013). For sandboxes
   attached to a project, `SandboxService::synchronize_data_network` already
   builds a per-sandbox *internal* network that contains the sandbox and the
   database containers of the project's linked services, and nothing else. A
   chat sandbox is created without a project unless the chat is a project
   chat, and the assistant chat that creates a project and its database never
   attaches either, so the path is never built.
2. **No scoped credential.** The credentials Temps hands a deployment
   (`get_runtime_env_vars`) name a database per project environment
   (`<project>_<environment>`) but authenticate as the cluster's administrator
   (`postgres`) with the administrator's password. Whoever holds them can read
   every other database on the same cluster. That is acceptable for a deployed
   app built from reviewed code; it is not acceptable for a sandbox that runs
   whatever the model writes. `SandboxService::runtime_environment` and its
   endpoint (`POST /v1/sandboxes/{id}/runtime-environment`) hand exactly those
   production credentials to a sandbox today.
3. **Environment parity is left to the user.** The assistant has told users to
   copy connection details into environment variables by hand, and to type
   passwords themselves. Moving between "local" (the sandbox) and "production"
   is where configuration usually breaks, and nothing in Temps owns that move.

## Decision

### 1. One contract across environments

The app reads the same, fixed variable names everywhere (`POSTGRES_URL`,
`POSTGRES_HOST`, `POSTGRES_PORT`, `POSTGRES_DB`, `POSTGRES_USER`,
`POSTGRES_PASSWORD`, and the equivalents of other service types). Code never
changes between environments. **Only the values change, and only the
environment decides them:**

| Where the code runs | Database it gets | Role it connects as |
|---|---|---|
| Sandbox | `<project>_sandbox`, its own database | a scoped role for that database only |
| Production | `<project>_production` (unchanged) | unchanged for now |
| Other environments | as today | as today |

A sandbox **never** receives production credentials and never touches
production data. Data is not copied between environments; the schema travels as
migrations in the repository, which the app runs on start in every environment.
The assistant writes names only (`.env.example`), never values, into the
workspace, so a secret cannot reach Git.

### 2. A scoped role per sandbox database

Provisioning the sandbox database also creates a role that can use that
database and nothing else:

- `NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION`, a connection limit, and
  ownership of the one database, so migrations work without any wider right.
- `CONNECT` on the database is revoked from `PUBLIC` and granted to the role
  only. The same revoke is applied to the other databases Temps manages on the
  cluster, so the role cannot connect to production and the older default
  (`CREATE` on `public`) cannot be used against it.
- The password is derived, not stored: `HMAC-SHA256(admin password,
  "temps-scoped-role-v1:" || service id || ":" || database)`. It is stable
  across calls, so an app keeps working between restarts, and it changes when
  the administrator password does. Provisioning is idempotent and re-syncs the
  role's password on every call.

### 2b. One engine per service type

The sandbox layer knows nothing about any one engine. A service type that can
hand out a credential limited to a namespace of its own implements
`SandboxAccessEngine` (`temps-providers/src/sandbox_engines.rs`) and is listed
in `ENGINES`; supporting another type is that one implementation and one line.
An engine answers two questions: which containers the sandbox must dial
(`containers`), and the variables to run with (`issue`). Everything shared
stays outside it: the link check, the data network, merging and redacting the
variables, the audit trail.

A type with no engine, a backend an engine does not cover, or a service on
another node, is left out: it is neither reachable nor given the deployment's
credentials. The variables an engine returns name one endpoint consistently
(a `host` and `port` pair that can be dialled as given, and a `POSTGRES_URL`
naming the same single writable member, because the common Node and Python
drivers reject a multi-host URL). Variables are resolved again for every
command, so a failover shows up in the next one.

| Type | Namespace | Credential |
|---|---|---|
| PostgreSQL | database `<project>_sandbox` | role limited to it, derived password |
| S3 / RustFS | bucket `<project>-sandbox` | temporary key from the service's STS `AssumeRole`, restricted by a session policy to that one bucket, valid for 12 hours and issued fresh for every command |
| Redis | key prefix `sbx:<project>:` | ACL user (`reset on`, keys and channels `sbx:<project>:*`, `+@all -@admin -@dangerous`), derived password |
| MariaDB | database `<project>_sandbox` | user granted that database only (underscores escaped in the grant, where `_` would otherwise be a wildcard), connection limit, derived password |
| MongoDB | database `<project>_sandbox` | user defined in that database with `readWrite` on it and no other role, derived password |

The S3 key is never stored and expires by itself, so a leaked value stops
working; the service's root key stays on the control plane, which also creates
the bucket (the temporary key may use it, never create it). The other engines
re-run their provisioning on every connect, so a user or grant changed by hand
is put back, and derive passwords from the service's administrator secret, so
nothing new is stored and a rotation reaches the sandbox on its next command.

Redis has no per-sandbox database, so isolation is a key prefix and code in the
sandbox must use `REDIS_KEY_PREFIX` (most clients have a `keyPrefix` option).
`INFO` is denied with the rest of `@dangerous` because it reports every
tenant's keyspace; clients that call it on connect (ioredis's ready check) need
that check turned off. A Redis service with no password is refused: every
client is the default user, so no sandbox could be limited.

### 3. The network path is the existing data network

No new container, and no forwarder: a forwarder would be one more process to
secure and keep alive, while the network already isolates. A connected sandbox
is attached to its per-sandbox internal data network, which holds only the
containers each engine names for the services the user connected. The scoped credentials name
those containers as hosts, on the container port, so the same names work
inside the sandbox. The connection set is the sandbox's rows in
`sandbox_services` (see 3b); the network is rebuilt from it, so disconnecting
also tears down open sockets (the reconciliation already disconnects the
sandbox first).

Supported first: the Docker backend, services running on the control plane. A
service on another node, or another backend, is refused with a message that
says why; it never falls back to the shared credentials.

### 3b. Sandboxes link to services the way projects do

`sandbox_services` is the mirror of `project_services`: one row per
`(sandbox, service)`, the single source of truth for the data network, the
per-command environment, the console and the audit trail. It replaces the list
of project ids that used to live in the chat's metadata, so the link belongs to
the sandbox and survives the chat being relinked or the sandbox waking up.

- A row records the project it was made through, if any. That decides the
  namespace: `<project>_sandbox` / `<project>-sandbox` for a link made through
  a project, a name derived from the sandbox id (`sbx_ab12…` becomes `sab12…`)
  for one made without. The same service linked to two sandboxes therefore
  gives two separate databases or buckets.
- "Connect a project's databases" is shorthand for linking every supported
  service of the project. A single service, even one that belongs to no
  project, can be linked or unlinked on its own
  (`POST /ai/conversations/{id}/sandbox/services`,
  `DELETE …/sandbox/services/{service_id}`).
- Unlinking removes the sandbox from the network and revokes what its next
  command receives. It never deletes the database or bucket, so linking again
  finds the same data.
- Unlinking or relinking the chat's sandbox releases its links (least
  privilege); the service rows cascade away with the sandbox or the service.
- The engines return the variable names a deployment gets, so code written in
  the sandbox runs unchanged once deployed.

### 4. Credentials are injected per command, server side

Connections are recorded in `sandbox_services` by service id only. On every
`sandbox_exec` the server resolves the variables fresh and passes them in the
command's environment, naming the database containers on the data network. Nothing is baked
into the container or written to a file, so revoking a connection or rotating a
password takes effect on the next command. Values are removed from the output
shown to the model and the transcript. A server started in the background keeps
the values it started with; the assistant restarts it after a change.

### 5. Consent, authorization, audit

Connecting is the user's decision, shown where the work happens: an option when
a sandbox is created for a project chat, and a control on each service the chat
made. It needs the permissions of issuing runtime credentials
(`ExternalServicesWrite`, `SecretsRead`, access to the project) and is audited
with variable names only, never values. The assistant may ask the user to
connect (an offer card, like the sandbox offer); it cannot connect on its own.

## Phases

1. **Provider:** scoped role and sandbox database provisioning for PostgreSQL
   (standalone and cluster), the `SandboxRuntimeCredentialsProvider` seam gains
   a sandbox-scoped issuance, and `runtime_environment` stops handing
   production credentials to a sandbox for PostgreSQL.
2. **Chat link:** the recorded connections (project ids), the data network
   built from them, per-command injection into `sandbox_exec`, output
   redaction, endpoints, audit.
3. **Surface:** the option at sandbox creation, the control on "Made here"
   service rows, the offer card, and a `connect-a-service` playbook that covers
   both environments.
4. **Other service types** (S3 done; each further type is one engine), and an
   opt-in move of production to scoped roles.
5. **Deploy from the sandbox:** `temps_deploy_sandbox_source` packs the
   sandbox's files (without `node_modules`, `.git` or secrets) and deploys them
   as an uploaded source, after the user approves, so a project needs no Git
   repository to ship what the chat built.

## Consequences

- Production behaviour does not change in phases 1 to 4, with one exception:
  `runtime_environment` for an application-workspace sandbox now prefers the
  scoped PostgreSQL credentials over the deployment's. Nothing that runs today
  is migrated.
- A sandbox database is real PostgreSQL on the user's cluster: it uses disk and
  connections, and survives the sandbox. It is named for the project, so a new
  chat on the same project continues with the same data.
- Moving production to scoped roles needs an ownership migration for existing
  databases and is deliberately a separate, opt-in step.

## Not decided here

- Seeding a sandbox database from production (a masked snapshot).
- Remote nodes and the Firecracker and microsandbox backends.
