use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Give SNS delivery its own bounded idempotency key and make provider-message
/// correlation indexed. The event's provider_message_id remains the SES ID;
/// replay identity is a separate concern.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE email_events
                    ADD COLUMN IF NOT EXISTS idempotency_key VARCHAR(64) NULL;

                ALTER TABLE email_providers
                    ADD COLUMN IF NOT EXISTS sns_topic_arn TEXT NULL;

                -- When the SNS HTTPS subscription for sns_topic_arn was last
                -- confirmed. NULL means "never confirmed for the current
                -- topic" — the setup UI uses this to distinguish a pending
                -- subscription (endpoint subscribed before the topic was
                -- authorized) from a working pipeline.
                ALTER TABLE email_providers
                    ADD COLUMN IF NOT EXISTS sns_subscription_confirmed_at TIMESTAMPTZ NULL;

                DROP INDEX IF EXISTS idx_email_events_provider_msg_id;
                CREATE INDEX IF NOT EXISTS idx_email_events_provider_message_id
                    ON email_events (provider_message_id)
                    WHERE provider_message_id IS NOT NULL;

                CREATE UNIQUE INDEX IF NOT EXISTS idx_email_events_idempotency_key
                    ON email_events (idempotency_key)
                    WHERE idempotency_key IS NOT NULL;

                CREATE INDEX IF NOT EXISTS idx_emails_provider_message_id
                    ON emails (provider_message_id)
                    WHERE provider_message_id IS NOT NULL;

                -- #296 may already have installed the original global,
                -- nullable suppression schema. Applied migrations never rerun,
                -- so upgrade it here instead of relying on edits to that file.
                DROP INDEX IF EXISTS idx_suppressed_recipients_email;
                DROP INDEX IF EXISTS idx_suppressed_recipients_domain_email;

                -- Preserve the old global safety behavior without retaining
                -- unowned rows: expand each legacy global suppression to every
                -- domain that exists at upgrade time. Existing scoped rows win.
                INSERT INTO suppressed_recipients (
                    email, reason, domain_id, detail, created_at
                )
                SELECT legacy.email, legacy.reason, domain.id,
                       legacy.detail, legacy.created_at
                FROM suppressed_recipients AS legacy
                CROSS JOIN email_domains AS domain
                WHERE legacy.domain_id IS NULL
                  AND NOT EXISTS (
                      SELECT 1
                      FROM suppressed_recipients AS scoped
                      WHERE scoped.domain_id = domain.id
                        AND lower(trim(scoped.email)) = lower(trim(legacy.email))
                  );
                DELETE FROM suppressed_recipients WHERE domain_id IS NULL;
                ALTER TABLE suppressed_recipients
                    ALTER COLUMN domain_id SET NOT NULL;
                ALTER TABLE suppressed_recipients
                    DROP CONSTRAINT IF EXISTS suppressed_recipients_domain_id_fkey;
                ALTER TABLE suppressed_recipients
                    ADD CONSTRAINT suppressed_recipients_domain_id_fkey
                    FOREIGN KEY (domain_id) REFERENCES email_domains(id) ON DELETE CASCADE;
                CREATE UNIQUE INDEX idx_suppressed_recipients_domain_email
                    ON suppressed_recipients (domain_id, email);
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
                DROP INDEX IF EXISTS idx_emails_provider_message_id;
                DROP INDEX IF EXISTS idx_email_events_provider_message_id;
                DROP INDEX IF EXISTS idx_email_events_idempotency_key;

                -- The legacy schema allowed only one event per SES message.
                -- Preserve every event row, but clear the correlation value on
                -- all but the first row so its unique index can be restored.
                WITH ranked_events AS (
                    SELECT id,
                           row_number() OVER (
                               PARTITION BY provider_message_id ORDER BY id
                           ) AS occurrence
                    FROM email_events
                    WHERE provider_message_id IS NOT NULL
                )
                UPDATE email_events AS event
                SET provider_message_id = NULL
                FROM ranked_events AS ranked
                WHERE event.id = ranked.id AND ranked.occurrence > 1;

                ALTER TABLE email_events DROP COLUMN IF EXISTS idempotency_key;
                ALTER TABLE email_providers DROP COLUMN IF EXISTS sns_topic_arn;
                ALTER TABLE email_providers
                    DROP COLUMN IF EXISTS sns_subscription_confirmed_at;

                DROP INDEX IF EXISTS idx_suppressed_recipients_domain_email;

                -- The legacy schema had global suppression uniqueness. Keep
                -- the oldest scoped row deterministically when rolling back.
                WITH ranked_suppressions AS (
                    SELECT id,
                           row_number() OVER (
                               PARTITION BY email ORDER BY created_at, id
                           ) AS occurrence
                    FROM suppressed_recipients
                )
                DELETE FROM suppressed_recipients AS suppression
                USING ranked_suppressions AS ranked
                WHERE suppression.id = ranked.id AND ranked.occurrence > 1;

                ALTER TABLE suppressed_recipients
                    ALTER COLUMN domain_id DROP NOT NULL;
                ALTER TABLE suppressed_recipients
                    DROP CONSTRAINT IF EXISTS suppressed_recipients_domain_id_fkey;
                ALTER TABLE suppressed_recipients
                    ADD CONSTRAINT suppressed_recipients_domain_id_fkey
                    FOREIGN KEY (domain_id) REFERENCES email_domains(id) ON DELETE SET NULL;
                CREATE UNIQUE INDEX idx_suppressed_recipients_email
                    ON suppressed_recipients (email);

                CREATE UNIQUE INDEX idx_email_events_provider_msg_id
                    ON email_events (provider_message_id)
                    WHERE provider_message_id IS NOT NULL;
                "#,
            )
            .await?;
        Ok(())
    }
}
