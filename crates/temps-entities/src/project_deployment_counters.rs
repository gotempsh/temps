// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project deployment counter: the numbers in deployment slugs.
//!
//! A deployment slug (`<project>-<n>`) is also the deployment's container name
//! and hostname, so two deployments must never share one. Every path that
//! numbers a deployment allocates through [`allocate_deployment_slug`], which
//! bumps this row atomically instead of counting the project's deployments.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseBackend, DbErr, Statement};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_deployment_counters")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    /// The last deployment number handed out for the project.
    pub last_number: i64,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}

/// Bump an existing counter. Postgres serialises concurrent updates of the
/// row, so each caller reads back a different number.
const BUMP_SQL: &str = "UPDATE project_deployment_counters \
     SET last_number = last_number + 1, updated_at = NOW() \
     WHERE project_id = $1 \
     RETURNING last_number";

/// Create the counter on a project's first allocation, seeded above both the
/// project's deployment count (the old numbering) and the highest number
/// already at the end of one of its slugs, so no existing slug is reused. If a
/// concurrent caller created the row first, bump it instead. Slugs ending in
/// more than nine digits (a hex suffix that happens to be all digits) are not
/// numbers this counter produced and are ignored.
const SEED_SQL: &str = "INSERT INTO project_deployment_counters (project_id, last_number) \
     SELECT $1, GREATEST( \
         COUNT(*), \
         COALESCE(MAX(SUBSTRING(slug FROM '-([0-9]{1,9})$')::BIGINT), 0) \
     ) + 1 \
     FROM deployments WHERE project_id = $1 \
     ON CONFLICT (project_id) DO UPDATE \
     SET last_number = project_deployment_counters.last_number + 1, updated_at = NOW() \
     RETURNING last_number";

/// Allocate the next deployment number for `project_id`. Distinct for every
/// call, including concurrent ones; numbers left unused by a deployment that
/// later fails to insert are simply skipped.
pub async fn next_deployment_number<C>(db: &C, project_id: i32) -> Result<i64, DbErr>
where
    C: ConnectionTrait,
{
    for sql in [BUMP_SQL, SEED_SQL] {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [project_id.into()],
            ))
            .await?;
        if let Some(row) = row {
            return row.try_get::<i64>("", "last_number");
        }
    }
    Err(DbErr::RecordNotFound(format!(
        "allocating a deployment number for project {project_id} returned no row"
    )))
}

/// Allocate a unique deployment slug, `<prefix>-<n>`, where `prefix` is the
/// project slug (or, for a preview, the project slug and the branch).
pub async fn allocate_deployment_slug<C>(
    db: &C,
    project_id: i32,
    prefix: &str,
) -> Result<String, DbErr>
where
    C: ConnectionTrait,
{
    let number = next_deployment_number(db, project_id).await?;
    Ok(format!("{prefix}-{number}"))
}
