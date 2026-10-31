// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backup schedules, CRUD, service membership, default schedules and
//! validation.

use super::*;

fn reject_source_backing_targets(
    backing_service_ids: &HashSet<i32>,
    target_service_ids: impl IntoIterator<Item = i32>,
) -> Result<(), BackupError> {
    if let Some(service_id) = target_service_ids
        .into_iter()
        .find(|service_id| backing_service_ids.contains(service_id))
    {
        return Err(BackupError::Validation(format!(
            "Service {} supplies this schedule's backup destination and cannot back up into itself",
            service_id
        )));
    }
    Ok(())
}

pub(super) fn exclude_source_backing_services(
    services: Vec<temps_entities::external_services::Model>,
    backing_service_ids: &HashSet<i32>,
) -> Vec<temps_entities::external_services::Model> {
    services
        .into_iter()
        .filter(|service| !backing_service_ids.contains(&service.id))
        .collect()
}

/// Refuse an instance-wide schedule -- one that targets every external
/// service or includes the control plane -- to a caller that does not run the
/// instance (NV1). Such a schedule dumps Temps's own database, so holding
/// `backups:*` (and `settings:write`) is not enough: a `Role::Custom` user or
/// API key with those grants must still be refused. Project-scoped schedules
/// (specific services, no control plane) are unaffected.
fn ensure_schedule_authority(
    authority: BackupScheduleAuthority,
    schedule: &str,
    target_all_services: bool,
    include_control_plane: bool,
    operation: &str,
) -> Result<(), BackupError> {
    if authority.is_instance_admin() || (!target_all_services && !include_control_plane) {
        return Ok(());
    }
    let reach = match (target_all_services, include_control_plane) {
        (true, true) => "targets every external service and includes the control plane",
        (true, false) => "targets every external service",
        (false, _) => "includes the control plane",
    };
    Err(BackupError::Forbidden {
        resource: schedule.to_string(),
        detail: format!(
            "Only an instance administrator may {operation} a backup schedule that {reach}"
        ),
    })
}

impl BackupService {
    /// Normalize and validate an explicit schedule target list before it is
    /// written. Returning the de-duplicated IDs keeps create, update, and the
    /// standalone attach endpoint on the same validation contract.
    async fn validated_schedule_service_ids(
        &self,
        s3_source_id: i32,
        service_ids: &[i32],
    ) -> Result<Vec<i32>, BackupError> {
        let mut unique_ids = service_ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();

        if unique_ids.is_empty() {
            return Ok(unique_ids);
        }

        let backing_service_ids = self.source_backing_service_ids(s3_source_id).await?;
        reject_source_backing_targets(&backing_service_ids, unique_ids.iter().copied())?;

        let found_count = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::Id.is_in(unique_ids.clone()))
            .count(self.db.as_ref())
            .await?;
        if found_count as usize != unique_ids.len() {
            return Err(BackupError::Validation(format!(
                "One or more service ids do not exist (requested {}, found {})",
                unique_ids.len(),
                found_count
            )));
        }

