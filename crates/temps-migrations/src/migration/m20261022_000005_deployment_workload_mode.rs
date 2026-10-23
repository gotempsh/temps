// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 5: deployments onto the workload plane, resources and
//! placement.
//!
//! - `environments.workload_mode` (`legacy` | `shadow` | `enforce`, default
//!   `legacy`): which scheduler places the environment's deployments. Every
//!   existing environment stays on `NodeScheduler`; `shadow` also runs the
//!   unified scheduler and logs where it would disagree.
//! - `nodes.resource_policy` (`best_effort` | `enforced`, default
//!   `best_effort`): whether the scheduler refuses a placement whose
//!   reservations would exceed the node's schedulable capacity.
//! - `nodes.system_reserved` (`{"cpu_millis", "memory_mb"}`): the capacity
//!   Temps keeps for itself on the node, deducted before scheduling. The
//!   defaults are ADR-055 §5's measured values per role: a worker keeps 0.25 vCPU
//!   and 256 MB; a control-plane row keeps the control plane's larger
//!   (local-database) reserve. The control plane itself (node 0) is not a
//!   row; its values live in the instance settings beside its other
//!   node-0 settings.
//!
//! Nothing changes for an existing workload: `legacy` keeps the old code
//! path, and `best_effort` keeps load-based placement.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE environments
    ADD COLUMN IF NOT EXISTS workload_mode TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE environments
    DROP CONSTRAINT IF EXISTS environments_workload_mode_valid;
ALTER TABLE environments
    ADD CONSTRAINT environments_workload_mode_valid
        CHECK (workload_mode IN ('legacy', 'shadow', 'enforce'));

ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS resource_policy TEXT NOT NULL DEFAULT 'best_effort';
ALTER TABLE nodes
    DROP CONSTRAINT IF EXISTS nodes_resource_policy_valid;
ALTER TABLE nodes
    ADD CONSTRAINT nodes_resource_policy_valid
        CHECK (resource_policy IN ('best_effort', 'enforced'));

ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS system_reserved JSONB NOT NULL
        DEFAULT '{"cpu_millis": 250, "memory_mb": 256}'::jsonb;
UPDATE nodes
   SET system_reserved = '{"cpu_millis": 500, "memory_mb": 1536}'::jsonb
 WHERE role IN ('control', 'control-plane');
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE nodes DROP COLUMN IF EXISTS system_reserved;
ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_resource_policy_valid;
ALTER TABLE nodes DROP COLUMN IF EXISTS resource_policy;
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_workload_mode_valid;
ALTER TABLE environments DROP COLUMN IF EXISTS workload_mode;
"#,
            )
            .await?;
        Ok(())
    }
}
