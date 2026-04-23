use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Backfill `owner` for GitLab repositories whose `full_name` contains
        // a nested group path. Prior to this migration the GitLab provider
        // stored only the first path segment in `owner`, so a project like
        // `gala-games/chain/platform/operation-api-next` was saved with
        // `owner = 'gala-games'` and `name = 'operation-api-next'`, which
        // breaks `owner/name` lookups.
        //
        // After: `owner` = everything in `full_name` before the last `/`.
        // Rows where `owner || '/' || name = full_name` are already correct
        // and left untouched.
        let conn = manager.get_connection();
        let backend = manager.get_database_backend();
        let stmt = sea_orm::Statement::from_string(
            backend,
            r#"
            UPDATE repositories
            SET owner = substring(full_name FROM 1 FOR length(full_name) - length(name) - 1)
            WHERE position('/' IN full_name) > 0
              AND full_name LIKE '%/' || name
              AND owner <> substring(full_name FROM 1 FOR length(full_name) - length(name) - 1)
            "#
            .to_owned(),
        );
        conn.execute(stmt).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No automatic downgrade: we cannot unambiguously restore the old
        // (broken) value without losing information. Leaving data as-is on
        // rollback is the safe choice.
        Ok(())
    }
}
