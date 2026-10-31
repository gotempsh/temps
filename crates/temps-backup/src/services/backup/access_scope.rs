// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped authorization, resolving which projects own a schedule, a
//! backup or a backup collection.

use super::*;

/// Project ownership resolved for one external service.
///
/// Resolution is intentionally service-layer owned so handlers never query
/// the `project_services` join table directly. An empty `project_ids` list is
/// ownerless and must fail closed whenever project-aware authorization is
/// enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProjectScope {
    pub service_id: i32,
    pub project_ids: Vec<i32>,
}

/// Why a schedule cannot be confined to a set of tenant projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalScheduleReason {
    TargetsAllServices,
    IncludesControlPlane,
    HasNoAttachedServices,
    HasOwnerlessService { service_id: i32 },
}

impl std::fmt::Display for GlobalScheduleReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetsAllServices => formatter.write_str("it targets all external services"),
            Self::IncludesControlPlane => formatter.write_str("it includes the control plane"),
            Self::HasNoAttachedServices => formatter.write_str("it has no attached services"),
            Self::HasOwnerlessService { service_id } => write!(
                formatter,
                "external service {service_id} is not linked to a project"
            ),
        }
    }
}

/// Authoritative tenant scope for a backup schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupScheduleAccessScope {
    Global {
        schedule_id: i32,
        reason: GlobalScheduleReason,
    },
    Projects {
        schedule_id: i32,
        project_ids: Vec<i32>,
    },
}

impl BackupScheduleAccessScope {
    pub fn schedule_id(&self) -> i32 {
        match self {
            Self::Global { schedule_id, .. } | Self::Projects { schedule_id, .. } => *schedule_id,
        }
    }
}

/// Authoritative tenant ownership for one backup row. Producer services are
/// immutable ownership-at-creation evidence. A row without a producer is
/// global: the current schedule configuration is mutable and therefore must
/// never be used to retroactively narrow historical backup ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupAccessScope {
    Services {
        backup_id: i32,
        service_ids: Vec<i32>,
    },
    Global {
        backup_id: i32,
    },
}

