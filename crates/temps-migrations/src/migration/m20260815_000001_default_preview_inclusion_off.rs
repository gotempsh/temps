//! Make preview-environment secret propagation opt-in for existing databases.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Preserve the exact rows changed by this migration so `down()` can
        // restore prior explicit/default-true behavior without changing rows
        // that users opt into after the upgrade.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE _temps_m20260815_preview_optout_backup (\
                    env_var_id INTEGER PRIMARY KEY REFERENCES env_vars(id) ON DELETE CASCADE\
                 )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO _temps_m20260815_preview_optout_backup (env_var_id) \
                 SELECT id FROM env_vars WHERE include_in_preview = TRUE",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE env_vars SET include_in_preview = FALSE \
                 WHERE include_in_preview = TRUE",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE env_vars ALTER COLUMN include_in_preview SET DEFAULT FALSE",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE _temps_m20260815_preview_optout_secret_backup (\
                    secret_id INTEGER PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE\
                 )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO _temps_m20260815_preview_optout_secret_backup (secret_id) \
                 SELECT id FROM secrets WHERE include_in_preview = TRUE",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE secrets SET include_in_preview = FALSE \
                 WHERE include_in_preview = TRUE",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE secrets AS secret \
                 SET include_in_preview = TRUE \
                 FROM _temps_m20260815_preview_optout_secret_backup AS backup \
                 WHERE secret.id = backup.secret_id",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE _temps_m20260815_preview_optout_secret_backup")
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE env_vars ALTER COLUMN include_in_preview SET DEFAULT TRUE",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE env_vars AS env \
                 SET include_in_preview = TRUE \
                 FROM _temps_m20260815_preview_optout_backup AS backup \
                 WHERE env.id = backup.env_var_id",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE _temps_m20260815_preview_optout_backup")
            .await?;
        Ok(())
    }
}
