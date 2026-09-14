// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const DIGEST_PREDICATE: &str =
    "image ~ '^ghcr[.]io/gotempsh/temps-sandbox-(nodejs|python|all)@sha256:[0-9a-f]{64}$'";

fn up_sql() -> Result<String, DbErr> {
    let previous = super::m20260913_000002_managed_daemon_workspace_images_v034::UP_SQL;
    let without_closing = previous.strip_suffix("))").ok_or_else(|| {
        DbErr::Custom("previous managed daemon image constraint has an unexpected shape".into())
    })?;
    Ok(format!("{without_closing}) OR {DIGEST_PREDICATE})"))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&up_sql()?)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // PostgreSQL rejects a downgrade while a digest-pinned workspace remains.
        manager
            .get_connection()
            .execute_unprepared(
                super::m20260913_000002_managed_daemon_workspace_images_v034::UP_SQL,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_constraint_retains_every_historical_tag_and_limits_repositories() {
        let sql = up_sql().expect("valid previous constraint");
        let historical =
            super::super::m20260913_000002_managed_daemon_workspace_images_v034::UP_SQL;
        for tag in ["node:0.1.0", "nodejs:0.2.0", "python:0.3.0", "all:0.3.4"] {
            assert!(
                sql.contains(tag),
                "historical tag {tag} must remain allowed"
            );
            assert!(historical.contains(tag));
        }
        assert!(sql.contains(DIGEST_PREDICATE));
        assert!(!DIGEST_PREDICATE.contains("temps-sandbox-bun"));
    }
}
