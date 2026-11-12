<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-100: The control-plane backup dumps with the local PostgreSQL client when there is no Docker

**Status:** Accepted
**Date:** 2026-11-09
**Builds on:** [ADR-014](./014-unified-backup-architecture.md) (unified backup execution)

## Context

The `control_plane` backup engine takes a logical dump of the Temps
database by running `pg_dumpall --globals-only` and `pg_dump` in a one-shot
`postgres:<major>` container, pulled through the local Docker daemon. A
control plane that runs in a microVM, or any server started without Docker,
has no daemon: every control-plane backup there failed when it tried to pull
the image, after its S3 checks, with a Docker connection error that said
nothing about what to install.

The startup log on such a server made it worse. It said the executor had
registered "the engines that need no local Docker (control_plane, ...)",
which is true of the worker engines and false of `control_plane`.

The Alpine image (`Dockerfile`) installed `postgresql-client` next to the
binary, and the release image (`Dockerfile.release`) installed no client at
all. Even where the tools are present, the version is the catch: `pg_dump`
refuses to dump a server with a newer major version than its own, and the
client a base image packages can lag the TimescaleDB major the control
plane runs (Alpine 3.22's is 17, Debian bookworm's 15; the control plane
runs 18).

## Decision

- With Docker, nothing changes: the sidecar container, matched to the
  server's major version, takes the dump.
- Without Docker, the engine runs `pg_dumpall --globals-only` and
  `pg_dump` as host processes with the sidecar's arguments
  (`engines::control_plane_host`, shared with the Docker-unreachable
  fallback that landed in #1863), and writes the same `backup.sql.gz`
  (globals first, then the database). The dump goes to a private (0600)
  attempt-scoped file and is then compressed in fixed-size chunks, so
  memory stays constant whatever the database size; the password travels
  in `PGPASSWORD`, never on the command line; cancellation kills the tools
  and stops the compression; an empty dump is refused rather than uploaded.
- "Without Docker" means either a server started with no Docker at all (it
  goes straight to the host tools) or one whose daemon does not answer.
  A daemon that answers always takes the sidecar path.
- Before anything else, including the S3 checks, the engine checks that
  `pg_dump` and `pg_dumpall` are on `PATH`, run, and are each at least the
  database's major version. Otherwise the backup fails with the tool it
  found missing or too old and what to install ("Install the PostgreSQL 18
  client tools ... or make the Docker daemon reachable").
- The startup log on a server without Docker probes the tools once and says
  what control-plane backups will do: run with the PostgreSQL N client, or
  fail and why.
- Both runtime images ship the PostgreSQL 18 client tools: the release
  image from the PostgreSQL apt repository (`postgresql-client-18`), the
  Alpine image from Alpine 3.23 (`postgresql18-client`). Each build fails
  if the tools report another major version, so a base image change cannot
  silently downgrade them. Moving the control plane to a newer PostgreSQL
  major means raising `PG_CLIENT_MAJOR` in both Dockerfiles first.

## Consequences

- Control-plane backups work on microVM planes whose image carries a client
  at least as new as the database. Where it is older, the operator gets the
  version to install on the first backup, and on every start.
- The dump format is unchanged, so restores do not care which path took it.
- Restoring a control-plane backup is not changed by this decision.
