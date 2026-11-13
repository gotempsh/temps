// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lets an environment publish its subdomain alias,
//! `<environment subdomain>.temps.local` (ADR-105 D7), as its own owner kind,
//! `environment_subdomain`, in `service_endpoints` and `dns_name_claims`.
//!
//! The alias has an owner of its own, not the environment's: a publish claims
//! every name it writes or none, so an alias held by a managed service would
//! otherwise also stop the environment's `<env>.<project>.temps.local` name
//! from updating. `owner_id` is `environments.id`, as for `environment`.
//!
//! No backfill: the DNS publisher writes the aliases on its next reconcile.
//! `down` deletes the aliases' records and claims, then restores both
//! constraints; nothing else ever had that owner kind.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const BEFORE: &str =
    "'service_member', 'service_role', 'node', 'static', 'deployment', 'environment'";
const AFTER: &str = "'service_member', 'service_role', 'node', 'static', 'deployment', \
                     'environment', 'environment_subdomain'";

fn constraints(kinds: &str) -> [String; 4] {
    [
        "ALTER TABLE service_endpoints DROP CONSTRAINT IF EXISTS service_endpoints_owner_kind_valid"
            .to_string(),
        format!(
            "ALTER TABLE service_endpoints ADD CONSTRAINT service_endpoints_owner_kind_valid \
             CHECK (owner_kind IN ({kinds}))"
        ),
        "ALTER TABLE dns_name_claims DROP CONSTRAINT IF EXISTS dns_name_claims_owner_kind_check"
            .to_string(),
        format!(
            "ALTER TABLE dns_name_claims ADD CONSTRAINT dns_name_claims_owner_kind_check \
             CHECK (owner_kind IN ({kinds}))"
        ),
    ]
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in constraints(AFTER) {
            conn.execute_unprepared(&statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in [
            "DELETE FROM service_endpoints WHERE owner_kind = 'environment_subdomain'",
            "DELETE FROM dns_name_claims WHERE owner_kind = 'environment_subdomain'",
            "DELETE FROM dns_name_conflicts \
             WHERE owner_kind = 'environment_subdomain' OR holder_kind = 'environment_subdomain'",
        ] {
            conn.execute_unprepared(statement).await?;
        }
        for statement in constraints(BEFORE) {
            conn.execute_unprepared(&statement).await?;
        }
        Ok(())
    }
}
