// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The background loop that runs due backup schedules and enforces retention.

use super::*;

impl BackupService {
    /// Start the backup scheduler with graceful cancellation support.
    ///
    /// This method runs an infinite loop that:
    /// 1. Initializes schedules that don't have `next_run` set.
    /// 2. Fires once per hour to enqueue any schedules whose `next_run` has
    ///    elapsed. Enqueueing is fast (milliseconds) because the runner picks
    ///    up and executes the jobs asynchronously — the scheduler never `.await`s
    ///    backup execution.
    /// 3. Enforces retention after enqueueing.
    /// 4. Can be gracefully cancelled via the provided `CancellationToken`.
    ///
    pub async fn start_backup_scheduler(
        &self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), BackupError> {
        debug!("Starting backup scheduler");

        // This task is spawned once and nothing restarts it, so a failure here
        // must not end scheduling for every other schedule.
        if let Err(e) = self.initialize_missing_next_runs().await {
            error!(
                error = %e,
                "Could not list backup schedules without a next run; they stay unscheduled until the next restart",
            );
        }

        loop {
            let now = Utc::now();

            // Only run at the start of each hour
            if now.minute() != 0 {
                // Sleep until next hour or cancellation
                let next_hour = (now + chrono::Duration::hours(1))
                    .with_minute(0)
                    .and_then(|t| t.with_second(0))
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(now + chrono::Duration::hours(1));
                let sleep_duration = next_hour - now;

                tokio::select! {
                    _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {
                        continue;
                    }
                    _ = cancellation_token.cancelled() => {
                        info!("Backup scheduler received cancellation signal");
                        return Ok(());
                    }
                }
            }

            // Process scheduled backups with cancellation check
            tokio::select! {
                result = self.process_scheduled_backups(now) => {
                    if let Err(e) = result {
                        error!("Error processing scheduled backups: {}", e);
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }

            // Enforce retention: delete backups older than the schedule's retention period
            tokio::select! {
                result = self.enforce_retention(None, None) => {
                    if let Err(e) = result {
                        error!("Error enforcing backup retention: {}", e);
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal during retention cleanup");
                    return Ok(());
                }
            }

            // Sleep until next hour or cancellation
            let next_hour = (now + chrono::Duration::hours(1))
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now + chrono::Duration::hours(1));
            let sleep_duration = next_hour - now;

            tokio::select! {
                _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {}
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }
        }
    }

    /// Give every schedule without a `next_run` its next occurrence.
    ///
    /// A schedule that cannot be initialised (an expression the cron parser
    /// rejects, a failed write) is logged with its ID and skipped: it never
    /// becomes due, and the others keep running.
    async fn initialize_missing_next_runs(&self) -> Result<(), BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::NextRun.is_null())
            .all(self.db.as_ref())
            .await?;
        debug!("Updating next_run for {} schedules", schedules.len());

        for schedule in schedules {
            let schedule_id = schedule.id;
            let cron_schedule = match Schedule::from_str(&schedule.schedule_expression) {
                Ok(cron_schedule) => cron_schedule,
                Err(e) => {
                    error!(
                        schedule_id,
                        schedule_name = %schedule.name,
                        schedule_expression = %schedule.schedule_expression,
                        error = %e,
                        "Skipping backup schedule with an unparseable cron expression; it will not run until the expression is fixed",
                    );
                    continue;
                }
            };
            let Some(next_run) = cron_schedule.upcoming(Utc).next() else {
                warn!(
                    schedule_id,
                    schedule_name = %schedule.name,
                    schedule_expression = %schedule.schedule_expression,
                    "Backup schedule has no upcoming occurrence; it will not run",
                );
                continue;
            };

            let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
                schedule.into_active_model();
            schedule_update.next_run = sea_orm::Set(Some(next_run));
            match schedule_update.update(self.db.as_ref()).await {
                Ok(_) => info!(
                    "Updated next_run for schedule {}: {}",
                    schedule_id, next_run
                ),
                Err(e) => error!(
                    schedule_id,
                    error = %e,
                    "Could not store next_run for backup schedule; continuing with the others",
                ),
            }
        }

        Ok(())
    }

