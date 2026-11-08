<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-093: A worker receives S3 credentials scoped to the job, never the source's own

**Status:** Proposed
**Date:** 2026-11-08
**Amends:** [ADR-092](./092-worker-backups-run-the-local-engines.md) (its "scoping them per service is a follow-up")

## Context

ADR-092 moved worker backups and restores onto the local engines, run by
the node's agent. Every one of those operations carries an S3 destination
in its sealed body, and today that destination is the S3 source's own
decrypted credential: the access key and secret the operator typed in, or,
for Temps Cloud managed backups, the tenant's bucket token. The same
credential is written to `walg.env` on the worker's data volume, where it
stays between base backups for continuous archiving.

So one compromised worker holds a credential for the whole bucket. It can
list, read and overwrite every service's backups, every project's, the
control plane's own, and delete them. The node token also opens a second
door: `GET /internal/nodes/{id}/s3-credentials/{source}` hands a worker the
same decrypted pair for any source a backup of one of its services uses.
No agent calls that route any more.

A worker legitimately needs much less:

| Job | Keys | Operations |
|---|---|---|
| Dump or WAL-G stream backup (MongoDB, Redis, MariaDB) | that backup's own directory, `.../<service>/<date>/<uuid>/` | write, read back, list |
| Storage service mirror backup | that backup's mirror prefix, `.../s3/<service>/<uuid>/` | write, list |
| PostgreSQL WAL-G base backup and continuous archiving | the service's WAL-G repository, `.../postgres/<service>/walg/` | write, read, list (WAL-G reads its own sentinels) |
| Any restore, PITR included | the backup's own location (another service's when restoring across services) | read, list |
| Retention, `wal-g delete`, deleting a backup | none: these run on the control plane (already the case) | |

No job needs `DeleteObject`, and none needs a key outside the table.

## Decision

**The control plane derives a credential for every worker job from the
source's credential, scoped to the job's key prefixes and operations and
short-lived, and that derived credential is the only one a worker ever
receives.** It is computed in one place, `temps_providers::worker_s3_scope`,
from a grant the engine states (`prefixes`, `read` or `write`, a lifetime),
and is used by every call site that sends a `WorkerS3Destination`. The node
half of each engine is unchanged: it already takes a session token, because
Temps Cloud's credentials are temporary.

### How each kind of source is scoped

1. **Temps Cloud managed backups (R2).** The plane cannot mint R2
   credentials: that takes Cloudflare's account API, which only Cloud holds.
   It asks Cloud, with its instance token, for a credential for the job
   (`POST /v1/backups/managed/scoped-credentials`, body: `prefixes`,
   `access` `read` or `write`, `ttl_seconds`). Cloud checks every prefix is
   a plain key prefix strictly inside the tenant's own `bucket_path` (never
   the whole of it, nor Cloud's catalog), checks the plan and payment for a
   write exactly as the capability route does, and mints from R2's
   Temporary Access Credentials API with those prefixes,
   `object-read-only` or `object-read-write`, and a lifetime clamped to 6
   hours: Cloud's token rotation overlap, because R2 ends a temporary
   credential when its parent token is deleted (Cloud ADR 0119). A write's parent is the
   tenant's normal token, so a tenant revoked for quota is refused; a
   read's is the token reads use, so a revoked tenant can still restore.
   Either way the credential can never exceed its parent. A refusal that
   retrying cannot change (no storage, no paid plan, quota revoked) is a
   409 and the job is refused; a Cloud or Cloudflare failure is a 503 the
   plane retries; a 404 or 405 means the Cloud predates the route.
   R2 has no write permission without delete: within its one prefix a write
   credential can delete. The residual risk is the service's own backups,
   held by a worker that already holds the service's live data.

