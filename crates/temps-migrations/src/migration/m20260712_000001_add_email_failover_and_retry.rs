//! Multi-provider failover + retry/circuit-breaker support for the email
//! send path. Previously a domain had exactly one provider (`email_domains.
//! provider_id`) and any send failure went straight to "captured" — a
//! transient SES throttle or a Scaleway blip permanently dropped the email
//! instead of trying again or falling back to a backup provider.
//!
//! - `email_domain_fallback_providers`: ordered backup providers tried, in
//!   `priority` order, after the domain's primary provider is exhausted.
//! - `emails.provider_id` / `emails.retry_count`: which provider actually
//!   attempted the send and how many attempts were made across the chain,
//!   so the UI can show why a send eventually succeeded or was captured.
//! - `email_providers.rate_limit_per_minute`: optional per-provider cap
//!   enforced on the send path (NULL = unlimited), operator-configurable
//!   per provider rather than a global env var.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000001_add_email_failover_and_retry"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE emails
                    ADD COLUMN IF NOT EXISTS provider_id INTEGER REFERENCES email_providers(id) ON DELETE SET NULL,
                    ADD COLUMN IF NOT EXISTS retry_count INTEGER NOT NULL DEFAULT 0;

                ALTER TABLE email_providers
                    ADD COLUMN IF NOT EXISTS rate_limit_per_minute INTEGER;

                CREATE TABLE IF NOT EXISTS email_domain_fallback_providers (
                    id SERIAL PRIMARY KEY,
                    domain_id INTEGER NOT NULL REFERENCES email_domains(id) ON DELETE CASCADE,
                    provider_id INTEGER NOT NULL REFERENCES email_providers(id) ON DELETE CASCADE,
                    priority INTEGER NOT NULL DEFAULT 0,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    UNIQUE (domain_id, provider_id)
                );

                CREATE INDEX IF NOT EXISTS idx_email_domain_fallback_providers_domain
                    ON email_domain_fallback_providers (domain_id, priority);
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
                DROP TABLE IF EXISTS email_domain_fallback_providers;
                ALTER TABLE email_providers DROP COLUMN IF EXISTS rate_limit_per_minute;
                ALTER TABLE emails
                    DROP COLUMN IF EXISTS retry_count,
                    DROP COLUMN IF EXISTS provider_id;
                "#,
            )
            .await?;
        Ok(())
    }
}