        Ok(unique_ids)
    }

    /// List all backup schedules
    pub async fn list_backup_schedules(
        &self,
    ) -> Result<Vec<temps_entities::backup_schedules::Model>, BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} backup schedules", schedules.len());
        Ok(schedules)
    }

    /// Create a new backup schedule
    ///
    /// `authority` is who asked: an instance-wide schedule (the defaults:
    /// every service plus the control plane) is refused to
    /// [`BackupScheduleAuthority::Scoped`] before anything is read or written.
    pub async fn create_backup_schedule(
        &self,
        request: CreateBackupScheduleRequest,
        authority: BackupScheduleAuthority,
    ) -> Result<BackupSchedule, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let target_all = request.target_all_services.unwrap_or(true);
        let include_control_plane = request.include_control_plane.unwrap_or(true);
        ensure_schedule_authority(
            authority,
            &format!("backup schedule '{}'", request.name),
            target_all,
            include_control_plane,
            "create",
        )?;

        validate_retention_period(request.retention_period)?;
        if target_all && !request.service_ids.is_empty() {
            return Err(BackupError::Validation(
                "service_ids cannot be set when target_all_services=true".to_string(),
            ));
        }
        if !target_all && !include_control_plane && request.service_ids.is_empty() {
            return Err(BackupError::Validation(
                "A schedule must include the control plane, at least one specific database, \
                 or all databases."
                    .to_string(),
            ));
        }

        // Resolve S3 source: explicit id OR fall back to the default source.
        let s3_source_id = self.resolve_s3_source_id(request.s3_source_id).await?;

        // Verify S3 source exists
        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        // Validate the schedule expression
        self.validate_backup_schedule(&request.schedule_expression)?;

        // Calculate next run time
        let cron_schedule = Schedule::from_str(&request.schedule_expression)
            .map_err(|e| BackupError::Schedule(e.to_string()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        let service_ids = self
            .validated_schedule_service_ids(s3_source_id, &request.service_ids)
            .await?;

        // Insert the schedule and its explicit memberships in one transaction.
        // A scheduler tick can therefore never observe an enabled specific-
        // target schedule before its databases have been attached.
        let txn = self.db.begin().await?;
        let now = chrono::Utc::now();
        let tags_json = serde_json::to_string(&request.tags)?;
        let new_schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(request.name.clone()),
            backup_type: Set(request.backup_type.clone()),
            retention_period: Set(request.retention_period),
            s3_source_id: Set(s3_source_id),
            schedule_expression: Set(request.schedule_expression.clone()),
            enabled: Set(request.enabled),
            created_at: Set(now),
            updated_at: Set(now),
            description: Set(request.description.clone()),
            tags: Set(tags_json),
            next_run: Set(next_run),
            max_runtime_secs: Set(request.max_runtime_secs),
            // Default is true ("back up every database, including future
            // ones") so a freshly-created schedule does the obvious thing
            // without the operator having to pick services up front.
            target_all_services: Set(target_all),
            include_control_plane: Set(include_control_plane),
            ..Default::default()
        };

        let schedule_model = new_schedule.insert(&txn).await?;
        if !service_ids.is_empty() {
            let memberships = service_ids.into_iter().map(|service_id| {
                temps_entities::backup_schedule_services::ActiveModel {
                    schedule_id: Set(schedule_model.id),
                    service_id: Set(service_id),
                    created_at: Set(now),
                }
            });
            temps_entities::backup_schedule_services::Entity::insert_many(memberships)
                .exec(&txn)
                .await?;
        }
        txn.commit().await?;
        info!("Created new backup schedule: {}", schedule_model.name);
        self.fire_lifecycle_reconcile(schedule_model.s3_source_id);
        Ok(schedule_model)
    }

    /// Get a backup schedule by ID
    pub async fn get_backup_schedule(&self, id: i32) -> Result<BackupSchedule, BackupError> {
        use sea_orm::EntityTrait;

        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        Ok(schedule)
    }

    /// Delete a backup schedule. An instance-wide schedule may only be
    /// deleted with [`BackupScheduleAuthority::InstanceAdmin`].
    pub async fn delete_backup_schedule(
        &self,
        id: i32,
        authority: BackupScheduleAuthority,
    ) -> Result<bool, BackupError> {
        use sea_orm::EntityTrait;

        // Ensure it exists to preserve previous behavior/logging
        let schedule = self.get_backup_schedule(id).await?;
        ensure_schedule_authority(
            authority,
            &format!("backup schedule {id}"),
            schedule.target_all_services,
            schedule.include_control_plane,
            "delete",
        )?;

        let result = temps_entities::backup_schedules::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        info!("Deleted backup schedule: {}", schedule.name);
        self.fire_lifecycle_reconcile(schedule.s3_source_id);
        Ok(result.rows_affected > 0)
    }

    /// Attach external services to a backup schedule.
    ///
    /// Idempotent: re-attaching an already-attached service is a no-op (rows
    /// are inserted with `ON CONFLICT DO NOTHING`). Returns the number of rows
    /// actually inserted. Validates that the schedule and every supplied
    /// service id exist.
    pub async fn attach_services_to_schedule(
        &self,
        schedule_id: i32,
        service_ids: &[i32],
    ) -> Result<u64, BackupError> {
        use sea_orm::ConnectionTrait;

        // Validate schedule exists (raises NotFound otherwise) and resolve the
        // destination identity before accepting explicit targets.
        let schedule = self.get_backup_schedule(schedule_id).await?;

        if service_ids.is_empty() {
            return Ok(0);
        }

        let unique_ids = self
            .validated_schedule_service_ids(schedule.s3_source_id, service_ids)
            .await?;

        // Build a single multi-row INSERT with ON CONFLICT DO NOTHING for
        // idempotency. Sea-ORM `insert_many` does not expose ON CONFLICT in
        // a portable way, so we drop to raw SQL.
        let mut sql = String::from(
            "INSERT INTO backup_schedule_services (schedule_id, service_id, created_at) VALUES ",
        );
        let mut params: Vec<sea_orm::Value> = Vec::with_capacity(unique_ids.len() * 2 + 1);
        params.push(sea_orm::Value::from(schedule_id));
        for (idx, sid) in unique_ids.iter().enumerate() {
            if idx > 0 {
                sql.push_str(", ");
            }
            let p = idx + 2; // $1 = schedule_id, $2.. = service_ids
            sql.push_str(&format!("($1, ${}, NOW())", p));
            params.push(sea_orm::Value::from(*sid));
        }
        sql.push_str(" ON CONFLICT (schedule_id, service_id) DO NOTHING");

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                params,
            ))
            .await
            .map_err(BackupError::Database)?;

        Ok(result.rows_affected())
    }

    /// Auto-provision a covering daily full-backup schedule for every MariaDB
    /// external service that does not yet have one.
    ///
    /// This drives point-in-time recovery out of the box: the daily full
    /// backup produces base backups via the `mariadb_physical` engine, and the
    /// binlog archiver already ships binary logs every few minutes, so
    /// base + binlogs = PITR with no operator action.
    ///
    /// Design (gated by the per-service `default_backup_provisioned` latch):
    /// - Only services where `default_backup_provisioned = false` are
    ///   considered, so we provision **exactly once** and never recreate a
    ///   schedule the operator later deletes.
    /// - Scope is **MariaDB only** (`service_type = "mariadb"`).
    /// - Requires a configured default S3 source. If none exists yet we log at
    ///   `debug` and return `Ok(())` — the next periodic tick retries, which
    ///   handles the "storage configured after the service" ordering.
    /// - Per-service failures are logged at `warn` and skipped, leaving the
    ///   latch `false` so the service is retried on the next tick. A single bad
    ///   service can't block the others.
    ///
    /// Idempotent and safe to call on a periodic tick.
    pub async fn reconcile_default_external_service_schedules(&self) -> Result<(), BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // 1. Resolve the destination for services not pinned anywhere yet.
        //    The Cloud-managed source wins when one exists: new services'
        //    continuous archiving defaults to it (see
        //    `temps_providers::continuous_archive`), so a base-backup
        //    schedule pointed anywhere else would leave the binlog shipper
        //    refusing to pin and PITR never starting. Otherwise the default
        //    source; if neither is configured yet, this is not an error — we
        //    simply have nothing to point a schedule at, so we bail quietly
        //    and retry on the next tick. A service that already carries a
        //    pin keeps it (see the loop below).
        let s3_source_id = match self.default_schedule_destination().await {
            Ok(id) => id,
            Err(_) => {
                debug!(
                    "reconcile_default_external_service_schedules: no default S3 source \
                     configured yet, skipping (will retry next tick)"
                );
                return Ok(());
            }
        };

        // 2. Load unprovisioned MariaDB services.
        let services = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::ServiceType.eq("mariadb"))
            .filter(temps_entities::external_services::Column::DefaultBackupProvisioned.eq(false))
            .all(self.db.as_ref())
            .await?;

        if services.is_empty() {
            return Ok(());
        }

        debug!(
            count = services.len(),
            s3_source_id,
            "reconcile_default_external_service_schedules: provisioning default backup \
             schedules for MariaDB services"
        );

        // 3. For each, create a daily full-backup schedule targeting exactly
        //    that service, then flip the latch.
        for service in services {
            // A service already pinned somewhere (an upgraded instance whose
            // binlogs ship to the operator's own bucket) keeps that
            // destination: a schedule pointed anywhere else would fail
            // every run on the pin mismatch.
            let destination = service
                .continuous_archive_s3_source_id
                .unwrap_or(s3_source_id);
            if let Err(e) = self
                .provision_default_schedule_for_service(&service, destination)
                .await
            {
                // Leave default_backup_provisioned = false so the next tick
                // retries. One failing service must not block the others.
                warn!(
                    service_id = service.id,
                    service_name = %service.name,
                    error = %e,
                    "Failed to auto-provision default backup schedule for MariaDB service; \
                     will retry on next reconcile tick"
                );
                continue;
            }
        }

        Ok(())
    }

    /// Create the default daily full-backup schedule for a single MariaDB
    /// service and mark it provisioned. Helper for
    /// [`reconcile_default_external_service_schedules`]; on success the
    /// service's `default_backup_provisioned` latch is set to `true`.
    /// Where an auto-provisioned schedule writes: the Cloud-managed source
    /// when the instance has one, else the default source.
    async fn default_schedule_destination(&self) -> Result<i32, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let managed = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::ManagedByCloud.eq(true))
            .one(self.db.as_ref())
            .await?;
        match managed {
            Some(source) => Ok(source.id),
            None => self.resolve_s3_source_id(None).await,
        }
    }

    async fn provision_default_schedule_for_service(
        &self,
        service: &temps_entities::external_services::Model,
        s3_source_id: i32,
    ) -> Result<(), BackupError> {
        use sea_orm::{ActiveModelTrait, Set};

        // Daily at 03:00 UTC. 6-field cron (`sec min hour dom mon dow`) as
        // required by the `cron` crate / `validate_backup_schedule`; the two
        // adjacent occurrences are 24h apart, satisfying the validator's
        // "at least 1 hour" rule. Reuse create_backup_schedule for validation
        // and next-run computation — do NOT hand-roll a second insert.
        let request = CreateBackupScheduleRequest {
            name: format!("Auto base backup — {}", service.name),
            // `backup_type` is the schedule/job label ("full"); the actual
            // backup engine (`mariadb_physical`) is resolved from the service's
            // `service_type` at run time, not from this field.
            backup_type: "full".to_string(),
            // Days. 14 days of base backups is a sane default retention window.
            retention_period: 14,
            // The destination resolved by the caller: the Cloud-managed
            // source when one exists, the default source otherwise.
            s3_source_id: Some(s3_source_id),
            schedule_expression: "0 0 3 * * *".to_string(),
            enabled: true,
            description: Some(
                "Automatically created daily base backup so point-in-time recovery \
                 works out of the box. Safe to edit or delete."
                    .to_string(),
            ),
            tags: vec![],
            max_runtime_secs: None,
            // Target exactly this service, not every DB or the control plane.
            // Schedule creation commits this membership atomically.
            target_all_services: Some(false),
            include_control_plane: Some(false),
            service_ids: vec![service.id],
        };

        // The generated schedule targets exactly one service and never the
        // control plane, so it is project-scoped whoever triggered it.
        let schedule = self
            .create_backup_schedule(request, BackupScheduleAuthority::Scoped)
            .await?;
        let mut generated_schedule: temps_entities::backup_schedules::ActiveModel =
            schedule.clone().into();
        generated_schedule.generated_kind = Set(Some("mariadb_base_backup".to_string()));
        generated_schedule.update(self.db.as_ref()).await?;

        // Flip the one-shot latch so we never provision this service again.
        let mut active: temps_entities::external_services::ActiveModel = service.clone().into();
        active.default_backup_provisioned = Set(true);
        active.update(self.db.as_ref()).await?;

        info!(
            service_id = service.id,
            service_name = %service.name,
            schedule_id = schedule.id,
            schedule_name = %schedule.name,
            "Auto-provisioned default daily base-backup schedule for MariaDB service \
             (enables point-in-time recovery; edit or delete it like any schedule)"
        );

        Ok(())
    }

    /// Detach a single external service from a backup schedule.
    ///
    /// Returns `true` if a row was removed, `false` if nothing was attached.
    /// Does not raise `NotFound` when the membership row is absent — callers
    /// can treat detach as idempotent.
    pub async fn detach_service_from_schedule(
        &self,
        schedule_id: i32,
        service_id: i32,
    ) -> Result<bool, BackupError> {
        use sea_orm::EntityTrait;

        let result = temps_entities::backup_schedule_services::Entity::delete_by_id((
            schedule_id,
            service_id,
        ))
        .exec(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        Ok(result.rows_affected > 0)
    }

    /// List the external services attached to a given schedule, ordered by
    /// service name for stable UI rendering. Raises `NotFound` if the
    /// schedule does not exist.
    pub async fn list_services_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<Vec<temps_entities::external_services::Model>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        self.get_backup_schedule(schedule_id).await?;

        let services = temps_entities::external_services::Entity::find()
            .inner_join(temps_entities::backup_schedule_services::Entity)
            .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule_id))
            .order_by_asc(temps_entities::external_services::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(services)
    }

    /// List the schedules that target a given external service. Raises
    /// `NotFound` if the service does not exist.
    pub async fn list_schedules_for_service(
        &self,
        service_id: i32,
    ) -> Result<Vec<BackupSchedule>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service {} not found", service_id),
            })?;

        let schedules = temps_entities::backup_schedules::Entity::find()
            .inner_join(temps_entities::backup_schedule_services::Entity)
            .filter(temps_entities::backup_schedule_services::Column::ServiceId.eq(service_id))
            .order_by_asc(temps_entities::backup_schedules::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(schedules)
    }

    /// List raw backup rows after the bounded history summary has been
    /// authorized by the handler.
    pub async fn list_backups_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<Vec<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        self.get_backup_schedule(schedule_id).await?;

        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleId.eq(schedule_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(backups)
    }

    // Add this new validation function
    fn validate_backup_schedule(&self, schedule: &str) -> Result<(), BackupError> {
        let schedule = Schedule::from_str(schedule)
            .map_err(|e| BackupError::Validation(format!("Invalid backup schedule: {}", e)))?;

        // Get the first two occurrences
        let upcoming = schedule.upcoming(Utc);
        let next_two = upcoming.take(2).collect::<Vec<_>>();
        if let [first, second] = next_two.as_slice() {
            let duration = *second - *first;
            if duration.num_minutes() < 60 {
                return Err(BackupError::Validation(
                    "Backup schedule must be at least 1 hour apart".into(),
                ));
            }
        }

        Ok(())
    }

    pub async fn update_next_run(&self, schedule_id: i32, schedule_str: &str) -> Result<()> {
        // Validate the schedule
        let schedule = Schedule::from_str(schedule_str)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;

        // Calculate next run time
        let next_run = schedule.upcoming(Utc).next();

        // Get the schedule and update it
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(schedule_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.next_run = sea_orm::Set(next_run);
        schedule_update.update(self.db.as_ref()).await?;

        info!(
            "Updated next run time for backup schedule {}: {:?}",
            schedule_id, next_run
        );
        Ok(())
    }

    /// Update an existing backup schedule with a partial set of changes.
    ///
    /// Only fields that are present (`Some`) in `request` are written to the
    /// database. Absent fields leave the column unchanged. Validation:
    ///
    /// - `name`: must be non-empty if present.
    /// - `schedule_expression`: validated by `validate_backup_schedule`; if
    ///   it differs from the stored value, `next_run` is recomputed.
    /// - `retention_period`: must be >= 1.
    /// - `max_runtime_secs`: `Some(Some(n))` requires `n >= 60`.
    ///
    /// An instance-wide schedule -- before or after the update -- may only be
    /// changed with [`BackupScheduleAuthority::InstanceAdmin`]: a scoped
    /// caller can neither edit one nor widen a project schedule into one.
    pub async fn update_backup_schedule(
        &self,
        id: i32,
        request: UpdateBackupScheduleRequest,
        authority: BackupScheduleAuthority,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel, Set};

        // 1. Load the existing schedule (returns NotFound if absent).
        let existing = self.get_backup_schedule(id).await?;
        ensure_schedule_authority(
            authority,
            &format!("backup schedule {id}"),
            existing.target_all_services || request.target_all_services == Some(true),
            existing.include_control_plane || request.include_control_plane == Some(true),
            "update",
        )?;
        let requested_target_all = request.target_all_services;
        let requested_service_ids = request.service_ids.clone();

        // 2. Validate fields before touching the ActiveModel.
        if let Some(ref name) = request.name {
            if name.is_empty() {
                return Err(BackupError::Validation("name cannot be empty".to_string()));
            }
        }

        if let Some(ref expr) = request.schedule_expression {
            self.validate_backup_schedule(expr)?;
        }

        if let Some(days) = request.retention_period {
            validate_retention_period(days)?;
        }

        if let Some(Some(secs)) = request.max_runtime_secs {
            if secs < 60 {
                return Err(BackupError::Validation(
                    "max_runtime_secs must be >= 60".to_string(),
                ));
            }
        }

        // 3. Build the ActiveModel from the loaded model.
        let mut active: temps_entities::backup_schedules::ActiveModel =
            existing.clone().into_active_model();

        let mut changed_fields: Vec<&str> = Vec::new();

        if let Some(name) = request.name {
            active.name = Set(name);
            changed_fields.push("name");
        }
        if let Some(description) = request.description {
            active.description = Set(if description.is_empty() {
                None
            } else {
                Some(description)
            });
            changed_fields.push("description");
        }
        if let Some(expr) = request.schedule_expression {
            if expr != existing.schedule_expression {
                let cron_schedule =
                    Schedule::from_str(&expr).map_err(|e| BackupError::Schedule(e.to_string()))?;
                let next_run = cron_schedule.upcoming(Utc).next();
                active.schedule_expression = Set(expr);
                active.next_run = Set(next_run);
                changed_fields.push("schedule_expression");
                changed_fields.push("next_run");
            }
        }
        if let Some(days) = request.retention_period {
            active.retention_period = Set(days);
            changed_fields.push("retention_period");
        }
        if let Some(runtime) = request.max_runtime_secs {
            active.max_runtime_secs = Set(runtime);
            changed_fields.push("max_runtime_secs");
        }
        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
            changed_fields.push("enabled");
        }
        if let Some(tags) = request.tags {
            let tags_json = serde_json::to_string(&tags)?;
            active.tags = Set(tags_json);
            changed_fields.push("tags");
        }
        if let Some(target_all) = request.target_all_services {
            active.target_all_services = Set(target_all);
            changed_fields.push("target_all_services");
        }
        if let Some(include_cp) = request.include_control_plane {
            active.include_control_plane = Set(include_cp);
            changed_fields.push("include_control_plane");
        }

        // Pre-flight: figure out what state the schedule would be in after
        // the update. If the operator is moving toward "nothing to back up,"
        // reject before we commit so the run history doesn't fill up with
        // no-op runs.
        let final_target_all = request
            .target_all_services
            .unwrap_or(existing.target_all_services);
        let final_include_cp = request
            .include_control_plane
            .unwrap_or(existing.include_control_plane);
        if final_target_all
            && requested_service_ids
                .as_ref()
                .is_some_and(|service_ids| !service_ids.is_empty())
        {
            return Err(BackupError::Validation(
                "service_ids cannot be set when target_all_services=true".to_string(),
            ));
        }

        let validated_service_ids = match requested_service_ids.as_deref() {
            Some(service_ids) => Some(
                self.validated_schedule_service_ids(existing.s3_source_id, service_ids)
                    .await?,
            ),
            None => None,
        };

        let txn = self.db.begin().await?;
        let final_service_count = if final_target_all {
            0
        } else if let Some(service_ids) = validated_service_ids.as_ref() {
            service_ids.len() as u64
        } else {
            temps_entities::backup_schedule_services::Entity::find()
                .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(id))
                .count(&txn)
                .await?
        };
        if !final_target_all && !final_include_cp && final_service_count == 0 {
            return Err(BackupError::Validation(
                "Schedule would have nothing to back up: select at least one specific database, \
                 enable the control plane, or target all databases."
                    .to_string(),
            ));
        }

        active.updated_at = Set(Utc::now());

        let updated = active.update(&txn).await?;

        // Retention drives `run_retention_cleanup`, which deletes a
        // schedule's backups once `started_at` is older than the retention
        // period. Changing the period therefore moves the deadline of every
        // existing backup of this schedule, so recompute the stored
        // `expires_at` in the same transaction — otherwise the console would
        // keep showing the deadline that was correct under the old period.
        if let Some(days) = request.retention_period {
            if days != existing.retention_period {
                // Clamp before the raw SQL addition: unlike `retention_expiry`,
                // `timestamp + interval` in Postgres raises an error (aborting
                // the whole transaction) instead of saturating on overflow.
                let clamped_days = i64::from(days).min(MAX_RETENTION_DAYS_FOR_SQL_ARITHMETIC);
                let recomputed = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "UPDATE backups SET expires_at = started_at + ($1 * interval '1 day') \
                         WHERE schedule_id = $2",
                        vec![Value::from(clamped_days), Value::from(id)],
                    ))
                    .await?;
                info!(
                    schedule_id = id,
                    retention_period_days = days,
                    rows_updated = recomputed.rows_affected(),
                    "Recomputed backup retention deadlines after a retention_period change",
                );
            }
        }

        // Apply the target-mode and explicit selection in the same transaction
        // as the schedule fields. This prevents both no-target scheduler races
        // and partial UI saves when switching from all to specific databases.
        if matches!(requested_target_all, Some(true)) || validated_service_ids.is_some() {
            let deleted = temps_entities::backup_schedule_services::Entity::delete_many()
                .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(id))
                .exec(&txn)
                .await?;
            info!(
                schedule_id = id,
                rows_deleted = deleted.rows_affected,
                "Cleared explicit service memberships before applying schedule targets",
            );
        }
        if !final_target_all {
            if let Some(service_ids) = validated_service_ids {
                if !service_ids.is_empty() {
                    let now = Utc::now();
                    let memberships = service_ids.into_iter().map(|service_id| {
                        temps_entities::backup_schedule_services::ActiveModel {
                            schedule_id: Set(id),
                            service_id: Set(service_id),
                            created_at: Set(now),
                        }
                    });
                    temps_entities::backup_schedule_services::Entity::insert_many(memberships)
                        .exec(&txn)
                        .await?;
                }
            }
        }
        txn.commit().await?;

        info!(
            schedule_id = id,
            fields = ?changed_fields,
            "Updated backup schedule fields",
        );

        // If retention or enabled flipped, the desired S3 lifecycle config
        // changed. Reconcile in the background. (Schedule can't be moved to
        // a different s3_source via UpdateBackupScheduleRequest today, so
        // we only reconcile one bucket.)
        if changed_fields.contains(&"retention_period") || changed_fields.contains(&"enabled") {
            self.fire_lifecycle_reconcile(updated.s3_source_id);
        }

        Ok(updated)
    }

    // Add this new method
    pub async fn disable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let s3_source_id = schedule_model.s3_source_id;
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.enabled = sea_orm::Set(false);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.update(self.db.as_ref()).await?;

        // Disabling may have been this source's last enabled schedule —
        // reconcile now so a stale S3-side lifecycle rule doesn't keep
        // expiring objects after the schedule stops running.
        self.fire_lifecycle_reconcile(s3_source_id);

        self.get_backup_schedule(id).await
    }

    // Add this new method
    pub async fn enable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        // Get the schedule to validate it exists and get the schedule expression
        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        // Calculate next run time based on the schedule expression
        let cron_schedule = Schedule::from_str(&schedule.schedule_expression)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // Update the schedule
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule.into_active_model();
        schedule_update.enabled = sea_orm::Set(true);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.next_run = sea_orm::Set(next_run);

        let updated_schedule = schedule_update.update(self.db.as_ref()).await?;

        // Re-enabling puts this source back in scope for lifecycle
        // reconciliation — push the rule immediately rather than waiting
        // for the hourly sweep to notice.
        self.fire_lifecycle_reconcile(updated_schedule.s3_source_id);

        Ok(updated_schedule)
    }
}

#[cfg(test)]
mod tests;
