// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Released builds retained the original pending/ready status after
        // issuance when DNS cleanup failed. Require a saved ACME certificate
        // matching the domain, issued after this order was created: an active
        // domain alone also describes a genuinely unfinished renewal. The
        // preceding certificate-settings migration repairs legacy immediate
        // issuance at the feature boundary using the old status invariant;
        // this repair remains safe for orders created under the new behavior.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            UPDATE acme_orders AS orders
            SET status = 'valid'
            FROM domains AS domain
            WHERE orders.domain_id = domain.id
              AND orders.status IN ('pending', 'ready', 'processing')
              AND jsonb_typeof(orders.authorizations->'temps_dns_cleanup') = 'object'
              AND EXISTS (
                  SELECT 1 FROM tls_acme_certificates AS cert
                  WHERE cert.domain = domain.domain
                    AND cert.certificate = domain.certificate
                    AND cert.issued_at >= orders.created_at
              )
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This repairs persisted state; reverting it would re-open issued orders.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn backfill_completed_dns_cleanup_orders() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping cleanup order migration test");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping cleanup order migration test");
            return Ok(());
        }
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "migration-test")
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:migration-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared(r#"
            CREATE TABLE domains (id INTEGER PRIMARY KEY, domain TEXT, certificate TEXT,
                private_key TEXT, status TEXT);
            CREATE TABLE tls_acme_certificates (domain TEXT, certificate TEXT, issued_at TIMESTAMPTZ);
            CREATE TABLE acme_orders (id INTEGER PRIMARY KEY, domain_id INTEGER, status TEXT,
                authorizations JSONB, created_at TIMESTAMPTZ);
            INSERT INTO domains VALUES
                (1, 'app.example.test', 'current-cert', 'key', 'challenge_requested'),
                -- Legacy immediate issuance: no history row, but serving a cert.
                (2, 'immediate.example.test', 'immediate-cert', 'key', 'active'),
                -- A legacy unfinished renewal is challenge_requested, even
                -- with the old certificate still present.
                (3, 'unfinished.example.test', 'old-cert', 'key', 'challenge_requested');
            INSERT INTO tls_acme_certificates VALUES
                ('app.example.test', 'current-cert', '2026-10-20T12:00:00Z'),
                ('app.example.test', 'different-cert', '2026-10-22T12:00:00Z');
            INSERT INTO acme_orders VALUES
                -- Legacy issued order with failed DNS cleanup.
                (1, 1, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z'),
                (2, 1, 'ready', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z'),
                -- Active domain, but this renewal was created after its certificate.
                -- A newer *different* certificate must not count as its completion.
                (3, 1, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-21T11:00:00Z'),
                -- Orders without receipts or already terminal are unchanged.
                (4, 1, 'pending', '{}', '2026-10-20T11:00:00Z'),
                (5, 1, 'invalid', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z'),
                (6, 1, 'pending', '{"temps_dns_cleanup":null}', '2026-10-20T11:00:00Z'),
                (7, 99, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z');
        "#).await?;
        db.execute_unprepared(
            r#"
            INSERT INTO acme_orders VALUES
                (8, 2, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z'),
                (9, 3, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-20T11:00:00Z');
        "#,
        )
        .await?;
        let manager = SchemaManager::new(&db);
        super::super::m20261028_000001_domain_certificate_settings::Migration
            .up(&manager)
            .await?;
        // Once the feature is enabled, an active domain may have a genuinely
        // pending renewal. The later repair must not use the old status rule.
        db.execute_unprepared(
            r#"
            INSERT INTO acme_orders VALUES
                (10, 2, 'pending', '{"temps_dns_cleanup":{"records":[]}}', '2026-10-21T11:00:00Z');
        "#,
        )
        .await?;
        Migration.up(&manager).await?;
        // Reapplying the repair cannot affect a genuinely pending renewal.
        Migration.up(&manager).await?;
        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT status FROM acme_orders ORDER BY id".to_string(),
            ))
            .await?;
        let statuses = rows
            .iter()
            .map(|row| row.try_get::<String>("", "status"))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            statuses,
            [
                "valid", "valid", "pending", "pending", "invalid", "pending", "pending", "valid",
                "pending", "pending"
            ]
        );
        Ok(())
    }
}
