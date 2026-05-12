//! Workspace deployment-token refresher.
//!
//! Each workspace session gets a project-scoped deployment token (named
//! `workspace-session-{id}`) at sandbox-init time, with a 6h expiry — see
//! `MessageExecutor::issue_session_token`. The token is what lets the
//! in-sandbox CLI and the credential daemon talk back to the control plane
//! (OTel ingest, `temps errors list`, etc).
//!
//! Without a periodic refresher, any session left idle for >6h has its token
//! expire and every subsequent API call from inside the sandbox 401s with
//! "Invalid or expired deployment token", even though the session itself is
//! still alive.
//!
//! This service walks active workspace sessions on a fixed cadence and, for
//! any whose token is about to expire (within `refresh_when_within`), calls
//! `MessageExecutor::refresh_sandbox` to re-issue the token and rewrite the
//! sandbox's `~/.env` + credential daemon files. The container's *process
//! env* still carries the old `TEMPS_API_TOKEN` until restart — that's a
//! known limitation documented on `refresh_sandbox` — but every tool that
//! reads from `~/.env` per command (the CLI, agent shells) picks up the
//! refreshed value transparently.
//!
//! Failures are logged at WARN and never crash the loop. A stuck session
//! cannot block the refresher from servicing others.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, PaginatorTrait,
    QueryFilter, Statement,
};
use temps_entities::workspace_sessions;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::services::message_executor::MessageExecutor;

/// How often the refresher wakes up to scan for soon-to-expire tokens.
///
/// 30 minutes is well under the 6h token lifetime — even a session that just
/// missed one tick still has ~5.5h of token validity left, so a single missed
/// scan can't cause an expiry. Going lower buys nothing; going higher risks
/// stacked misses bumping into the threshold.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Refresh any token whose `expires_at` is within this window of now.
///
/// At 90 minutes the typical session refreshes ~4.5h after creation, then
/// every cycle thereafter. That gives plenty of slack for a refresh attempt
/// to fail and be retried on the next tick before the token actually dies.
pub const DEFAULT_REFRESH_WHEN_WITHIN: Duration = Duration::from_secs(90 * 60);

/// Background task that periodically refreshes workspace-session deployment
/// tokens before they expire.
pub struct TokenRefresher {
    db: Arc<DatabaseConnection>,
    executor: Arc<MessageExecutor>,
    poll_interval: Duration,
    refresh_when_within: Duration,
}

impl TokenRefresher {
    pub fn new(db: Arc<DatabaseConnection>, executor: Arc<MessageExecutor>) -> Self {
        Self {
            db,
            executor,
            poll_interval: DEFAULT_POLL_INTERVAL,
            refresh_when_within: DEFAULT_REFRESH_WHEN_WITHIN,
        }
    }

    /// Override the poll interval. Primarily for tests; production should
    /// stick with `DEFAULT_POLL_INTERVAL`.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Override the refresh threshold. Primarily for tests; production should
    /// stick with `DEFAULT_REFRESH_WHEN_WITHIN`.
    pub fn with_refresh_when_within(mut self, within: Duration) -> Self {
        self.refresh_when_within = within;
        self
    }

    /// Spawn the refresher onto the current tokio runtime and return its
    /// `JoinHandle`. The task runs until the runtime is shut down.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Run loop. Sleeps `poll_interval` between scans. Skips the first
    /// immediate tick so the plugin init phase isn't blocked.
    async fn run(self) {
        let mut tick = tokio::time::interval(self.poll_interval);
        tick.tick().await; // skip immediate first tick
        loop {
            tick.tick().await;
            self.scan_and_refresh_once().await;
        }
    }

    /// Run one scan pass synchronously. Public so tests can drive it without
    /// waiting on the timer.
    pub async fn scan_and_refresh_once(&self) {
        let cutoff = Utc::now()
            + chrono::Duration::from_std(self.refresh_when_within)
                .unwrap_or_else(|_| chrono::Duration::minutes(90));

        let due = match find_sessions_due_for_refresh(self.db.as_ref(), cutoff).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("Token refresher: failed to query due sessions: {}", e);
                return;
            }
        };

        if due.is_empty() {
            debug!("Token refresher: nothing due (cutoff {})", cutoff);
            return;
        }

        info!(
            "Token refresher: {} workspace session(s) have tokens expiring within threshold",
            due.len()
        );

        for row in due {
            // Best-effort, one session at a time. Each refresh takes the
            // executor's per-session lock so an in-flight chat turn won't
            // race with us — but if the session has been closed in the
            // window between query and refresh, `refresh_sandbox` returns
            // SessionNotActive which we log and skip.
            match self.executor.refresh_sandbox(row.session_id).await {
                Ok(()) => {
                    info!(
                        "Token refresher: refreshed session {} (token expired at {})",
                        row.session_id, row.expires_at
                    );
                }
                Err(e) => {
                    warn!(
                        "Token refresher: failed to refresh session {} (token expires {}): {}",
                        row.session_id, row.expires_at, e
                    );
                }
            }
        }
    }
}

