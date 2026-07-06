use sea_orm_migration::prelude::*;

/// Adds `default_backup_provisioned` to `external_services`.
///
/// Backs the auto-provisioning reconcile loop that gives MariaDB services a
/// covering daily full-backup schedule (base backups via the
/// `mariadb_physical` engine) so point-in-time recovery works out of the box
/// once a default S3 source is configured.
///
/// The flag is a one-shot latch: the reconcile loop only provisions services
/// where it is `false`, and sets it to `true` after creating the schedule.
/// This guarantees we provision exactly once and never recreate a schedule the
/// operator later deletes.
///
/// Defaults to `false` so every existing row is considered "not yet
/// provisioned" on upgrade — the reconcile loop will create their schedules on
/// the next tick. **Safely re-runnable:** uses an `IF NOT EXISTS` column guard
/// inside a PL/pgSQL block.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'external_services'
          AND column_name = 'default_backup_provisioned'
    ) THEN
        ALTER TABLE external_services
            ADD COLUMN default_backup_provisioned BOOL NOT NULL DEFAULT false;
    END IF;
END $$;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE external_services
    DROP COLUMN IF EXISTS default_backup_provisioned;
            "#,
        )
        .await?;

        Ok(())
    }
}
