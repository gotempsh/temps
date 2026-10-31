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

        // First update all schedules that don't have next_run set
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::NextRun.is_null())
            .all(self.db.as_ref())
            .await?;
        debug!("Updating next_run for {} schedules", schedules.len());
        for schedule in schedules {
            let cron_schedule = Schedule::from_str(&schedule.schedule_expression).map_err(|e| {
                BackupError::Validation(format!(
                    "Error parsing schedule expression for schedule {}: {}",
                    schedule.id, e
                ))
            })?;
            if let Some(next_run) = cron_schedule.upcoming(Utc).next() {
                let schedule_id = schedule.id;
                let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
                    schedule.into_active_model();
                schedule_update.next_run = sea_orm::Set(Some(next_run));
                schedule_update.update(self.db.as_ref()).await?;
                info!(
                    "Updated next_run for schedule {}: {}",
                    schedule_id, next_run
                );
            }
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
