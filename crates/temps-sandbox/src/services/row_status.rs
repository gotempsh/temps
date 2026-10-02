// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conditional writes to `sandboxes` rows.
//!
//! Lifecycle calls read a row, wait on a provider (a worker node can take
//! tens of seconds), then write the row. A destroy or a node eviction can
//! land in that window. An unconditional write would then put `running` or
//! `stopped` back over `destroyed`: the row would point at a container that
//! no longer exists and block removing its node. Every such write goes
//! through [`update_when`], which only applies while the row is still in a
//! state the caller expects, and reports when it is not.

use sea_orm::{
    sea_query::SimpleExpr, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter,
};
use temps_entities::sandboxes;

/// Status of a row that is gone for good.
pub const DESTROYED: &str = "destroyed";

/// Which current statuses a conditional write accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect<'a> {
    /// The row must be in exactly this status.
    Status(&'a str),
    /// The row must not be destroyed (any live status).
    Live,
}

impl Expect<'_> {
    fn condition(self) -> SimpleExpr {
        match self {
            Expect::Status(status) => sandboxes::Column::Status.eq(status),
            Expect::Live => sandboxes::Column::Status.ne(DESTROYED),
        }
    }
}

/// Apply `changes` (an active model carrying the row's primary key) only if
/// the row's current status matches `expect`. `Ok(None)` means the row was
/// not updated: it is gone, or moved to another status meanwhile (the caller
/// decides what that means, typically by re-reading it).
pub async fn update_when<C: ConnectionTrait>(
    db: &C,
    changes: sandboxes::ActiveModel,
    expect: Expect<'_>,
) -> Result<Option<sandboxes::Model>, DbErr> {
    match sandboxes::Entity::update(changes)
        .filter(expect.condition())
        .exec(db)
        .await
    {
        Ok(model) => Ok(Some(model)),
        Err(DbErr::RecordNotUpdated) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveValue::Set, DatabaseBackend, MockDatabase};

    fn row(id: i32, status: &str) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id,
            node_id: Some(3),
            public_id: format!("sbx_{:016x}", id),
            user_id: Some(1),
            agent_run_id: None,
            name: format!("sbx-{id}"),
            status: status.to_string(),
            image: None,
            work_dir: "/workspace".to_string(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    fn stop(id: i32) -> sandboxes::ActiveModel {
        sandboxes::ActiveModel {
            id: Set(id),
            status: Set("stopped".to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn applies_when_the_row_is_in_the_expected_status() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(7, "stopped")]])
            .into_connection();

        let updated = update_when(&db, stop(7), Expect::Status("running"))
            .await
            .expect("update");

        assert_eq!(updated.map(|r| r.status).as_deref(), Some("stopped"));
        let sql = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("UPDATE"), "{sql}");
        assert!(
            sql.contains(r#""status" = 'running'"#),
            "the write must be conditioned on the expected status: {sql}"
        );
    }

    /// A destroy that lands while the caller waits on a worker leaves the
    /// row `destroyed`; the conditional write then matches nothing and must
    /// say so instead of resurrecting the row.
    #[tokio::test]
    async fn reports_a_row_that_moved_on() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();

        let updated = update_when(&db, stop(7), Expect::Status("running"))
            .await
            .expect("no error for a row that moved on");

        assert!(updated.is_none());
    }

    #[tokio::test]
    async fn live_condition_excludes_destroyed_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();

        assert!(update_when(&db, stop(7), Expect::Live)
            .await
            .expect("update")
            .is_none());
        let sql = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains(r#""status" <> 'destroyed'"#), "{sql}");
    }

    #[tokio::test]
    async fn database_errors_are_not_mistaken_for_a_moved_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".into())])
            .into_connection();

        let err = update_when(&db, stop(7), Expect::Live)
            .await
            .expect_err("database errors propagate");
        assert!(err.to_string().contains("connection reset"), "{err}");
    }
}
