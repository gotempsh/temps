// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attribute the audit events written before vault audit attribution existed.

use crate::error::{SchemaError, SchemaResult};
use sea_orm::DatabaseConnection;
pub use temps_migrations::VaultAuditBackfillSummary;

/// Attribute pre-migration audit events to their vaults, resuming a previous
/// run. Safe on every startup and a no-op once complete. Until it completes,
/// vault audit pages keep the read-time enumeration, so no event is hidden
/// meanwhile; run it detached rather than before serving.
pub async fn run_vault_audit_backfill(
    db: &DatabaseConnection,
) -> SchemaResult<VaultAuditBackfillSummary> {
    temps_migrations::backfill_vault_audit_attribution(db)
        .await
        .map_err(|source| SchemaError::VaultAuditBackfill { source })
}
