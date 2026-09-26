// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Before `is_managed` existed, Temps' automatic environment monitor
        // used the deterministic "{environment} Monitor" name. A previous
        // migration left those rows unmanaged and startup reconciliation then
        // created a replacement, splitting uptime history across duplicate
        // rows. Keep the oldest legacy row as the canonical managed monitor,
        // move references from every duplicate automatic row to it, and remove
        // only duplicate rows with the reserved environment-monitor name or
        // explicit managed provenance. Custom-named monitors remain untouched.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_monitors \
                     ADD COLUMN check_path_revision BIGINT NOT NULL DEFAULT 0; \
                 LOCK TABLE status_monitors, status_checks, status_incidents \
                     IN SHARE ROW EXCLUSIVE MODE; \
                 CREATE TEMP TABLE _temps_monitor_canonical ON COMMIT DROP AS \
                 SELECT DISTINCT ON (monitor.environment_id) \
                        monitor.environment_id, monitor.id AS canonical_id \
                 FROM status_monitors AS monitor \
                 JOIN environments AS environment \
                   ON environment.id = monitor.environment_id \
                  AND environment.project_id = monitor.project_id \
                 WHERE monitor.environment_id IS NOT NULL \
                   AND monitor.name = environment.name || ' Monitor' \
                 ORDER BY monitor.environment_id, monitor.id; \
                 CREATE TEMP TABLE _temps_monitor_duplicates ON COMMIT DROP AS \
                 SELECT canonical.environment_id, canonical.canonical_id, \
                        monitor.id AS duplicate_id, monitor.is_managed AS was_managed \
                 FROM _temps_monitor_canonical AS canonical \
                 JOIN status_monitors AS monitor \
                   ON monitor.environment_id = canonical.environment_id \
                 JOIN environments AS environment \
                   ON environment.id = canonical.environment_id \
                  AND environment.project_id = monitor.project_id \
                 WHERE monitor.id <> canonical.canonical_id \
                   AND (monitor.is_managed = TRUE \
                        OR monitor.name = environment.name || ' Monitor'); \
                 CREATE TABLE _temps_m20260908_monitor_canonical_backup AS \
                 SELECT monitor.id, monitor.is_managed, monitor.check_path, \
                        monitor.check_path_revision, \
                        monitor.check_path AS reconciled_check_path, \
                        monitor.check_path_revision AS reconciled_check_path_revision \
                 FROM status_monitors AS monitor \
                 JOIN _temps_monitor_canonical AS canonical \
                   ON canonical.canonical_id = monitor.id; \
                 ALTER TABLE _temps_m20260908_monitor_canonical_backup \
                     ADD PRIMARY KEY (id); \
                 CREATE TABLE _temps_m20260908_monitor_duplicate_backup AS \
                 SELECT monitor.*, mapping.canonical_id \
                 FROM status_monitors AS monitor \
                 JOIN _temps_monitor_duplicates AS mapping \
                   ON mapping.duplicate_id = monitor.id; \
                 ALTER TABLE _temps_m20260908_monitor_duplicate_backup \
                     ADD PRIMARY KEY (id); \
                 CREATE TABLE _temps_m20260908_status_check_backup AS \
                 SELECT status_check.id, status_check.checked_at, \
                        status_check.monitor_id \
                 FROM status_checks AS status_check \
                 JOIN _temps_monitor_duplicates AS mapping \
                   ON mapping.duplicate_id = status_check.monitor_id; \
                 ALTER TABLE _temps_m20260908_status_check_backup \
                     ADD PRIMARY KEY (id, checked_at); \
                 CREATE TABLE _temps_m20260908_status_incident_backup AS \
                 SELECT incident.id, incident.monitor_id \
                 FROM status_incidents AS incident \
                 JOIN _temps_monitor_duplicates AS mapping \
                   ON mapping.duplicate_id = incident.monitor_id; \
                 ALTER TABLE _temps_m20260908_status_incident_backup \
                     ADD PRIMARY KEY (id); \
                 UPDATE status_monitors AS duplicate \
                 SET is_managed = FALSE \
                 FROM _temps_monitor_duplicates AS mapping \
                 WHERE duplicate.id = mapping.duplicate_id; \
                 UPDATE status_monitors AS canonical \
                 SET is_managed = TRUE, \
                     check_path = CASE \
                         WHEN EXISTS ( \
                             SELECT 1 \
                             FROM _temps_monitor_duplicates AS mapping \
                             WHERE mapping.canonical_id = canonical.id \
                         ) THEN ( \
                             SELECT duplicate.check_path \
                             FROM _temps_monitor_duplicates AS mapping \
                             JOIN status_monitors AS duplicate \
                               ON duplicate.id = mapping.duplicate_id \
                             WHERE mapping.canonical_id = canonical.id \
                             ORDER BY mapping.was_managed DESC, \
                                      duplicate.updated_at DESC, \
                                      duplicate.id DESC \
                             LIMIT 1 \
                         ) \
                         ELSE canonical.check_path \
                     END, \
                     check_path_revision = GREATEST( \
                         canonical.check_path_revision, \
                         COALESCE(( \
                             SELECT MAX(duplicate.check_path_revision) \
                             FROM _temps_monitor_duplicates AS mapping \
                             JOIN status_monitors AS duplicate \
                               ON duplicate.id = mapping.duplicate_id \
                             WHERE mapping.canonical_id = canonical.id \
                         ), canonical.check_path_revision) \
                     ) + 1 \
                 FROM _temps_monitor_canonical AS selected \
                 WHERE canonical.id = selected.canonical_id; \
                 UPDATE _temps_m20260908_monitor_canonical_backup AS backup \
                 SET reconciled_check_path = canonical.check_path, \
                     reconciled_check_path_revision = canonical.check_path_revision \
                 FROM status_monitors AS canonical \
                 WHERE canonical.id = backup.id; \
                 UPDATE status_checks AS status_check \
                 SET monitor_id = mapping.canonical_id \
                 FROM _temps_monitor_duplicates AS mapping \
                 WHERE status_check.monitor_id = mapping.duplicate_id; \
                 UPDATE status_incidents AS incident \
                 SET monitor_id = mapping.canonical_id \
                 FROM _temps_monitor_duplicates AS mapping \
                 WHERE incident.monitor_id = mapping.duplicate_id; \
                 DELETE FROM status_monitors AS duplicate \
                 USING _temps_monitor_duplicates AS mapping \
                 WHERE duplicate.id = mapping.duplicate_id; \
                 DROP TABLE IF EXISTS \
                     _temps_m20260904_managed_monitor_ownership_backup",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Restore only the rows and associations captured by up(). Checks or
        // incidents created on the canonical monitor after the migration stay
        // there; they did not belong to a duplicate in the pre-migration state.
        manager
            .get_connection()
            .execute_unprepared(
                "LOCK TABLE status_monitors, status_checks, status_incidents \
                     IN SHARE ROW EXCLUSIVE MODE; \
                 UPDATE status_monitors AS canonical \
                 SET is_managed = FALSE \
                 FROM _temps_m20260908_monitor_canonical_backup AS backup \
                 WHERE canonical.id = backup.id; \
                 INSERT INTO status_monitors \
                     (id, project_id, environment_id, name, monitor_type, check_path, \
                      check_path_revision, check_interval_seconds, is_active, is_managed, \
                      created_at, updated_at) \
                 SELECT id, project_id, environment_id, name, monitor_type, check_path, \
                        check_path_revision, check_interval_seconds, is_active, is_managed, \
                        created_at, updated_at \
                 FROM _temps_m20260908_monitor_duplicate_backup; \
                 WITH changed_canonical AS ( \
                     SELECT canonical.id, canonical.check_path, \
                            canonical.check_path_revision, canonical.updated_at \
                     FROM status_monitors AS canonical \
                     JOIN _temps_m20260908_monitor_canonical_backup AS backup \
                       ON backup.id = canonical.id \
                     WHERE canonical.check_path_revision <> \
                           backup.reconciled_check_path_revision \
                 ), handoff AS ( \
                     SELECT DISTINCT ON (duplicate.canonical_id) \
                            duplicate.id, changed.check_path, \
                            changed.check_path_revision, changed.updated_at \
                     FROM _temps_m20260908_monitor_duplicate_backup AS duplicate \
                     JOIN changed_canonical AS changed \
                       ON changed.id = duplicate.canonical_id \
                     ORDER BY duplicate.canonical_id, duplicate.is_managed DESC, \
                              duplicate.updated_at DESC, duplicate.id DESC \
                 ) \
                 UPDATE status_monitors AS restored \
                 SET check_path = handoff.check_path, \
                     check_path_revision = handoff.check_path_revision, \
                     updated_at = handoff.updated_at \
                 FROM handoff \
                 WHERE restored.id = handoff.id; \
                 UPDATE status_checks AS status_check \
                 SET monitor_id = backup.monitor_id \
                 FROM _temps_m20260908_status_check_backup AS backup \
                 WHERE status_check.id = backup.id \
                   AND status_check.checked_at = backup.checked_at; \
                 UPDATE status_incidents AS incident \
                 SET monitor_id = backup.monitor_id \
                 FROM _temps_m20260908_status_incident_backup AS backup \
                 WHERE incident.id = backup.id; \
                 UPDATE status_monitors AS canonical \
                 SET is_managed = backup.is_managed, \
                     check_path = CASE \
                         WHEN canonical.check_path_revision = \
                              backup.reconciled_check_path_revision \
                         THEN backup.check_path \
                         ELSE canonical.check_path \
                     END, \
                     check_path_revision = CASE \
                         WHEN canonical.check_path_revision = \
                              backup.reconciled_check_path_revision \
                         THEN backup.check_path_revision \
                         ELSE canonical.check_path_revision \
                     END \
                 FROM _temps_m20260908_monitor_canonical_backup AS backup \
                 WHERE canonical.id = backup.id; \
                 DROP TABLE _temps_m20260908_status_incident_backup; \
                 DROP TABLE _temps_m20260908_status_check_backup; \
                 DROP TABLE _temps_m20260908_monitor_duplicate_backup; \
                 DROP TABLE _temps_m20260908_monitor_canonical_backup; \
                 ALTER TABLE status_monitors DROP COLUMN check_path_revision",
            )
            .await?;

        Ok(())
    }
}
