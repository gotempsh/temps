// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retention for analytics data and the identifiers attached to it.
//!
//! `events` and `proxy_logs` already expire through TimescaleDB retention
//! policies, but the rows they point at did not: `ip_geolocations` holds the
//! full client IP address and was never deleted, `visitor` rows were never
//! deleted, and `error_events` had no policy at all. This service enforces
//! the windows in [`AnalyticsRetentionSettings`]:
//!
//! - **`events` / `error_events`**: the TimescaleDB retention policy is kept
//!   in step with the setting (the same mechanism the observability tables
//!   use, so whole chunks are dropped rather than rows deleted).
//! - **`visitor`**: rows not seen for N days are deleted in bounded batches.
//!   Every foreign key to `visitor` is `SET NULL` or `CASCADE`.
//! - **`ip_geolocations`**: a row is deleted only when *nothing* references
//!   it, so the stored address lives exactly as long as the data that needs
//!   its location. An address referenced by an audit log is kept with it.
//!
//! The job is control-plane work: every pass is bounded (batch size and
//! batches per run), paced, and resumes where the previous run stopped, so a
//! large backlog is worked off over several runs instead of in one long
//! statement holding locks.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, FromQueryResult, Statement,
};
use temps_core::AnalyticsRetentionSettings;
use thiserror::Error;
use tracing::{debug, info, warn};

/// How often the job re-reads the settings and enforces them.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Delay before the first pass, so startup is not competing with a sweep.
pub const RETENTION_STARTUP_DELAY: Duration = Duration::from_secs(5 * 60);
/// Rows removed (or examined) by one statement.
const BATCH_SIZE: i64 = 500;
/// Statements per pass per table. With [`BATCH_SIZE`] this bounds one pass at
/// 100k rows; an operator with a larger backlog catches up over several runs.
const MAX_BATCHES_PER_RUN: usize = 200;
/// Pause between batches, so the sweep yields to ingest.
const BATCH_PAUSE: Duration = Duration::from_millis(200);

/// Tables whose retention is a TimescaleDB policy managed here.
const EVENTS_TABLE: &str = "events";
const ERROR_EVENTS_TABLE: &str = "error_events";

/// Every column that stores an `ip_geolocations.id`, as `(table, column)`.
///
/// Only some of these carry a foreign key (`events`, `error_events`,
/// `proxy_logs`); the rest are plain integers, so the database cannot be
/// relied on to refuse deleting a row that is still in use -- the cleanup
/// itself has to look. A column that does not exist in this installation is
/// skipped (see [`existing_reference_columns`]).
const IP_REFERENCES: &[(&str, &str)] = &[
    ("events", "ip_geolocation_id"),
    ("proxy_logs", "ip_geolocation_id"),
    ("error_events", "ip_geolocation_id"),
    ("visitor", "ip_address_id"),
    ("performance_metrics", "ip_address_id"),
    ("audit_logs", "ip_address_id"),
    ("request_logs", "ip_address_id"),
];

#[derive(Debug, Error)]
pub enum AnalyticsRetentionError {
    #[error("Failed to read analytics retention settings: {reason}")]
    Settings { reason: String },

    #[error("Failed to {operation} while enforcing analytics retention: {source}")]
    Database {
        operation: String,
        #[source]
        source: DbErr,
    },
}

impl AnalyticsRetentionError {
    fn database(operation: impl Into<String>, source: DbErr) -> Self {
        Self::Database {
            operation: operation.into(),
            source,
        }
    }
}

/// What a TimescaleDB retention policy needs to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    /// The installed policy already matches.
    Keep,
    /// Install or replace the policy with this window.
    Set(u32),
    /// Remove the policy: the window is now "keep".
    Remove,
}

/// Decide what to do given the installed window and the configured one.
pub fn policy_action(installed_days: Option<u32>, desired_days: Option<u32>) -> PolicyAction {
    match (installed_days, desired_days) {
        (installed, Some(desired)) if installed == Some(desired) => PolicyAction::Keep,
        (_, Some(desired)) => PolicyAction::Set(desired),
        (Some(_), None) => PolicyAction::Remove,
        (None, None) => PolicyAction::Keep,
    }
}

/// Outcome of one pass, for logging and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub events_policy: Option<PolicyAction>,
    pub error_events_policy: Option<PolicyAction>,
    pub visitors_deleted: u64,
    pub ip_addresses_deleted: u64,
}

