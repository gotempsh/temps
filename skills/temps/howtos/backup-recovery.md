# Backup and recovery

Read [the backups reference](../references/commands/backups.md) and, for a
specific managed service, [the services reference](../references/commands/services.md).

## Backup workflow

1. Inspect sources, schedules, retention, destinations, recent runs, immutable
   artifact IDs, sizes, checksums, and engine-verification status.
   Confirm through runtime help which artifact namespace the chosen restore
   command accepts; do not mix Cloud backup IDs, schedule-run IDs, and service
   backup IDs or expand a short ID by guesswork.
2. Before enabling a schedule, confirm source, frequency, retention, storage
   destination, encryption ownership, and expected recovery objective.
3. After a run, require upload completion, provider-observed size/checksum, and
   engine restore verification. “Object exists” is not a restore proof.
4. Report the immutable artifact and verification evidence without exposing
   signed URLs or credentials.

## Restore workflow

Restore is destructive or can create billable infrastructure. Obtain explicit
confirmation immediately before execution. Name the source artifact, recovery
point, destination service/volume, overwrite behavior, and expected downtime.

Prove the target service belongs to the intended project and environment; a
CLI target context identifies only the Temps server. Before an in-place
restore, define maintenance/write fencing, active-connection draining, a fresh
pre-restore safety backup, rollback criteria, and a downtime estimate based on
a comparable restore test. Stop if the platform cannot provide those controls.

Prefer restoring into a new isolated service. Verify engine startup and inspect
structural evidence appropriate to the engine: databases/tables and estimated
rows for SQL, databases/collections for MongoDB, key counts/types for Redis,
and bucket/object counts plus bytes for S3-compatible storage. Switch traffic
only after the user approves the verified destination.

Use the platform's bounded data-inspection API for structural evidence where
available. Do not reveal a connection string merely to run verification. If
credential-safe inspection is unavailable, report that verification gap
instead of claiming engine proof.