impl BackupAccessScope {
    pub fn backup_id(&self) -> i32 {
        match self {
            Self::Services { backup_id, .. } | Self::Global { backup_id } => *backup_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupWithAccessScope {
    pub backup: Backup,
    pub access_scope: BackupAccessScope,
}

/// Bounded authorization summary for a backup collection. The number of
/// service ids is bounded by configured services rather than backup history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCollectionAccessScope {
    pub contains_global: bool,
    pub service_ids: Vec<i32>,
}

#[derive(FromQueryResult)]
struct BackupCollectionGlobalRow {
    contains_global: bool,
}

#[derive(FromQueryResult)]
struct BackupCollectionServiceRow {
    service_id: i32,
}

impl BackupService {
    /// Resolve project ownership for many external services in one database
    /// query. The result contains one entry for every requested service id,
    /// including ids with no project link (their `project_ids` is empty).
    pub async fn project_scopes_for_services(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ServiceProjectScope>, BackupError> {
        if service_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut unique_ids = service_ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();

        let links = temps_entities::project_services::Entity::find()
            .filter(temps_entities::project_services::Column::ServiceId.is_in(unique_ids.clone()))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        let mut projects_by_service: BTreeMap<i32, BTreeSet<i32>> = unique_ids
            .into_iter()
            .map(|service_id| (service_id, BTreeSet::new()))
            .collect();
        for link in links {
            if let Some(project_ids) = projects_by_service.get_mut(&link.service_id) {
                project_ids.insert(link.project_id);
            }
        }

        Ok(projects_by_service
            .into_iter()
            .map(|(service_id, project_ids)| ServiceProjectScope {
                service_id,
                project_ids: project_ids.into_iter().collect(),
            })
            .collect())
    }

    /// Resolve the authoritative access scope for many backup schedules with
    /// a fixed number of batched queries. This avoids the former per-service
    /// project lookup in schedule list and membership handlers.
    pub async fn access_scopes_for_schedules(
        &self,
        schedule_ids: &[i32],
    ) -> Result<Vec<BackupScheduleAccessScope>, BackupError> {
        if schedule_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut unique_schedule_ids = schedule_ids.to_vec();
        unique_schedule_ids.sort_unstable();
        unique_schedule_ids.dedup();

        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Id.is_in(unique_schedule_ids.clone()))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        let memberships = temps_entities::backup_schedule_services::Entity::find()
            .filter(
                temps_entities::backup_schedule_services::Column::ScheduleId
                    .is_in(unique_schedule_ids),
            )
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        let mut services_by_schedule: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        let mut service_ids = Vec::with_capacity(memberships.len());
        for membership in memberships {
            services_by_schedule
                .entry(membership.schedule_id)
                .or_default()
                .push(membership.service_id);
            service_ids.push(membership.service_id);
        }

        let project_scopes = self.project_scopes_for_services(&service_ids).await?;
        let projects_by_service: BTreeMap<i32, Vec<i32>> = project_scopes
            .into_iter()
            .map(|scope| (scope.service_id, scope.project_ids))
            .collect();

        let mut scopes = Vec::with_capacity(schedules.len());
        for schedule in schedules {
            if schedule.target_all_services {
                scopes.push(BackupScheduleAccessScope::Global {
                    schedule_id: schedule.id,
                    reason: GlobalScheduleReason::TargetsAllServices,
                });
                continue;
            }
            if schedule.include_control_plane {
                scopes.push(BackupScheduleAccessScope::Global {
                    schedule_id: schedule.id,
                    reason: GlobalScheduleReason::IncludesControlPlane,
                });
                continue;
            }

            let Some(attached_service_ids) = services_by_schedule.get(&schedule.id) else {
                scopes.push(BackupScheduleAccessScope::Global {
                    schedule_id: schedule.id,
                    reason: GlobalScheduleReason::HasNoAttachedServices,
                });
                continue;
            };

            let mut project_ids = BTreeSet::new();
            let mut ownerless_service_id = None;
            for service_id in attached_service_ids {
                let service_project_ids = projects_by_service
                    .get(service_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if service_project_ids.is_empty() {
                    ownerless_service_id = Some(*service_id);
                    break;
                }
                project_ids.extend(service_project_ids.iter().copied());
            }

            if let Some(service_id) = ownerless_service_id {
                scopes.push(BackupScheduleAccessScope::Global {
                    schedule_id: schedule.id,
                    reason: GlobalScheduleReason::HasOwnerlessService { service_id },
                });
            } else if project_ids.is_empty() {
                scopes.push(BackupScheduleAccessScope::Global {
                    schedule_id: schedule.id,
                    reason: GlobalScheduleReason::HasNoAttachedServices,
                });
            } else {
                scopes.push(BackupScheduleAccessScope::Projects {
                    schedule_id: schedule.id,
                    project_ids: project_ids.into_iter().collect(),
                });
            }
        }

        scopes.sort_by_key(BackupScheduleAccessScope::schedule_id);
        Ok(scopes)
    }

    /// Resolve a single schedule scope, preserving a contextual not-found
    /// error when the caller supplies an unknown schedule id.
    pub async fn access_scope_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<BackupScheduleAccessScope, BackupError> {
        self.access_scopes_for_schedules(&[schedule_id])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: format!("Backup schedule {schedule_id} not found"),
            })
    }

    /// Resolve the owning schedule for a real or synthetic schedule-run id.
    /// Synthetic legacy run ids are the negated `backups.id` values emitted
    /// by `list_schedule_runs`.
    pub async fn schedule_id_for_run(&self, run_id: i64) -> Result<i32, BackupError> {
        if run_id >= 0 {
            let run = temps_entities::schedule_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?
                .ok_or_else(|| BackupError::NotFound {
                    resource: "ScheduleRun".to_string(),
                    detail: format!("Schedule run {run_id} not found"),
                })?;
            return Ok(run.schedule_id);
        }

        let backup_id_i64 = run_id.checked_neg().ok_or_else(|| {
            BackupError::Validation(format!("Schedule run id {run_id} cannot be resolved"))
        })?;
        let backup_id = i32::try_from(backup_id_i64).map_err(|_| {
            BackupError::Validation(format!(
                "Schedule run id {run_id} is outside the valid range"
            ))
        })?;
        let backup = temps_entities::backups::Entity::find_by_id(backup_id)
            .one(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ScheduleRun".to_string(),
                detail: format!("Synthetic schedule run {run_id} not found"),
            })?;
        backup.schedule_id.ok_or_else(|| BackupError::NotFound {
            resource: "ScheduleRun".to_string(),
            detail: format!("Backup {backup_id} is not linked to a schedule"),
        })
    }