/// Position of the unreferenced-IP sweep: `(updated_at, id)` of the last row
/// examined. Kept between runs so rows that are still referenced do not
/// starve the ones behind them.
#[derive(Debug, Clone, Copy)]
struct IpCursor {
    updated_at: DateTime<Utc>,
    id: i32,
}

impl IpCursor {
    const START: IpCursor = IpCursor {
        updated_at: DateTime::<Utc>::UNIX_EPOCH,
        id: 0,
    };
}

#[derive(Debug, FromQueryResult)]
struct IpWindowRow {
    id: i32,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct ColumnRow {
    table_name: String,
    column_name: String,
}

#[derive(Debug, FromQueryResult)]
struct PolicySecondsRow {
    seconds: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct PresenceRow {
    present: bool,
}

pub struct AnalyticsRetentionService {
    db: Arc<DatabaseConnection>,
    ip_cursor: tokio::sync::Mutex<IpCursor>,
}

impl AnalyticsRetentionService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            ip_cursor: tokio::sync::Mutex::new(IpCursor::START),
        }
    }

    /// Apply every window in `settings`. A step that fails does not stop the
    /// others: its error is logged and the pass continues, so one broken
    /// table cannot hold up the rest of the cleanup.
    pub async fn enforce(&self, settings: &AnalyticsRetentionSettings) -> RetentionReport {
        let mut report = RetentionReport::default();
        let now = Utc::now();

        match self
            .sync_policy(EVENTS_TABLE, Some(settings.events_days))
            .await
        {
            Ok(action) => report.events_policy = Some(action),
            Err(error) => {
                warn!(table = EVENTS_TABLE, %error, "analytics retention policy sync failed")
            }
        }
        match self
            .sync_policy(ERROR_EVENTS_TABLE, settings.error_events_days)
            .await
        {
            Ok(action) => report.error_events_policy = Some(action),
            Err(error) => {
                warn!(table = ERROR_EVENTS_TABLE, %error, "analytics retention policy sync failed")
            }
        }

        // Visitors before addresses: a deleted visitor releases its address.
        if let Some(days) = settings.visitor_days {
            let cutoff = now - chrono::Duration::days(i64::from(days));
            match self.purge_visitors(cutoff).await {
                Ok(deleted) => report.visitors_deleted = deleted,
                Err(error) => warn!(retention_days = days, %error, "visitor retention pass failed"),
            }
        }
        if let Some(days) = settings.unreferenced_ip_days {
            let cutoff = now - chrono::Duration::days(i64::from(days));
            match self.purge_unreferenced_ips(cutoff).await {
                Ok(deleted) => report.ip_addresses_deleted = deleted,
                Err(error) => {
                    warn!(retention_days = days, %error, "IP address retention pass failed")
                }
            }
        }

        if report.visitors_deleted > 0 || report.ip_addresses_deleted > 0 {
            info!(
                visitors_deleted = report.visitors_deleted,
                ip_addresses_deleted = report.ip_addresses_deleted,
                "analytics retention removed expired rows"
            );
        }
        report
    }

    /// Bring the retention policy of `table` in step with `desired_days`.
    ///
    /// A deployment without TimescaleDB (or where the table is not a
    /// hypertable) has no policy to manage and is left alone.
    pub async fn sync_policy(
        &self,
        table: &'static str,
        desired_days: Option<u32>,
    ) -> Result<PolicyAction, AnalyticsRetentionError> {
        if !self.is_hypertable(table).await? {
            debug!(
                table,
                "not a TimescaleDB hypertable; no retention policy to manage"
            );
            return Ok(PolicyAction::Keep);
        }
        let installed = self.installed_retention_days(table).await?;
        let action = policy_action(installed, desired_days);
        let sql = match action {
            PolicyAction::Keep => return Ok(action),
            PolicyAction::Set(days) => format!(
                "SELECT remove_retention_policy('{table}', if_exists => TRUE); \
                 SELECT add_retention_policy('{table}', \
                     drop_after => make_interval(days => {days}), if_not_exists => TRUE)"
            ),
            PolicyAction::Remove => {
                format!("SELECT remove_retention_policy('{table}', if_exists => TRUE)")
            }
        };
        self.db.execute_unprepared(&sql).await.map_err(|source| {
            AnalyticsRetentionError::database(
                format!("set the retention policy of {table} to {action:?}"),
                source,
            )
        })?;
        info!(
            table,
            installed_days = ?installed,
            ?action,
            "analytics retention policy updated"
        );
        Ok(action)
    }

    async fn is_hypertable(&self, table: &str) -> Result<bool, AnalyticsRetentionError> {
        let presence = PresenceRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT (to_regclass('timescaledb_information.hypertables') IS NOT NULL \
                     AND EXISTS (SELECT 1 FROM timescaledb_information.hypertables \
                                 WHERE hypertable_schema = current_schema() \
                                   AND hypertable_name = $1)) AS present",
            [table.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                format!("check whether {table} is a hypertable"),
                source,
            )
        })?;
        Ok(presence.is_some_and(|row| row.present))
    }

    async fn installed_retention_days(
        &self,
        table: &str,
    ) -> Result<Option<u32>, AnalyticsRetentionError> {
        let row = PolicySecondsRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT EXTRACT(EPOCH FROM (config ->> 'drop_after')::interval)::BIGINT AS seconds \
             FROM timescaledb_information.jobs \
             WHERE proc_name = 'policy_retention' \
               AND hypertable_schema = current_schema() \
               AND hypertable_name = $1 \
             LIMIT 1",
            [table.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                format!("read the retention policy of {table}"),
                source,
            )
        })?;
        Ok(row
            .and_then(|row| row.seconds)
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| u32::try_from((seconds + 43_200) / 86_400).ok()))
    }

    /// Delete visitors last seen before `cutoff`, at most
    /// `BATCH_SIZE * MAX_BATCHES_PER_RUN` per call. Returns the rows deleted.
    pub async fn purge_visitors(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AnalyticsRetentionError> {
        let mut total = 0u64;
        for batch in 0..MAX_BATCHES_PER_RUN {
            let result = self
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM visitor WHERE id IN ( \
                         SELECT id FROM visitor WHERE last_seen < $1 \
                         ORDER BY last_seen ASC LIMIT $2)",
                    [cutoff.into(), BATCH_SIZE.into()],
                ))
                .await
                .map_err(|source| {
                    AnalyticsRetentionError::database(
                        format!("delete visitors last seen before {cutoff}"),
                        source,
                    )
                })?;
            let deleted = result.rows_affected();
            total += deleted;
            if deleted < BATCH_SIZE as u64 {
                break;
            }
            if batch + 1 < MAX_BATCHES_PER_RUN {
                tokio::time::sleep(BATCH_PAUSE).await;
            }
        }
        Ok(total)
    }

    /// Delete stored IP addresses that nothing references and that were last
    /// looked up before `cutoff`. Returns the rows deleted.
    pub async fn purge_unreferenced_ips(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AnalyticsRetentionError> {
        let references = self.existing_reference_columns().await?;
        let mut cursor = self.ip_cursor.lock().await;
        let mut total = 0u64;

        for batch in 0..MAX_BATCHES_PER_RUN {
            let window = IpWindowRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, updated_at FROM ip_geolocations \
                 WHERE updated_at < $1 AND (updated_at, id) > ($2, $3) \
                 ORDER BY updated_at ASC, id ASC LIMIT $4",
                [
                    cutoff.into(),
                    cursor.updated_at.into(),
                    cursor.id.into(),
                    BATCH_SIZE.into(),
                ],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                AnalyticsRetentionError::database(
                    format!("list IP addresses not looked up since {cutoff}"),
                    source,
                )
            })?;

            let Some(last) = window.last() else {
                // End of the candidates: start over next run.
                *cursor = IpCursor::START;
                break;
            };
            let next = IpCursor {
                updated_at: last.updated_at,
                id: last.id,
            };
            let ids: Vec<i32> = window.iter().map(|row| row.id).collect();
            let reached_end = (ids.len() as i64) < BATCH_SIZE;

            let deleted = self
                .db
                .query_all(Statement::from_string(
                    DatabaseBackend::Postgres,
                    delete_unreferenced_sql(&ids, &references),
                ))
                .await
                .map_err(|source| {
                    AnalyticsRetentionError::database(
                        format!("delete {} unreferenced IP addresses", ids.len()),
                        source,
                    )
                })?;
            total += deleted.len() as u64;
            *cursor = next;

            if reached_end {
                *cursor = IpCursor::START;
                break;
            }
            if batch + 1 < MAX_BATCHES_PER_RUN {
                tokio::time::sleep(BATCH_PAUSE).await;
            }
        }
        Ok(total)
    }

    /// The subset of [`IP_REFERENCES`] present in this database.
    ///
    /// Introspected rather than assumed: a missing table must not turn the
    /// whole cleanup into an error, and a column that exists must always be
    /// consulted.
    async fn existing_reference_columns(
        &self,
    ) -> Result<Vec<(&'static str, &'static str)>, AnalyticsRetentionError> {
        let rows = ColumnRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT table_name, column_name FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND ({})",
                IP_REFERENCES
                    .iter()
                    .map(|(table, column)| format!(
                        "(table_name = '{table}' AND column_name = '{column}')"
                    ))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                "list the columns that reference IP addresses",
                source,
            )
        })?;

        Ok(IP_REFERENCES
            .iter()
            .copied()
            .filter(|(table, column)| {
                rows.iter()
                    .any(|row| row.table_name == *table && row.column_name == *column)
            })
            .collect())
    }
}

