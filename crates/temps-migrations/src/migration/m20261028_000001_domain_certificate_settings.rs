// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
            CREATE TABLE domain_certificate_settings (
                domain_id INTEGER PRIMARY KEY REFERENCES domains(id) ON DELETE CASCADE,
                challenge_type TEXT NOT NULL CHECK (challenge_type IN ('http-01', 'dns-01')),
                renewal_days INTEGER NOT NULL DEFAULT 30 CHECK (renewal_days BETWEEN 1 AND 90),
                alert_days INTEGER CHECK (alert_days BETWEEN 1 AND 90),
                alerts_enabled BOOLEAN
            );

            -- This runs at the feature boundary, BEFORE new code can preserve
            -- active status during an unfinished renewal. Released builds set
            -- challenge_requested on every open order, including renewals.
            -- An active row with a receipt therefore already finished issuance,
            -- even when cached authorization issued immediately and wrote no
            -- tls_acme_certificates history row or last_renewed timestamp.
            UPDATE acme_orders AS orders
            SET status = 'valid'
            FROM domains AS domain
            WHERE orders.domain_id = domain.id
              AND orders.status IN ('pending', 'ready', 'processing')
              AND jsonb_typeof(orders.authorizations->'temps_dns_cleanup') = 'object'
              AND domain.status = 'active'
              AND domain.certificate IS NOT NULL
              AND domain.private_key IS NOT NULL;
        "#,
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE domain_certificate_settings")
            .await?;
        Ok(())
    }
}
