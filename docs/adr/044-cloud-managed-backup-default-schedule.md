# ADR-044: A Managed Backup Destination Gets a Schedule

**Status:** Accepted
**Date:** 2026-09-11
**Author:** David Viejo
**Builds on:** ADR-042 (the cross-plugin trait seam in `temps-core`)

---

## Context

Enrolling an instance in Temps Cloud provisions a managed backup destination:
an `s3_sources` row flagged `managed_by_cloud`, backed by a bucket-scoped
credential the backend vends on the managed backup capability. The backup
engines write to it like any other S3 source, and the in-place catalog
reports each snapshot to Cloud.

Nothing creates a backup schedule for that destination. After enrollment the
Cloud settings page says "Managed backup destination is ready" and stops. A
destination no schedule points at never receives a backup. The published
offer says every Cloud plan includes nightly backups with plan-specific
retention (7, 30 or 90 days). Today that is only true for an operator who
knows to open Backups, create a schedule by hand, and pick the managed
destination from the list. Everyone else has a ready bucket and no backups,
and the page gives no hint that anything is missing.

The gap sits between two crates that do not depend on each other:
`temps-cloud` knows when the destination appears and what retention the plan
carries; `temps-backup` owns schedules. `temps-cloud` must not depend on
`temps-backup` (the backup plugin is optional and the dependency direction is
backup → cloud protocol, not the reverse).

## Decision

1. **A trait in `temps-core` carries the request across the seam.**
   `ManagedBackupScheduleProvisioner` has three methods: the schedule that
   targets an S3 source, "ensure one exists, creating the default when none
   does", and "release every schedule targeting the source". The backup
   plugin registers `ManagedScheduleProvisioner` (a small struct over the
   database connection and `BackupService`) as the implementation; the Cloud
   plugin looks it up optionally in `initialize_plugin_services`, after every
   plugin has registered. Same shape as `CloudTelemetryActivationTrigger`
   (ADR-042).

2. **The default schedule is an ordinary schedule.** Name "Temps Cloud
   nightly", six-field cron `0 0 2 * * *`, full backup of every service plus
   the control plane, tagged `temps-cloud`, retention from the plan. It is
   created through `create_backup_schedule` like any other, so it appears in
   Backups, can be renamed, retimed, retargeted, paused or deleted, and is
   never recreated behind the operator's back: "ensure" returns the oldest
   schedule targeting the source when one exists, whatever its state.

3. **Retention comes from the backend, and is never guessed.**
   `ManagedBackupCapability` gains an optional `retention_days`. Cloud fills
   it from the plan; an older backend that answers without it gets the
   Starter figure, seven days, which is the smallest promise the offer makes.
   The instance keeps the last capability it read this process. If none has
   been read yet (after a restart, say) the ensure path fetches the
   capability first and refuses to create a schedule when Cloud does not
   answer: a 30-day plan must not end up with a 7-day schedule because the
   value was not loaded.

4. **Enrollment creates the schedule; the page offers it again.** The first
   time the managed source is inserted (`UpsertOutcome::Created`), the Cloud
   service asks the provisioner to ensure the schedule, best effort: a
   failure is logged and does not fail enrollment. `ManagedBackupSetup`
   gains `schedule: Option<ManagedBackupSchedule>`, filled on every status
   read and reconcile. When the destination is ready and no schedule targets
   it, the Cloud settings page says so, states exactly what the default
   would do, and offers "Create nightly schedule", backed by
   `POST /api/cloud/backups/schedule/ensure`. The CLI mirrors it with
   `temps cloud backup-schedule ensure`, and `temps cloud status` prints the
   schedule line.

5. **Disconnect releases the schedules first.** Before the credential is
   revoked, every schedule targeting the managed destination is released:
   the one enrollment created (still tagged `temps-cloud`) is deleted, any
   other is disabled so the operator's configuration survives but nothing
   fires against a dead credential. One statement disables every schedule
   targeting the source before any delete, so a failure part-way leaves only
   "disabled, not yet deleted", which the next attempt completes. A release
   failure aborts the disconnect with the reason, leaving the link intact.
   The existing rule that the destination row is kept while backup records
   reference it is unchanged.

6. **Look-up-then-insert is serialised.** `backup_schedules.s3_source_id` is
   not unique, and enrollment, the settings page and the CLI can all ask at
   once, so the provisioner runs ensure and release behind one mutex. The
   provisioner is its own struct in `temps-backup`, built from the database
   connection and `BackupService`, so the service's internals stay private.

7. **Services that archive elsewhere are named, not silently failed.**
   Postgres WAL-G and MariaDB binlog archiving are pinned to one S3 source
   per service and refuse to move on their own, so a service pinned to the
   operator's own bucket fails every night under the Cloud schedule with a
   permanent mismatch. `ManagedBackupSetup` carries `archive_conflicts` (the
   service, its type, where it is pinned) and `managed_s3_source_id`; the
   Cloud settings page lists them with "Repoint to Temps Cloud", which calls
   the existing audited repoint endpoint, and `temps cloud status` prints
   the exact `temps services repoint-continuous-archive-source` command.
   Unpinned services are not conflicts: they default to the managed
   destination on their first archiving run.

8. **Auto-provisioned MariaDB base-backup schedules follow the same
   default.** The per-service schedule the backup plugin creates for new
   MariaDB services targeted the default S3 source. With a managed
   destination present, the binlog shipper would refuse to pin there
   (new services default to Cloud) and PITR would never start. It now
   targets the managed source when one exists, the default source otherwise.

## Consequences

- An enrolled instance backs up nightly to Cloud with the plan's retention
  without the operator doing anything, and the page shows which schedule does
  it. The offer's backup line is delivered by the software, not by a runbook.
- Status reads now do two extra indexed lookups (managed source, then its
  oldest schedule) only when the destination is ready. A failure in either
  leaves the schedule field empty and is logged; status never fails because
  the scheduler cannot answer.
- Cloud must send `retention_days` for the plan figures to apply. Until it
  does, every default schedule keeps seven days. Changing the plan later does
  not rewrite an existing schedule; the operator edits it, or deletes it and
  clicks the button again.
- A schedule created before this change that already targets the managed
  destination is recognised as "the" schedule, so upgrading does not create a
  second one. On disconnect it is disabled, not deleted, because it does not
  carry the `temps-cloud` tag.
- Repointing is the operator's call, never automatic: it strands the WAL or
  binlogs already under the old source for restores from this instance, and
  the page says so before the button.
- The backup plugin stays optional. Without it the Cloud plugin logs that
  schedules are unavailable, status carries no schedule, and the ensure
  endpoint answers with that reason.