/// `DELETE` of the candidate ids that no referencing column points at,
/// returning the ids actually removed.
///
/// The ids are `i32`s read from the database a moment earlier and the table
/// and column names are constants, so the statement is built directly: there
/// is no caller-controlled text in it.
fn delete_unreferenced_sql(ids: &[i32], references: &[(&str, &str)]) -> String {
    let id_list = ids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let not_referenced: String = references
        .iter()
        .map(|(table, column)| {
            format!(" AND NOT EXISTS (SELECT 1 FROM {table} r WHERE r.{column} = g.id)")
        })
        .collect();
    format!(
        "DELETE FROM ip_geolocations g WHERE g.id IN ({id_list}){not_referenced} RETURNING g.id"
    )
}

/// Spawn the hourly enforcement loop. The settings are re-read every pass, so
/// a change in the console applies within [`RETENTION_INTERVAL`].
pub fn spawn_retention_job(
    service: Arc<AnalyticsRetentionService>,
    config: Arc<temps_config::ConfigService>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(RETENTION_STARTUP_DELAY).await;
        let mut interval = tokio::time::interval(RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match config.get_settings().await {
                Ok(settings) => {
                    service.enforce(&settings.analytics_retention).await;
                }
                Err(error) => {
                    let error = AnalyticsRetentionError::Settings {
                        reason: error.to_string(),
                    };
                    warn!(%error, "skipping analytics retention pass");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn mock_service(db: DatabaseConnection) -> AnalyticsRetentionService {
        AnalyticsRetentionService::new(Arc::new(db))
    }

    #[test]
    fn policy_action_keeps_a_matching_policy() {
        assert_eq!(policy_action(Some(90), Some(90)), PolicyAction::Keep);
        assert_eq!(policy_action(None, None), PolicyAction::Keep);
    }

    #[test]
    fn policy_action_installs_or_replaces_to_the_configured_window() {
        assert_eq!(policy_action(None, Some(30)), PolicyAction::Set(30));
        assert_eq!(policy_action(Some(90), Some(30)), PolicyAction::Set(30));
    }

    #[test]
    fn policy_action_removes_a_policy_when_the_window_becomes_keep() {
        assert_eq!(policy_action(Some(30), None), PolicyAction::Remove);
    }

    #[test]
    fn delete_sql_checks_every_reference_and_returns_the_ids() {
        let sql = delete_unreferenced_sql(
            &[3, 5],
            &[
                ("events", "ip_geolocation_id"),
                ("visitor", "ip_address_id"),
            ],
        );
        assert!(sql.starts_with("DELETE FROM ip_geolocations g WHERE g.id IN (3, 5)"));
        assert!(
            sql.contains("NOT EXISTS (SELECT 1 FROM events r WHERE r.ip_geolocation_id = g.id)")
        );
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM visitor r WHERE r.ip_address_id = g.id)"));
        assert!(sql.ends_with("RETURNING g.id"));
    }

    #[test]
    fn reference_list_covers_audit_logs_and_visitors() {
        // The columns without a foreign key are the dangerous ones: the
        // database would not stop a delete, so dropping one of these from the
        // list silently orphans live rows.
        for expected in [
            ("audit_logs", "ip_address_id"),
            ("visitor", "ip_address_id"),
            ("performance_metrics", "ip_address_id"),
            ("proxy_logs", "ip_geolocation_id"),
            ("events", "ip_geolocation_id"),
            ("error_events", "ip_geolocation_id"),
        ] {
            assert!(IP_REFERENCES.contains(&expected), "missing {expected:?}");
        }
    }

    #[tokio::test]
    async fn purge_visitors_stops_after_a_short_batch() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 7,
            }])
            .into_connection();
        let service = mock_service(db);

        let deleted = service
            .purge_visitors(Utc::now())
            .await
            .expect("a short batch ends the pass");

        assert_eq!(deleted, 7);
    }

    #[tokio::test]
    async fn purge_visitors_binds_cutoff_and_batch_size() {
        let cutoff = Utc::now() - chrono::Duration::days(425);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let service = AnalyticsRetentionService::new(db.clone());
        service.purge_visitors(cutoff).await.expect("empty pass");
        drop(service);

        let log = Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("service dropped, connection is unshared"))
            .into_transaction_log();
        assert_eq!(log.len(), 1, "one bounded DELETE per batch");
        let rendered = format!("{log:?}");
        assert!(
            rendered.contains("DELETE FROM visitor WHERE id IN"),
            "{rendered}"
        );
        assert!(rendered.contains("LIMIT $2"), "{rendered}");
    }

    #[tokio::test]
    async fn purge_visitors_reports_the_failed_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([DbErr::Custom("deadlock detected".into())])
            .into_connection();
        let service = mock_service(db);

        let error = service
            .purge_visitors(Utc::now())
            .await
            .expect_err("a database failure surfaces");

        let message = error.to_string();
        assert!(
            message.contains("delete visitors last seen before"),
            "{message}"
        );
        assert!(message.contains("deadlock detected"), "{message}");
    }

    #[tokio::test]
    async fn sync_policy_leaves_a_non_hypertable_alone() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[std::collections::BTreeMap::from([(
                "present",
                sea_orm::Value::Bool(Some(false)),
            )])]])
            .into_connection();
        let service = mock_service(db);

        let action = service
            .sync_policy(ERROR_EVENTS_TABLE, Some(30))
            .await
            .expect("a plain table has no policy to manage");

        assert_eq!(action, PolicyAction::Keep);
    }
}

