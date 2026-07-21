//! Keep audit history when a user account is deleted.
//!
//! `audit_logs.user_id` previously referenced `users(id)` with `ON DELETE
//! CASCADE`, so removing a user silently removed every audit row they ever
//! produced. Audit records are the system's history of record and must
//! outlive the accounts they describe, so the reference now becomes NULL on
//! user deletion instead. The serialized `data` payload on each row already
//! carries the original actor context, so no information is lost.
//!
//! Retention policy: the `data` payload keeps whatever actor/target details
//! (username, name, email) the operation recorded, even after the account is
//! deleted — that is intentional, since audit rows are the history of record.
//! Operators who need stricter erasure guarantees must scrub `data` fields
//! themselves as a separate step; deleting the account does not do it.

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
                ALTER TABLE audit_logs ALTER COLUMN user_id DROP NOT NULL;
                ALTER TABLE audit_logs DROP CONSTRAINT IF EXISTS fk_audit_logs_user_id;
                ALTER TABLE audit_logs
                    ADD CONSTRAINT fk_audit_logs_user_id
                    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Restoring NOT NULL + CASCADE requires removing rows whose user was
        // deleted while SET NULL was in effect — they cannot satisfy either
        // constraint. The explicit transaction keeps the destructive DELETE
        // from committing if any of the DDL that follows it fails.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                BEGIN;
                DELETE FROM audit_logs WHERE user_id IS NULL;
                ALTER TABLE audit_logs ALTER COLUMN user_id SET NOT NULL;
                ALTER TABLE audit_logs DROP CONSTRAINT IF EXISTS fk_audit_logs_user_id;
                ALTER TABLE audit_logs
                    ADD CONSTRAINT fk_audit_logs_user_id
                    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
                COMMIT;
                "#,
            )
            .await?;
        Ok(())
    }
}
