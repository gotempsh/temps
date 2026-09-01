// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backfill `domains.verification_method` values produced by the live issuance
//! path before ADR-041 §7a step (b) landed.
//!
//! `generate_certificate_from_order` hardcoded `verification_method: "acme"`
//! on every certificate it returned, and `save_certificate`'s upsert included
//! `VerificationMethod` in its `update_columns`, so every provision/renewal
//! overwrote a correctly-set `http-01` row with `"acme"`. The renewal
//! dispatcher only understood `"http-01"` and `"dns-01"`, so those rows were
//! silently never renewed.
//!
//! This migration maps the aliases produced by that bug to their correct
//! renewal-dispatcher values:
//! - `"acme"` → `"http-01"` (the challenge type for every non-DNS issuance)
//! - `"http"` → `"http-01"` (a second alias that appears in some older rows)
//! - `"manual"` is left alone — the dispatcher already routes it to
//!   `send_manual_renewal_notification`, and changing it to `"dns-01"` would
//!   be wrong for certificates that are not DNS-01.
//!
//! After this migration, step (c) of ADR-041 §7a can safely make a truly
//! unrecognized value produce a Critical alarm, because on a healthy instance
//! no unrecognized value will remain.
//!
//! See ADR-041 §7a for full design rationale.

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
-- Map "acme" and "http" to "http-01" so the renewal scheduler can dispatch them.
-- "manual" is intentionally left alone: it routes to send_manual_renewal_notification
-- which is the correct actionable path for certificates that need manual renewal.
UPDATE domains
   SET verification_method = 'http-01',
       updated_at = NOW()
 WHERE verification_method IN ('acme', 'http');
"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Intentionally a no-op: the old "acme"/"http" values were bugs, not
        // canonical data. Re-introducing them would re-break renewal dispatch.
        // Roll forward with a corrective migration instead of rolling back.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn migration_maps_acme_and_http_aliases() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("backfill migration should succeed");

        let log = format!("{:?}", db.into_transaction_log());

        assert!(
            log.contains("verification_method = 'http-01'"),
            "must rewrite to the canonical renewal-dispatcher value"
        );
        assert!(
            log.contains("verification_method IN ('acme', 'http')"),
            "must target both known aliases"
        );
        // "manual" must not be touched: it produces send_manual_renewal_notification,
        // not TlsRenewalFailed. Changing it to dns-01 would be wrong.
        assert!(
            !log.contains("'manual'"),
            "manual rows must not be rewritten — the dispatcher already routes them correctly"
        );
    }

    #[tokio::test]
    async fn migration_down_is_a_noop() {
        // Re-introducing "acme"/"http" would re-break renewal dispatch, so
        // rollback is deliberately a no-op.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("no-op rollback should succeed");
    }
}