#[cfg(test)]
mod integration_tests {
    //! Run against a real TimescaleDB container; skipped without Docker.

    use super::*;
    use crate::testing::test_utils::TestDatabase;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use temps_entities::upstream_config::UpstreamList;
    use temps_entities::{audit_logs, environments, ip_geolocations, projects, visitor};

    async fn database(name: &str) -> Option<TestDatabase> {
        match TestDatabase::new(name).await {
            Ok(db) => Some(db),
            Err(error) => {
                println!("Docker/TimescaleDB not available, skipping: {error}");
                None
            }
        }
    }

    /// Insert an address row last looked up `age_days` ago.
    async fn insert_ip(db: &DatabaseConnection, ip: &str, age_days: i64) -> i32 {
        let at = Utc::now() - chrono::Duration::days(age_days);
        ip_geolocations::ActiveModel {
            ip_address: Set(ip.to_string()),
            country: Set("Testland".to_string()),
            is_eu: Set(false),
            created_at: Set(at),
            updated_at: Set(at),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert ip")
        .id
    }

    async fn ip_exists(db: &DatabaseConnection, id: i32) -> bool {
        ip_geolocations::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("query ip")
            .is_some()
    }

    /// A project with one environment; returns `(project_id, environment_id)`.
    async fn seed_environment(db: &DatabaseConnection) -> (i32, i32) {
        let now = Utc::now();
        let project = projects::ActiveModel {
            name: Set("retention".to_string()),
            slug: Set("retention".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("retention".to_string()),
            repo_owner: Set("retention".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project");
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("retention".to_string()),
            host: Set("retention.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            project_id: Set(project.id),
            branch: Set(Some("main".to_string())),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert environment");
        (project.id, environment.id)
    }

    async fn insert_visitor(
        db: &DatabaseConnection,
        ids: (i32, i32),
        name: &str,
        seen_days_ago: i64,
        ip_address_id: Option<i32>,
    ) {
        let seen = Utc::now() - chrono::Duration::days(seen_days_ago);
        visitor::ActiveModel {
            visitor_id: Set(name.to_string()),
            project_id: Set(ids.0),
            environment_id: Set(ids.1),
            first_seen: Set(seen),
            last_seen: Set(seen),
            ip_address_id: Set(ip_address_id),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert visitor");
    }

    #[tokio::test]
    async fn only_unreferenced_and_old_addresses_are_deleted() {
        let Some(test_db) = database("retention_ips").await else {
            return;
        };
        let db = test_db.db.as_ref();

        let stale_orphan = insert_ip(db, "198.51.100.1", 60).await;
        let recent_orphan = insert_ip(db, "198.51.100.2", 2).await;
        let used_by_visitor = insert_ip(db, "198.51.100.3", 60).await;
        let used_by_audit = insert_ip(db, "198.51.100.4", 60).await;

        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "kept-visitor", 1, Some(used_by_visitor)).await;
        let now = Utc::now();
        audit_logs::ActiveModel {
            user_agent: Set("retention-test".to_string()),
            operation_type: Set("LOGIN".to_string()),
            ip_address_id: Set(Some(used_by_audit)),
            audit_date: Set(now),
            created_at: Set(now),
            data: Set("{}".to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert audit log");

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let deleted = service
            .purge_unreferenced_ips(Utc::now() - chrono::Duration::days(30))
            .await
            .expect("purge runs");

        assert_eq!(deleted, 1);
        assert!(
            !ip_exists(db, stale_orphan).await,
            "stale orphan is removed"
        );
        assert!(
            ip_exists(db, recent_orphan).await,
            "recent lookups are kept"
        );
        assert!(
            ip_exists(db, used_by_visitor).await,
            "visitor still uses it"
        );
        assert!(
            ip_exists(db, used_by_audit).await,
            "audit log still uses it"
        );
    }

    #[tokio::test]
    async fn an_address_is_released_once_its_visitor_is_purged() {
        let Some(test_db) = database("retention_chain").await else {
            return;
        };
        let db = test_db.db.as_ref();
        let ip = insert_ip(db, "198.51.100.9", 500).await;
        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "gone", 480, Some(ip)).await;

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let report = service
            .enforce(&AnalyticsRetentionSettings {
                events_days: 90,
                error_events_days: None,
                visitor_days: Some(425),
                unreferenced_ip_days: Some(30),
            })
            .await;

        assert_eq!(report.visitors_deleted, 1);
        assert_eq!(report.ip_addresses_deleted, 1);
        assert!(!ip_exists(db, ip).await);
    }

    #[tokio::test]
    async fn visitors_not_seen_since_the_cutoff_are_deleted() {
        let Some(test_db) = database("retention_visitors").await else {
            return;
        };
        let db = test_db.db.as_ref();
        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "old", 480, None).await;
        insert_visitor(db, ids, "recent", 3, None).await;

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let deleted = service
            .purge_visitors(Utc::now() - chrono::Duration::days(425))
            .await
            .expect("purge runs");

        assert_eq!(deleted, 1);
        let remaining = visitor::Entity::find()
            .all(db)
            .await
            .expect("remaining visitors");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].visitor_id, "recent");
    }

    #[tokio::test]
    async fn the_error_events_policy_follows_the_setting() {
        let Some(test_db) = database("retention_policy").await else {
            return;
        };
        let service = AnalyticsRetentionService::new(test_db.db.clone());

        // `events` ships with the 90-day policy from the initial schema.
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(90)).await.unwrap(),
            PolicyAction::Keep
        );
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(30)).await.unwrap(),
            PolicyAction::Set(30)
        );
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(30)).await.unwrap(),
            PolicyAction::Keep,
            "an unchanged setting is a no-op on the next pass"
        );

        // `error_events` has no policy until one is configured.
        assert_eq!(
            service.sync_policy(ERROR_EVENTS_TABLE, None).await.unwrap(),
            PolicyAction::Keep
        );
        assert_eq!(
            service
                .sync_policy(ERROR_EVENTS_TABLE, Some(14))
                .await
                .unwrap(),
            PolicyAction::Set(14)
        );
        assert_eq!(
            service.sync_policy(ERROR_EVENTS_TABLE, None).await.unwrap(),
            PolicyAction::Remove
        );
    }
}
