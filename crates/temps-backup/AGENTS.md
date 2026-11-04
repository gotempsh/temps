# temps-backup

Backups and restores of the control plane and managed services. Adds to
[`AGENTS.md`](../../AGENTS.md), which still applies.

## Owns

- `BackupEngine`s, the in-process executor, schedules, recovery sets, retention,
  S3 destinations and lifecycle rules, restore runs, the pre-migration dump.
- Not restore mechanics: the `ExternalService` trait is in `temps-providers-core`,
  implemented in `temps-providers-{postgres,mariadb,mongodb,redis,s3}`.
- Not Postgres major upgrades (`PostgresUpgradeService` in `temps-providers`; only
  its routes are here), the job runtime (`WorkloadPlane` in `temps-workload-plane`)
  or project blob storage (`temps-blob`; `s3_mirror` only copies such a service).

## Entry points

- `plugin.rs`: `BackupPlugin` wires services, engines and background loops.
- `handlers/`: one file per API (`backup_handler.rs`...), guards in `authz.rs`.
- `BackupService` (`services/backup/mod.rs`, whose doc maps the submodules);
  `RestoreService` (`services/restore.rs`, plus `restore_cancel`, `restore_reconcile`).
- A backup: `Job::BackupRequested` -> `BackupJobProcessor` -> `BackupExecutor::spawn`
  -> `BackupEngine::run`. `create_pre_upgrade_backup` skips this and runs
  `backup_external_service`. A new engine starts at `engines/mod.rs`.
- `execution::BackupError` (`is_permanent` picks the retry) and
  `services::BackupError` (maps to `Problem`) are different enums.

## Invariants

- **One writer process per database**: `reconcile_orphans_on_startup` fails every
  live row at boot, so a second process fails the first one's backups
  (`reconcile_orphans_on_startup_fails_every_live_backup`).
- **`BackupEngine::run` can be dropped at any await**: cancel and timeout drop it,
  and cancel skips `cleanup`. Cleanup not in a Drop guard (`RetainedContainer`,
  `AttemptGuard`) leaks the container or job
  (`dropping_an_attempt_after_its_job_started_stops_the_job_then_removes_the_object`).
- **Retry attempts never overlap**, or two write one object: no shared paths
  (`two_attempts_of_one_backup_never_share_names_or_paths`), and an unstoppable job
  fails the retry (`a_job_that_cannot_be_stopped_blocks_the_retry`).
- **Queue events are only hints**: `publish_recovery_set_if_complete` re-reads every
  sibling, or a partial run is offered as a whole-instance snapshot
  (`recovery_set_refuses_partial_fan_out_run`).
- **Stored locations are untrusted**: the credential can usually delete the whole
  bucket, so `validated_snapshot_prefix` allows one UUID directory under the root
  (`snapshot_prefix_validation_requires_configured_root_and_exact_backup_id`).
- **A stale lifecycle reconcile cannot clear a newer failure**, which would drop
  the source from the sweep
  (`a_stale_reconcile_attempt_cannot_clobber_a_newer_attempts_failure`).
- **Restore admission holds row locks** shared with `delete_backup_model`; PITR in
  place has only the lock, as `idx_restore_runs_source_service_active` covers
  `in_place` (`interrupted_restore_releases_the_service_only_after_fencing`).
- **The restore row decides cancellation**: both writes are conditional updates;
  `RestoreCancellations` only wakes (`guarded_phase_move_loses_to_a_recorded_cancellation`).
- **An orphaned restore stays active until fenced**, or a second restore races a
  surviving helper (`fence_failure_keeps_the_run_active`).

## Do not

- Put credentials in container commands or job specs
  (`dump_shell_contains_no_credentials`), or error text in telemetry
  (`failed_run_reports_code_not_message`).
- Build a destination S3 client without `s3_config_builder`, which bundles roots
  and adds `Content-MD5`
  (`backup_uploads_succeed_on_a_bucket_with_default_object_lock_retention`).
- Publish `BackupRequested` before commit (see `enqueue_scheduled_run`).
- Skip `handlers/authz.rs` guards on a handler keyed by a service, schedule or
  backup id (`deployment_token_denied_for_unlinked_service`).
- Leave a Docker engine out of `DOCKER_BACKED_ENGINES`; it would fail as unknown.

## Testing

`cargo test --lib -p temps-backup <name>`; `cargo test -p temps-backup --features
docker-tests --test workload_job_proof` (or `mariadb_pitr_e2e`).

- `docker-tests` gates only those binaries and one lib test,
  `resolve_backup_location_from_s3_finds_mariadb_physical_and_logical_backups`.
  Those and the `TestDatabase`/RustFS lib tests skip without Docker or a database.
- CI: `docker-deployments` runs every ungated test. `docker-backups` selects
  `workload_job_proof` and that lib test by name; `MariaDB PITR E2E` selects
  `test(mariadb_pitr_full_chain_e2e)`, so a new gated test must match a filter.
