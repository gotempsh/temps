//! Suppression list: recipients who must not receive further email due to a
//! hard bounce, a spam complaint, or a manual admin action. Without this,
//! nothing stopped a permanently-bad or complained address from being
//! emailed again on the next send, which is exactly the pattern that gets a
//! sending domain's reputation downgraded by receiving mail providers.
//! Enforced per sending domain in `EmailService::send`. A recipient bouncing
//! for one tenant/domain must never block another tenant from contacting it.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000002_create_suppressed_recipients"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE IF NOT EXISTS suppressed_recipients (
                    id SERIAL PRIMARY KEY,
                    email TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    domain_id INTEGER NOT NULL REFERENCES email_domains(id) ON DELETE CASCADE,
                    detail TEXT,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
                );

                CREATE UNIQUE INDEX IF NOT EXISTS idx_suppressed_recipients_domain_email
                    ON suppressed_recipients (domain_id, email);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS suppressed_recipients;")
            .await?;
        Ok(())
    }
}