2. **AWS S3.** `sts:GetFederationToken` with the source's own access key and
   a session policy granting `s3:PutObject`, `s3:GetObject`,
   `s3:AbortMultipartUpload`, `s3:ListMultipartUploadParts` (write) or
   `s3:GetObject` (read) on `arn:aws:s3:::<bucket>/<prefix>*`, and
   `s3:ListBucket` on the bucket conditioned on `s3:prefix`. AWS intersects
   the session policy with the user's own, so the result can never exceed
   the source. No delete in either. The source's IAM user needs
   `sts:GetFederationToken`; the refusal says so when it is missing.

3. **S3-compatible stores with STS (MinIO, and any store implementing
   MinIO's `AssumeRole` with a session `Policy`).** The same session policy
   through `AssumeRole` against the source's endpoint, signed with the
   source's key.

4. **Stores that cannot scope** (Garage; an operator's own R2 bucket, whose S3
   keys cannot mint; any store whose STS call is rejected as unsupported):
   **the job is refused** with what is missing and the setting that would
   change it. Nothing falls back to the source's credential on its own.
   An operator who accepts the risk can set the source's **worker
   credentials** to `allow_full` (`s3_sources.worker_credentials`, an
   audited column, `scoped` by default). Every job that then sends the full
   credential logs a warning naming the node, the service and the source,
   and the source's page in the console says that worker jobs receive its
   full credential.

A transient STS or Cloud failure (timeout, 5xx) fails the job as retryable;
it is never read as "cannot scope".

### Why refuse, and not proxy through the plane

A plane-proxied transfer (the worker uploads to the plane, which holds the
credential) scopes every store, but:

- continuous archiving would put the plane in the write path of every
  worker database: `archive_command` blocks WAL recycling while the plane is
  unreachable, so a plane outage fills the worker's disk;
- every byte of every worker backup would cross the plane twice, on a plane
  sized at 1 vCPU and 2 GB;
- it is a new S3 endpoint to authenticate, bound and keep correct.

Most real sources (AWS, R2 via Cloud, MinIO) can scope, so the price falls
on the few that cannot, and refusing tells their operator exactly what to
do. A proxy can be added later behind the same grant without changing the
engines.

### Lifetimes, and archiving

- A one-shot job (backup, restore) gets a credential living 12 hours, more
  than any backup's runtime limit.
- PostgreSQL's base backup writes the credential into `walg.env`, where
  `archive_command` keeps using it. That credential is the archive
  credential: `write` on the WAL-G repository, living 24 hours (6 on a
  Temps Cloud managed source). The plane refreshes it every 2 hours
  through a new agent operation,
  `POST /agent/services/postgres/walg-credentials`, which rewrites
  `walg.env` and nothing else (no restart, no `archive_command` change;
  refused when archiving is not set up). A plane down for up to 22 hours
  (4 on a managed source) therefore loses no WAL; longer, and archiving
  retries until the plane is back, as with any failed `archive_command`.
- On a managed source a one-shot job also gets at most 6 hours, so a
  worker backup or restore running longer fails when its credential
  expires.
- Scoped credentials cannot be revoked individually; the lifetime is the
  revocation window.

### What the worker can no longer do

- Read, list or overwrite another service's backups, or the control
  plane's.
- Delete anything, except on R2 within the prefix of its own job.
- Use a credential after its lifetime.
- Obtain the source's credential through the node API: the
  `s3-credentials` route is removed.

## Consequences

- Retention and deletion stay on the control plane; nothing about them
  changes.
- Every worker backup and restore costs one STS or Cloud call. They are
  control-plane calls, not hot-path ones.
- AWS sources need `sts:GetFederationToken` on their IAM user, and MinIO
  sources need STS enabled. An unscopable source makes worker jobs refuse
  until the operator either moves to a store that scopes or accepts `allow_full`.
- Temps Cloud gains the scoped-credentials route (temps-cloud-app); a
  plane talking to a Cloud without it refuses managed worker jobs with
  "Temps Cloud cannot scope credentials yet", never sends the bucket token.
- The control plane's own services keep using the source's credential:
  the plane holds it anyway.