    /// Resolve the immutable authorization summary for all historical backups
    /// of a schedule without materializing every backup id. The two bounded
    /// queries return one existence bit plus distinct producer service ids.
    pub async fn backup_history_access_scope_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<BackupCollectionAccessScope, BackupError> {
        self.get_backup_schedule(schedule_id).await?;

        let global_row =
            BackupCollectionGlobalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT EXISTS (
                    SELECT 1
                    FROM backups b
                    WHERE b.schedule_id = $1
                      AND NOT EXISTS (
                          SELECT 1 FROM external_service_backups esb
                          WHERE esb.backup_id = b.id
                      )
                ) AS contains_global"#,
                vec![Value::from(schedule_id)],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?
            .ok_or_else(|| BackupError::Internal {
                message: format!(
                    "Schedule {schedule_id} backup history scope query returned no result"
                ),
            })?;

        let service_rows =
            BackupCollectionServiceRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT DISTINCT esb.service_id
                    FROM backups b
                    JOIN external_service_backups esb ON esb.backup_id = b.id
                    WHERE b.schedule_id = $1
                    ORDER BY esb.service_id"#,
                vec![Value::from(schedule_id)],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(BackupCollectionAccessScope {
            contains_global: global_row.contains_global,
            service_ids: service_rows.into_iter().map(|row| row.service_id).collect(),
        })
    }

