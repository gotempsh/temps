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

The Temps image installs `postgresql-client` next to the binary, so the two
tools are usually already there. The catch is the version: `pg_dump`
refuses to dump a server with a newer major version than its own, and the
client a base image packages can lag the TimescaleDB major the control
plane runs.

## Decision

- With Docker, nothing changes: the sidecar container, matched to the
  server's major version, takes the dump.
- Without Docker, the engine runs the server's own `pg_dump` and
  `pg_dumpall` with the sidecar's arguments, and streams both outputs into
  the same `backup.sql.gz` (globals first, then the database) through a
  bounded channel and a blocking gzip writer. Memory stays at a few hundred
  KiB whatever the database size; the password travels in `PGPASSWORD`,
  never on the command line; cancellation kills the tool.
- Before anything else, including the S3 checks, the engine checks that
  both tools run and report the same major version, at least the
  database's. Otherwise the backup fails permanently with what it found and
  what to install ("Install the PostgreSQL 18 client tools ... or run the
  control plane on a host with Docker").
- The startup log on a server without Docker probes the tools once and says
  what control-plane backups will do: run with the PostgreSQL N client, or
  fail and why.

## Consequences

- Control-plane backups work on microVM planes whose image carries a client
  at least as new as the database. Where it is older, the operator gets the
  version to install on the first backup, and on every start.
- The dump format is unchanged, so restores do not care which path took it.
- Restoring a control-plane backup is not changed by this decision.