/// One row of the due-for-refresh query.
#[derive(Debug, Clone, FromQueryResult)]
pub struct DueSession {
    pub session_id: i32,
    pub token_id: i32,
    pub expires_at: DateTime<Utc>,
}

/// Find active workspace sessions whose deployment token will expire on or
/// before `cutoff`. Returns one row per session even if (somehow) there are
/// multiple matching tokens — DISTINCT ON keeps the soonest-to-expire.
///
/// The session/token correspondence is established by token name —
/// `MessageExecutor::issue_session_token` always names the token
/// `workspace-session-{session.id}`. We match on that and on
/// `project_id` for safety so two projects with overlapping numeric
/// session IDs can never collide.
async fn find_sessions_due_for_refresh(
    db: &DatabaseConnection,
    cutoff: DateTime<Utc>,
) -> Result<Vec<DueSession>, sea_orm::DbErr> {
    // Cheap early-out: if there are no active sessions, skip the join entirely.
    let active_count = workspace_sessions::Entity::find()
        .filter(workspace_sessions::Column::Status.eq("active"))
        .count(db)
        .await?;
    if active_count == 0 {
        return Ok(Vec::new());
    }

    let sql = r#"
        SELECT DISTINCT ON (s.id)
            s.id            AS session_id,
            dt.id           AS token_id,
            dt.expires_at   AS expires_at
        FROM workspace_sessions s
        JOIN deployment_tokens dt
          ON dt.project_id = s.project_id
         AND dt.name = 'workspace-session-' || s.id::text
        WHERE s.status = 'active'
          AND dt.is_active = true
          AND dt.expires_at IS NOT NULL
          AND dt.expires_at <= $1
        ORDER BY s.id, dt.expires_at ASC
    "#;

    let stmt = Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, vec![cutoff.into()]);

    DueSession::find_by_statement(stmt).all(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{MockDatabase, Value};
    use std::collections::BTreeMap;

    /// Build a mock count() result row. Sea-ORM's `count` calls
    /// `select COUNT(*) AS num_items FROM ...`, so the row needs a
    /// `num_items` column of type `i64`.
    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::new();
        row.insert("num_items".to_string(), Value::BigInt(Some(n)));
        row
    }

    /// Build one DISTINCT-ON-join result row in the shape that
    /// `find_sessions_due_for_refresh` expects.
    fn due_row(
        session_id: i32,
        token_id: i32,
        expires_at: DateTime<Utc>,
    ) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::new();
        row.insert("session_id".to_string(), Value::Int(Some(session_id)));
        row.insert("token_id".to_string(), Value::Int(Some(token_id)));
        row.insert(
            "expires_at".to_string(),
            Value::ChronoDateTimeUtc(Some(Box::new(expires_at))),
        );
        row
    }

    #[tokio::test]
    async fn test_find_no_active_sessions_returns_empty_without_join() {
        // Only the count query is set up. If the code under test reached the
        // join (it shouldn't when active_count is 0) the mock would panic for
        // missing query result — so the absence of that panic is itself part
        // of the assertion.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(0)]])
            .into_connection();

        let cutoff = Utc::now() + chrono::Duration::minutes(90);
        let rows = find_sessions_due_for_refresh(&db, cutoff).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_find_returns_due_rows() {
        let now = Utc::now();
        let expires_a = now + chrono::Duration::minutes(30);
        let expires_b = now + chrono::Duration::minutes(60);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Count: 2 active sessions.
            .append_query_results([vec![count_row(2)]])
            // Join: two rows due for refresh.
            .append_query_results([vec![due_row(7, 42, expires_a), due_row(11, 51, expires_b)]])
            .into_connection();

        let cutoff = now + chrono::Duration::minutes(90);
        let rows = find_sessions_due_for_refresh(&db, cutoff).await.unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, 7);
        assert_eq!(rows[0].token_id, 42);
        assert_eq!(rows[0].expires_at, expires_a);
        assert_eq!(rows[1].session_id, 11);
        assert_eq!(rows[1].token_id, 51);
        assert_eq!(rows[1].expires_at, expires_b);
    }

    #[test]
    fn test_defaults_are_sane() {
        // The threshold must be strictly less than a token lifetime — otherwise
        // we'd refresh every tick from creation. The 6h lifetime is enforced
        // in MessageExecutor::issue_session_token; 90min << 6h.
        assert!(DEFAULT_REFRESH_WHEN_WITHIN < Duration::from_secs(6 * 60 * 60));
        // The poll interval must be shorter than the refresh threshold so a
        // single missed tick can't push us past it.
        assert!(DEFAULT_POLL_INTERVAL < DEFAULT_REFRESH_WHEN_WITHIN);
    }
}