    /// Resolve immutable access scopes for every backup job in one scheduler
    /// run. This is used to authorize run drill-down before returning any
    /// aggregate or per-job metadata.
    pub async fn backups_with_access_scopes_for_schedule_run(
        &self,
        run_id: i64,
    ) -> Result<Vec<BackupWithAccessScope>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleRunId.eq(run_id))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        self.derive_backup_access_scopes(backups).await
    }

    /// Resolve immutable scopes for the exact live children that a run cancel
    /// can mutate. Terminal children are intentionally excluded because the
    /// cancellation helper does not update them.
    pub async fn live_backups_with_access_scopes_for_schedule_run(
        &self,
        run_id: i64,
    ) -> Result<Vec<BackupWithAccessScope>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleRunId.eq(run_id))
            .filter(
                temps_entities::backups::Column::State
                    .is_in(["pending".to_string(), "running".to_string()]),
            )
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        self.derive_backup_access_scopes(backups).await
    }

    /// Resolve immutable scopes for the exact UUID set supplied by a
    /// preview-bound destructive operation. Missing rows are left for the
    /// operation's stale-preview validation to reject.
    pub async fn backups_with_access_scopes_by_uuids(
        &self,
        backup_uuids: &[String],
    ) -> Result<Vec<BackupWithAccessScope>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        if backup_uuids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_backup_uuids = backup_uuids.to_vec();
        unique_backup_uuids.sort();
        unique_backup_uuids.dedup();
        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.is_in(unique_backup_uuids))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        self.derive_backup_access_scopes(backups).await
    }

    /// Resolve a bounded immutable scope summary for all backups currently
    /// selected by one schedule's retention policy. The authorization query
    /// scales with distinct producer services, not expired backup count.
    pub async fn retention_candidate_access_scope(
        &self,
        schedule_id: i32,
    ) -> Result<BackupCollectionAccessScope, BackupError> {
        let schedule = self.get_backup_schedule(schedule_id).await?;
        if schedule.retention_period <= 0 {
            return Ok(BackupCollectionAccessScope {
                contains_global: false,
                service_ids: Vec::new(),
            });
        }
        let cutoff = retention_cutoff(schedule.retention_period)?;

        let global_row =
            BackupCollectionGlobalRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT EXISTS (
                    SELECT 1
                    FROM backups b
                    WHERE b.schedule_id = $1
                      AND b.started_at < $2
                      AND NOT EXISTS (
                          SELECT 1 FROM external_service_backups esb
                          WHERE esb.backup_id = b.id
                      )
                ) AS contains_global"#,
                vec![Value::from(schedule_id), Value::from(cutoff)],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?
            .ok_or_else(|| BackupError::Internal {
                message: format!("Schedule {schedule_id} retention scope query returned no result"),
            })?;

        let service_rows =
            BackupCollectionServiceRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT DISTINCT esb.service_id
                    FROM backups b
                    JOIN external_service_backups esb ON esb.backup_id = b.id
                    WHERE b.schedule_id = $1
                      AND b.started_at < $2
                    ORDER BY esb.service_id"#,
                vec![Value::from(schedule_id), Value::from(cutoff)],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(BackupCollectionAccessScope {
            contains_global: global_row.contains_global,
            service_ids: service_rows.into_iter().map(|row| row.service_id).collect(),
        })
    }

    /// Resolve backup rows and their authoritative access scopes in a fixed
    /// number of batched queries. Producer-service ownership is the only
    /// project-confined ownership evidence; rows without a producer are
    /// global even when they reference a currently project-scoped schedule.
    pub async fn backups_with_access_scopes(
        &self,
        backup_ids: &[i32],
    ) -> Result<Vec<BackupWithAccessScope>, BackupError> {
        if backup_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_backup_ids = backup_ids.to_vec();
        unique_backup_ids.sort_unstable();
        unique_backup_ids.dedup();
        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::Id.is_in(unique_backup_ids))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;
        self.derive_backup_access_scopes(backups).await
    }

    pub async fn backup_with_access_scope_by_id(
        &self,
        backup_id: i32,
    ) -> Result<BackupWithAccessScope, BackupError> {
        self.backups_with_access_scopes(&[backup_id])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Backup row {backup_id} not found"),
            })
    }

    pub async fn backup_with_access_scope_by_uuid(
        &self,
        backup_uuid: &str,
    ) -> Result<BackupWithAccessScope, BackupError> {
        let backup = self
            .get_backup(backup_uuid)
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Backup {backup_uuid} not found"),
            })?;
        self.derive_backup_access_scopes(vec![backup])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| BackupError::Internal {
                message: format!(
                    "Backup {backup_uuid} disappeared while resolving its authorization scope"
                ),
            })
    }

    async fn derive_backup_access_scopes(
        &self,
        backups: Vec<Backup>,
    ) -> Result<Vec<BackupWithAccessScope>, BackupError> {
        if backups.is_empty() {
            return Ok(Vec::new());
        }

        let backup_ids: Vec<i32> = backups.iter().map(|backup| backup.id).collect();
        let producer_rows = temps_entities::external_service_backups::Entity::find()
            .filter(
                temps_entities::external_service_backups::Column::BackupId
                    .is_in(backup_ids.iter().copied()),
            )
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;
        let mut producers_by_backup: BTreeMap<i32, BTreeSet<i32>> = backup_ids
            .iter()
            .copied()
            .map(|backup_id| (backup_id, BTreeSet::new()))
            .collect();
        for producer in producer_rows {
            if let Some(service_ids) = producers_by_backup.get_mut(&producer.backup_id) {
                service_ids.insert(producer.service_id);
            }
        }

        Ok(backups
            .into_iter()
            .map(|backup| {
                let producer_service_ids: Vec<i32> = producers_by_backup
                    .remove(&backup.id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let access_scope = if !producer_service_ids.is_empty() {
                    BackupAccessScope::Services {
                        backup_id: backup.id,
                        service_ids: producer_service_ids,
                    }
                } else {
                    BackupAccessScope::Global {
                        backup_id: backup.id,
                    }
                };
                BackupWithAccessScope {
                    backup,
                    access_scope,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_schedule_membership(
        schedule_id: i32,
        service_id: i32,
    ) -> temps_entities::backup_schedule_services::Model {
        temps_entities::backup_schedule_services::Model {
            schedule_id,
            service_id,
            created_at: Utc::now(),
        }
    }

    fn make_project_service_link(
        id: i32,
        project_id: i32,
        service_id: i32,
    ) -> temps_entities::project_services::Model {
        temps_entities::project_services::Model {
            id,
            project_id,
            service_id,
            database_provisioning_mode: "project_environment".to_string(),
            custom_database_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_collection_global_row(
        contains_global: bool,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut row = std::collections::BTreeMap::new();
        row.insert(
            "contains_global".to_string(),
            sea_orm::Value::Bool(Some(contains_global)),
        );
        row
    }

    fn make_collection_service_row(
        service_id: i32,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut row = std::collections::BTreeMap::new();
        row.insert(
            "service_id".to_string(),
            sea_orm::Value::Int(Some(service_id)),
        );
        row
    }

    #[tokio::test]
    async fn test_access_scopes_for_schedules_global_reasons_are_classified() {
        // Arrange: exercise every condition that makes a schedule global.
        let mut targets_all = make_test_schedule(10, 1);
        targets_all.include_control_plane = false;

        let mut includes_control_plane = make_test_schedule(20, 1);
        includes_control_plane.target_all_services = false;

        let mut no_attachments = make_test_schedule(30, 1);
        no_attachments.target_all_services = false;
        no_attachments.include_control_plane = false;

        let mut ownerless = make_test_schedule(40, 1);
        ownerless.target_all_services = false;
        ownerless.include_control_plane = false;

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    ownerless,
                    no_attachments,
                    includes_control_plane,
                    targets_all,
                ]])
                .append_query_results(vec![vec![make_schedule_membership(40, 400)]])
                .append_query_results(vec![Vec::<temps_entities::project_services::Model>::new()])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scopes = service
            .access_scopes_for_schedules(&[40, 30, 20, 10])
            .await
            .expect("schedule scopes should resolve");

        // Assert: output is sorted and every global reason remains distinct.
        assert_eq!(
            scopes,
            vec![
                BackupScheduleAccessScope::Global {
                    schedule_id: 10,
                    reason: GlobalScheduleReason::TargetsAllServices,
                },
                BackupScheduleAccessScope::Global {
                    schedule_id: 20,
                    reason: GlobalScheduleReason::IncludesControlPlane,
                },
                BackupScheduleAccessScope::Global {
                    schedule_id: 30,
                    reason: GlobalScheduleReason::HasNoAttachedServices,
                },
                BackupScheduleAccessScope::Global {
                    schedule_id: 40,
                    reason: GlobalScheduleReason::HasOwnerlessService { service_id: 400 },
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_access_scopes_for_schedules_batch_dedupes_and_sorts_project_scope() {
        // Arrange: duplicate schedule ids, memberships and project links model
        // a batch list query without introducing an authorization N+1.
        let mut schedule = make_test_schedule(50, 1);
        schedule.target_all_services = false;
        schedule.include_control_plane = false;
        let memberships = vec![
            make_schedule_membership(50, 500),
            make_schedule_membership(50, 501),
        ];
        let links = vec![
            make_project_service_link(1, 9, 500),
            make_project_service_link(2, 3, 500),
            make_project_service_link(3, 3, 500),
            make_project_service_link(4, 7, 501),
            make_project_service_link(5, 3, 501),
        ];
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![schedule]])
                .append_query_results(vec![memberships])
                .append_query_results(vec![links])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());

        // Act.
        let scopes = service
            .access_scopes_for_schedules(&[50, 50])
            .await
            .expect("batched schedule scope should resolve");

        // Assert.
        assert_eq!(
            scopes,
            vec![BackupScheduleAccessScope::Projects {
                schedule_id: 50,
                project_ids: vec![3, 7, 9],
            }]
        );

        drop(service);
        let statements = Arc::try_unwrap(db)
            .expect("service dropped, leaving one database reference")
            .into_transaction_log();
        assert_eq!(
            statements.len(),
            3,
            "scope batching must use one schedules, one memberships and one project-links query"
        );
    }

    #[tokio::test]
    async fn test_project_scopes_for_services_ownerless_and_duplicates_are_preserved_safely() {
        // Arrange: service 600 is ownerless; service 601 has duplicate links.
        let links = vec![
            make_project_service_link(1, 8, 601),
            make_project_service_link(2, 3, 601),
            make_project_service_link(3, 8, 601),
        ];
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![links])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scopes = service
            .project_scopes_for_services(&[601, 600, 601])
            .await
            .expect("service scopes should resolve");

        // Assert.
        assert_eq!(
            scopes,
            vec![
                ServiceProjectScope {
                    service_id: 600,
                    project_ids: vec![],
                },
                ServiceProjectScope {
                    service_id: 601,
                    project_ids: vec![3, 8],
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_project_scopes_for_services_database_error_is_typed() {
        // Arrange.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors(vec![sea_orm::DbErr::Custom(
                    "scope lookup failed".to_string(),
                )])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let error = service
            .project_scopes_for_services(&[700])
            .await
            .expect_err("database failure must remain typed");

        // Assert.
        assert!(matches!(error, BackupError::Database(_)));
        assert!(error.to_string().contains("scope lookup failed"));
    }

    #[tokio::test]
    async fn test_backups_with_access_scopes_producer_precedes_global_fallback() {
        // Arrange.
        let mut producer_backup = make_test_backup_model(801);
        producer_backup.schedule_id = Some(10);
        let mut scheduled_backup = make_test_backup_model(802);
        scheduled_backup.schedule_id = Some(20);
        let global_backup = make_test_backup_model(803);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![global_backup, scheduled_backup, producer_backup]])
                .append_query_results(vec![vec![
                    make_external_service_backup(1, 801, 101),
                    make_external_service_backup(2, 801, 101),
                ]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scoped = service
            .backups_with_access_scopes(&[803, 801, 802, 801])
            .await
            .expect("backup scopes should resolve");
        let scopes: BTreeMap<i32, BackupAccessScope> = scoped
            .into_iter()
            .map(|entry| (entry.backup.id, entry.access_scope))
            .collect();

        // Assert: immutable producer ownership wins. A schedule id alone is
        // not ownership evidence because schedule configuration can change
        // after the backup is created, so both ownerless rows are global.
        assert_eq!(
            scopes.get(&801),
            Some(&BackupAccessScope::Services {
                backup_id: 801,
                service_ids: vec![101],
            })
        );
        assert_eq!(
            scopes.get(&802),
            Some(&BackupAccessScope::Global { backup_id: 802 })
        );
        assert_eq!(
            scopes.get(&803),
            Some(&BackupAccessScope::Global { backup_id: 803 })
        );
    }

    #[tokio::test]
    async fn test_schedule_history_ownerless_backup_stays_global_after_schedule_scope_change() {
        // Arrange: the schedule currently targets one attached project
        // service, but the historical backup has no immutable producer row,
        // matching a control-plane backup created before the schedule changed.
        let mut current_schedule = make_test_schedule(20, 1);
        current_schedule.target_all_services = false;
        current_schedule.include_control_plane = false;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![current_schedule]])
                .append_query_results(vec![vec![make_collection_global_row(true)]])
                .append_query_results(vec![vec![make_collection_service_row(202)]])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());

        // Act.
        let scope = service
            .backup_history_access_scope_for_schedule(20)
            .await
            .expect("historical backup scopes should resolve");

        // Assert: the global bit forces administrator-only access, while the
        // producer set remains bounded by distinct configured services.
        assert_eq!(
            scope,
            BackupCollectionAccessScope {
                contains_global: true,
                service_ids: vec![202],
            }
        );

        drop(service);
        let statements = Arc::try_unwrap(db)
            .expect("service dropped, leaving one database reference")
            .into_transaction_log();
        let sql = format!("{statements:?}");
        assert!(
            sql.contains("EXISTS"),
            "global ownership must use EXISTS: {sql}"
        );
        assert!(
            sql.contains("DISTINCT"),
            "producer ownership must select distinct services: {sql}"
        );
        assert!(
            !sql.contains("backup_id IN"),
            "history authorization must not build an unbounded backup-id IN list: {sql}"
        );
    }

    #[tokio::test]
    async fn test_live_run_cancel_scope_keeps_ownerless_control_plane_backup_global() {
        // Arrange: this live control-plane child belongs to a schedule run,
        // but has no producer row. A later schedule change to project-only
        // must not make the child cancellable by that project.
        let mut live_control_plane_backup = make_test_backup_model(804);
        live_control_plane_backup.schedule_id = Some(20);
        live_control_plane_backup.schedule_run_id = Some(55);
        live_control_plane_backup.state = "running".to_string();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![live_control_plane_backup]])
                .append_query_results(vec![
                    Vec::<temps_entities::external_service_backups::Model>::new(),
                ])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scoped = service
            .live_backups_with_access_scopes_for_schedule_run(55)
            .await
            .expect("live cancellation scopes should resolve");

        // Assert.
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].access_scope,
            BackupAccessScope::Global { backup_id: 804 }
        );
    }

    #[tokio::test]
    async fn test_retention_preview_scope_keeps_historical_control_plane_backup_global() {
        // Arrange: the schedule is project-scoped today, while the expired
        // historical control-plane backup has no immutable producer.
        let mut current_schedule = make_test_schedule(20, 1);
        current_schedule.target_all_services = false;
        current_schedule.include_control_plane = false;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![current_schedule]])
                .append_query_results(vec![vec![make_collection_global_row(true)]])
                .append_query_results(vec![Vec::<
                    std::collections::BTreeMap<String, sea_orm::Value>,
                >::new()])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scope = service
            .retention_candidate_access_scope(20)
            .await
            .expect("retention candidate scopes should resolve");

        // Assert: the dry-run handler will pass this global scope through the
        // collection guard before returning any candidate metadata.
        assert_eq!(
            scope,
            BackupCollectionAccessScope {
                contains_global: true,
                service_ids: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn test_preview_bound_deletion_scope_keeps_expected_ownerless_backup_global() {
        // Arrange: destructive cleanup re-resolves the exact preview UUIDs.
        // A schedule id alone must not turn an ownerless historical row into
        // project-owned data.
        let mut expected_control_plane_backup = make_test_backup_model(806);
        expected_control_plane_backup.schedule_id = Some(20);
        let expected_uuid = expected_control_plane_backup.backup_id.clone();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![expected_control_plane_backup]])
                .append_query_results(vec![
                    Vec::<temps_entities::external_service_backups::Model>::new(),
                ])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let scoped = service
            .backups_with_access_scopes_by_uuids(&[expected_uuid])
            .await
            .expect("preview-bound deletion scopes should resolve");

        // Assert: the destructive handler will require global admin before
        // handing this exact candidate set to enforce_retention.
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].access_scope,
            BackupAccessScope::Global { backup_id: 806 }
        );
    }

    #[tokio::test]
    async fn test_backups_with_access_scopes_producer_query_error_is_typed() {
        // Arrange.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_test_backup_model(900)]])
                .append_query_errors(vec![sea_orm::DbErr::Custom(
                    "producer lookup failed".to_string(),
                )])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        // Act.
        let error = service
            .backups_with_access_scopes(&[900])
            .await
            .expect_err("producer query error must be typed");

        // Assert.
        assert!(matches!(error, BackupError::Database(_)));
        assert!(error.to_string().contains("producer lookup failed"));
    }
}
