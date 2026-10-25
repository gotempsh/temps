// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-sandbox egress policy: which outside destinations a sandbox may reach.
//!
//! Adds `sandboxes.egress_policy`, a JSON object
//! `{"mode": "internet" | "restricted", "rules": [...]}` (see
//! `temps_workload::SandboxEgressPolicy`). Existing sandboxes, and new ones
//! that do not ask for a policy, get `internet`: any public destination on
//! ports 80 and 443, which is exactly what every sandbox's egress proxy
//! enforced before this column existed, so the migration changes no
//! behaviour.
//!
//! The CHECK constraint keeps the column an object with a known mode; the
//! rules themselves are validated by the API, which is the only writer.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
ALTER TABLE sandboxes
    ADD COLUMN IF NOT EXISTS egress_policy JSONB NOT NULL
        DEFAULT '{"mode":"internet","rules":[]}'::jsonb;
ALTER TABLE sandboxes DROP CONSTRAINT IF EXISTS sandboxes_egress_policy_valid;
ALTER TABLE sandboxes
    ADD CONSTRAINT sandboxes_egress_policy_valid
        CHECK (
            jsonb_typeof(egress_policy) = 'object'
            AND egress_policy->>'mode' IN ('internet', 'restricted')
            AND jsonb_typeof(egress_policy->'rules') = 'array'
        );
"#;

pub const DOWN_SQL: &str = r#"
ALTER TABLE sandboxes
    DROP CONSTRAINT IF EXISTS sandboxes_egress_policy_valid,
    DROP COLUMN IF EXISTS egress_policy;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_defaults_every_sandbox_to_internet() {
        for fragment in [
            "ADD COLUMN IF NOT EXISTS egress_policy JSONB NOT NULL",
            r#"DEFAULT '{"mode":"internet","rules":[]}'::jsonb"#,
            "ADD CONSTRAINT sandboxes_egress_policy_valid",
            "egress_policy->>'mode' IN ('internet', 'restricted')",
        ] {
            assert!(UP_SQL.contains(fragment), "up is missing `{fragment}`");
        }
    }

    #[test]
    fn down_drops_the_column_and_its_constraint() {
        assert!(DOWN_SQL.contains("DROP COLUMN IF EXISTS egress_policy"));
        assert!(DOWN_SQL.contains("DROP CONSTRAINT IF EXISTS sandboxes_egress_policy_valid"));
    }

    #[test]
    fn is_registered() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names
            .iter()
            .any(|n| n == "m20261024_000011_sandbox_egress_policy"));
    }
}
