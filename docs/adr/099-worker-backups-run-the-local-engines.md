<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-099: Backups of a worker's services run the local engines through its agent

**Status:** Proposed
**Date:** 2026-11-07
**Amends:** [ADR-055](./055-workload-plane-and-runtime-drivers.md) (phase 9, "backups as jobs on the volume's node")
**Builds on:** [ADR-014](./014-unified-backup-architecture.md) (unified backup execution)

## Context

A managed database on a worker node is backed up and restored differently
from the same database on the control plane:

| Engine | Control plane | Worker node today |
|---|---|---|
| PostgreSQL (WAL-G image) | `wal-g backup-push` in the container, continuous WAL archiving, PITR | `pg_dumpall` job (ADR-055 phase 9), no archiving, no PITR |
| Redis (WAL-G image) | WAL-G stream in the container | `redis-cli --rdb` dump by the agent |
| MongoDB (WAL-G image) | WAL-G stream in the container | `mongodump` by the agent |
| MariaDB (WAL-G image) | physical `mariadb-backup` and binlog shipping, PITR | `mariadb-dump` by the agent |
| Managed S3 | bucket mirror | bucket mirror |

So the same service produces different artifacts depending on where it runs.
A control-plane backup cannot be restored onto a worker (and the reverse only
for the logical formats), the console tells a worker PostgreSQL on a WAL-G
image that it "needs WAL-G", Temps Cloud's verified restores see two layouts,
and every engine has two implementations to keep correct.

The divergence came from one constraint: the control plane cannot reach a
worker's Docker daemon, and the local engines call Docker directly (some 200
call sites across the engine crates). The worker paths were written around
that constraint instead of through it.

## Decision

**One implementation per engine. Where it runs is a transport choice.**

Each engine is split into two halves:

- **The plane half** decides and records: which engine, the S3 source and its
  pin, the WAL-G prefix and key layout, credential decryption, the size and
  metadata companion, the backup row and its WAL-G identity. It is the same
  code for every service.
- **The node half** is the part that touches the container: write the WAL-G
  settings file, set `archive_command`, recreate the container when
  `archive_mode` must change, `exec wal-g backup-push`, stage a restore. It
  is a plain function over a Docker client. For a control-plane service the
  plane calls it in process with its own Docker client, exactly as today. For
  a worker service the plane asks the node's agent, and the agent calls **the
  same function** with the node's Docker client.

### Agent operations

The agent gains a small, fixed set of operations, one per node half. Each:

- sits behind the node token like every `/agent/services/*` route;
- takes a sealed body (it carries the service's credentials and the S3
  source's), never plain HTTP;
- names the service and the operation, never a command, image, host or
  path: the engine derives everything from the service configuration, as it
  does on the control plane;
- is bounded: at most two data operations at once per node, each with a
  deadline, as the current data routes.

For PostgreSQL:

1. **WAL-G base backup.** Enable (or refresh) continuous archiving, then
   `wal-g backup-push $PGDATA` in the container with the same environment
   (WAL-G memory limits, identity, endpoint, path style), then read the LSN.
   Returns the outputs the plane half needs for size and metadata.
2. **Continuous archiving.** Enable or re-point archiving without a backup
   (the console's "repoint continuous archive source").
3. **Staged restore.** In-place restore and PITR from a WAL-G base: the
   provider's own restore, which fetches into the volume next to the live
   data while the server runs, writes the recovery settings (the recovery
   target for PITR), stops the server and swaps the restored directory in
   with the original moved aside on the same volume, then starts it and
   waits for recovery to promote. Only then is the original deleted; when
   recovery does not promote, the original is moved back and its container
   started as it was. The agent runs it in a task of its own, so a control
   plane that stops waiting cannot interrupt it between the two.

Redis, MongoDB and MariaDB get the same shape in later steps: their WAL-G
stream or physical backup, their restore, and for MariaDB binlog shipping.

### Copy local exactly

Archiving on a worker is the local mechanism, not a variant of it:

- the WAL-G credentials are written to `/var/lib/postgresql/walg.env` on the
  data volume (outside PGDATA, mode 0600);
- `archive_command` reads that file (`. walg.env && wal-g wal-push %p`), set
  by `ALTER SYSTEM` and a reload;
- `archive_mode=on` reaches the server only through a container recreate, and
  that recreate needs the user's restart confirmation (scheduled runs follow
  the same rule as local ones);
- the file is rewritten on every base backup. That is also how local keeps
  archive credentials fresh: there is no refresh between base backups. A
  source whose session token expires sooner than the backup interval breaks
  archiving until the next base backup, on the control plane as on a worker;
  fixing that is a separate decision for both.

### A recreate that knows its node

The one place the halves cannot be identical is the container a recreate
builds. The provider builds a control-plane container: ports published on
127.0.0.1, its own labels and restart policy. A worker container is created
by the agent from the plane's create request: ports on the node's bind
address, the node's DNS, the agent's labels. A provider recreate run on a
worker would therefore break the service's address.

So the recreate step is a parameter of the node half:

- on the control plane, the provider's own container specification, as today;
- on a worker, the running container's own inspected specification
  (restart policy included), with only the PostgreSQL setting being changed
  (`archive_mode`) replaced.

Both run under the same recreate guard: refuse unless the new container
mounts the same data, set the original aside, verify the same cluster came
back (system identifier, databases, roles), and put the original back
otherwise.

### What is retired

As each engine moves, its worker-only path goes:

- the `pg_dumpall` job of ADR-055 phase 9 (`engines/workload_job.rs`);
- the agent's `data-backup` and `data-restore` dump scripts
  (`externalsvc/worker_data.rs`) and their routes.

Until an engine has moved, its worker backups keep the current path and the
capability endpoint says so plainly. Existing worker dumps stay restorable
in place for as long as they are retained.

### Version skew

A plane that sends a new operation to an agent that does not have it gets a
404 and refuses the backup with "update the agent on node N". It never falls
back to the old dump silently.

## Consequences

- A worker service's backups are the same artifacts as a control-plane
  service's: the same S3 layout, the same metadata, restorable across
  control plane and workers, and by Temps Cloud's verified restores.
- PITR becomes available on workers once the restore step lands.
- The agent gains a few sealed, fixed operations and keeps no caller-chosen
  command execution.
- The worker receives the S3 source's decrypted credentials, as it does for
  the dump paths today. Scoping them per service is a follow-up.
- Each step ships with a Docker proof that runs the node half through the
  agent's handler against a real container, and a parity test: a local and a
  worker service of the same type choose the same engine and produce the
  same commands and keys.

## Order

1. PostgreSQL WAL-G base backup through the agent.
2. Continuous archiving on, with restart confirmation.
3. PostgreSQL in-place restore and PITR through the agent.
4. A control-plane WAL-G backup restored into a worker service, and the
   reverse.
5. Redis (WAL-G stream), then MongoDB, MariaDB (physical and binlog), then
   retiring the dump paths.
