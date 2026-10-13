// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Builds without a release manifest pin the `-beta` tag that main-branch
/// builds publish, because the stable tag only exists once a release publishes
/// it. The tag is mutable, so unlike the digest form it is limited to this one
/// suffix on the three managed repositories.
const BETA_PREDICATE: &str =
    "image ~ '^ghcr[.]io/gotempsh/temps-sandbox-(nodejs|python|all):[0-9]+[.][0-9]+[.][0-9]+-beta$'";

fn up_sql() -> Result<String, DbErr> {
    let previous = super::m20260914_000001_managed_daemon_digest_images::up_sql()?;
    let without_closing = previous.strip_suffix(')').ok_or_else(|| {
        DbErr::Custom("previous managed daemon image constraint has an unexpected shape".into())
    })?;
    Ok(format!("{without_closing} OR {BETA_PREDICATE})"))
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
        // PostgreSQL rejects a downgrade while a workspace still pins a beta tag.
        manager
            .get_connection()
            .execute_unprepared(&super::m20260914_000001_managed_daemon_digest_images::up_sql()?)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_constraint_keeps_tags_and_digests_and_limits_the_suffix() {
        let sql = up_sql().expect("valid previous constraint");
        for tag in ["node:0.1.0", "nodejs:0.2.0", "all:0.3.4"] {
            assert!(
                sql.contains(tag),
                "historical tag {tag} must remain allowed"
            );
        }
        assert!(sql.contains("@sha256:"), "digest form must remain allowed");
        assert!(sql.contains(BETA_PREDICATE));
        assert!(sql.ends_with("-beta$')"), "CHECK must be closed: {sql}");
        assert!(!BETA_PREDICATE.contains("temps-sandbox-bun"));
        assert_eq!(
            sql.matches('(').count(),
            sql.matches(')').count(),
            "parentheses must balance in {sql}"
        );
    }
}
