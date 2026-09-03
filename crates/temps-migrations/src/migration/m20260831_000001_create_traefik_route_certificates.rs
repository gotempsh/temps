// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable per-host TLS authorization records for Traefik-discovered routes.
//!
//! Separate table from `traefik_discovered_routes` because those rows are
//! deleted when the container stops — authorization to hold a publicly-trusted
//! certificate must survive container replacement. Keyed on `host` (the thing
//! the certificate is actually about), not on the container's lifetime.
//!
//! No trigger: nothing here feeds the route table, so changes must **not**
//! fire `notify_route_table_change()`. Certificate changes reach the proxy
//! through the cert-loader cache path (ADR-017), not a route reload.
//!
//! See ADR-041 §2 for full design rationale.

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
CREATE TABLE traefik_route_certificates (
    id SERIAL PRIMARY KEY,

    -- Normalized (lowercased) hostname. The natural key for the authorization:
    -- one host, one authorization record, surviving container churn.
    host VARCHAR NOT NULL,

    -- Whether the operator has explicitly authorized TLS for this host.
    -- `enabled` answers "route HTTP traffic"; `cert_authorized` answers "the
    -- operator accepts responsibility for a cert". Separate, deliberately.
    cert_authorized BOOLEAN NOT NULL DEFAULT FALSE,

    -- When/who granted the authorization.
    authorized_at TIMESTAMPTZ,
    authorized_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,

    -- Which discovery network the authorization was granted against. Repointing
    -- TEMPS_TRAEFIK_DISCOVERY_NETWORK makes new operations reject; existing
    -- certs keep serving (the cert loader ignores this table).
    authorized_network VARCHAR NOT NULL DEFAULT '',

    -- Container identity at authorization time. Divergence from the currently-
    -- serving container is a HIGH-severity finding (ADR-041 §2a), surfaced as
    -- a Critical alarm and a distinct API/console state. NOT auto-cleared:
    -- clearing does not remove the certificate, and auto-clear is a DoS
    -- primitive against legitimate renewals.
    authorized_container_id   VARCHAR NOT NULL DEFAULT '',
    authorized_container_name VARCHAR NOT NULL DEFAULT '',

    -- Set when the current serving container no longer matches the authorized
    -- identity. Cleared only by explicit operator re-authorization.
    container_drift_detected_at TIMESTAMPTZ,

    -- The container ID of the last container whose drift was already alarmed.
    -- Without this, the drift alarm re-fires every reconcile pass for the same
    -- already-alarmed container. Deduplicate by comparing against this column.
    last_drift_alarmed_container_id VARCHAR,

    -- Renewal method. CHECK-constrained to the two values the renewal scheduler
    -- understands (tls/service.rs:490). A third value would produce a cert that
    -- is never renewed; the constraint makes that unrepresentable.
    renewal_method VARCHAR NOT NULL DEFAULT 'http-01'
        CONSTRAINT traefik_route_certificates_renewal_method_check
        CHECK (renewal_method IN ('http-01', 'dns-01')),

    -- How the certificate was obtained: operator-initiated ACME or import.
    source VARCHAR NOT NULL DEFAULT 'acme'
        CONSTRAINT traefik_route_certificates_source_check
        CHECK (source IN ('acme', 'imported')),

    -- FK to the resulting domains row. NULL while issuance is in flight.
    certificate_id INTEGER REFERENCES domains(id) ON DELETE SET NULL,

    -- Timestamp of the last successful import (Path B).
    imported_at TIMESTAMPTZ,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT traefik_route_certificates_host_unique UNIQUE (host)
);

-- Drift-check query: for all cert-authorized hosts, join to
-- traefik_discovered_routes and compare container identities.
CREATE INDEX idx_traefik_route_certs_cert_authorized
    ON traefik_route_certificates (cert_authorized)
    WHERE cert_authorized = TRUE;

-- FK-to-domains lookup for status/expiry queries.
CREATE INDEX idx_traefik_route_certs_certificate_id
    ON traefik_route_certificates (certificate_id);

-- Host lookup for single-host API operations.
CREATE INDEX idx_traefik_route_certs_host
    ON traefik_route_certificates (host);
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
DROP TABLE IF EXISTS traefik_route_certificates;
"#,
            )
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn migration_creates_table_with_check_constraints() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("traefik_route_certificates migration should succeed");

        let log = format!("{:?}", db.into_transaction_log());

        assert!(
            log.contains("traefik_route_certificates_host_unique UNIQUE (host)"),
            "host must be unique — one hostname, one authorization record"
        );
        assert!(
            log.contains("renewal_method IN ('http-01', 'dns-01')"),
            "renewal_method must be CHECK-constrained to the two values the renewal dispatcher \
             understands; a third value would produce a cert that is never renewed"
        );
        assert!(
            log.contains("source IN ('acme', 'imported')"),
            "source must be constrained to the two supported paths"
        );
        assert!(
            log.contains("certificate_id INTEGER REFERENCES domains(id) ON DELETE SET NULL"),
            "certificate_id must FK to domains so UI can show expiry/status"
        );
        assert!(
            log.contains("authorized_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL"),
            "authorization must record who granted it"
        );
        assert!(
            log.contains("authorized_container_id"),
            "container identity at authorization time must be captured (ADR-041 §2a)"
        );
        assert!(
            log.contains("last_drift_alarmed_container_id"),
            "drift deduplication column must be present to avoid re-firing the alarm \
             every reconcile pass for the same already-alarmed container"
        );
        assert!(
            !log.contains("notify_route_table_change"),
            "this table must NOT have a route-table trigger: cert changes reach the \
             proxy through the cert-loader cache path, not a route reload"
        );
    }

    #[tokio::test]
    async fn migration_down_drops_table() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("traefik_route_certificates rollback should succeed");

        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("DROP TABLE IF EXISTS traefik_route_certificates"));
    }
}