    /// Iterate over all enabled schedules whose `next_run` has elapsed and
    /// fan-out a `schedule_runs` row + backup jobs for each one.
    ///
    /// Each call to `enqueue_scheduled_run` is transactional and completes in
    /// milliseconds — the runner picks up and executes each job asynchronously.
    /// Sequential iteration is fast because we never `.await` backup execution
    /// inside this method.
    async fn process_scheduled_backups(&self, now: DateTime<Utc>) -> Result<(), BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        for schedule in schedules {
            // Skip if next_run hasn't elapsed yet (or if it's unset — the
            // init loop in start_backup_scheduler already populated it).
            let due = schedule.next_run.is_some_and(|t| t <= now);
            if !due {
                continue;
            }

            match self
                .enqueue_scheduled_run(&schedule, TriggerSource::Cron, None)
                .await
            {
                Ok(ScheduleRunOutcome::Started { run_id, ref jobs }) => {
                    info!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        run_id,
                        job_count = jobs.len(),
                        "scheduled run enqueued",
                    );
                }
                Ok(ScheduleRunOutcome::AlreadyInFlight { existing_run_id }) => {
                    info!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        existing_run_id,
                        "scheduled run skipped: previous run still in flight",
                    );
                }
                Err(e) => {
                    error!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        error = %e,
                        "scheduled run enqueue failed",
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};

    fn schedule_without_next_run(
        id: i32,
        expression: &str,
    ) -> temps_entities::backup_schedules::Model {
        temps_entities::backup_schedules::Model {
            schedule_expression: expression.to_string(),
            next_run: None,
            ..make_test_schedule(id, 1)
        }
    }

    /// The `id` bound by each `UPDATE backup_schedules` the mock saw.
    fn updated_schedule_ids(db: DatabaseConnection) -> Vec<Value> {
        db.into_transaction_log()
            .iter()
            .flat_map(|txn| txn.statements().iter())
            .filter(|statement| statement.sql.contains("UPDATE \"backup_schedules\""))
            .filter_map(|statement| {
                let values = statement.values.as_ref()?;
                assert!(statement.sql.contains("\"next_run\""), "{}", statement.sql,);
                values.0.last().cloned()
            })
            .collect()
    }

    /// One bad row must not cost the good row after it its next run.
    #[tokio::test]
    async fn an_unparseable_cron_does_not_stop_the_schedules_after_it() {
        let good = schedule_without_next_run(2, "0 0 2 * * *");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    schedule_without_next_run(1, "this is not cron"),
                    good.clone(),
                ]])
                // The RETURNING row of the one UPDATE that should happen.
                .append_query_results([vec![good]])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());

        service
            .initialize_missing_next_runs()
            .await
            .expect("a bad schedule is skipped, not fatal");

        drop(service);
        let db = Arc::try_unwrap(db).expect("service should release the mock database");
        assert_eq!(updated_schedule_ids(db), vec![Value::from(2)]);
    }

    /// A failed write for one schedule must not cost the next one either.
    #[tokio::test]
    async fn a_failed_next_run_write_does_not_stop_the_schedules_after_it() {
        let second = schedule_without_next_run(2, "0 0 2 * * *");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    schedule_without_next_run(1, "0 0 3 * * *"),
                    second.clone(),
                ]])
                .append_query_errors([sea_orm::DbErr::Custom("write unavailable".to_string())])
                .append_query_results([vec![second]])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());

        service
            .initialize_missing_next_runs()
            .await
            .expect("a failed write is logged, not fatal");

        drop(service);
        let db = Arc::try_unwrap(db).expect("service should release the mock database");
        assert_eq!(
            updated_schedule_ids(db),
            vec![Value::from(1), Value::from(2)],
        );
    }

    #[tokio::test]
    async fn listing_failure_is_reported_to_the_caller() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
                .into_connection(),
        );
        let service = make_scope_test_service(db);

        let error = service
            .initialize_missing_next_runs()
            .await
            .expect_err("the listing fails");

        assert!(matches!(error, BackupError::Database(_)), "{error:?}");
    }

    /// The scheduler itself keeps running past a bad row. It used to return
    /// `Err` here, ending scheduling for the life of the process. The token is
    /// already cancelled, so the loop exits at its first checkpoint.
    #[tokio::test]
    async fn the_scheduler_survives_an_unparseable_cron_at_startup() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![schedule_without_next_run(1, "this is not cron")]])
                .into_connection(),
        );
        let service = make_scope_test_service(db);
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            service.start_backup_scheduler(token),
        )
        .await
        .expect("a cancelled scheduler exits promptly");

        assert!(result.is_ok(), "{result:?}");
    }
}
