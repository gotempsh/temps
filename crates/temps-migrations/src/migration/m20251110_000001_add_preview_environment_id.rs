use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Add preview_environment_id column to projects table
        db.execute_unprepared(
            r#"
                ALTER TABLE projects
                ADD COLUMN preview_environment_id INTEGER NULL
            "#,
        )
        .await?;

        // Add foreign key constraint for preview_environment_id
        db.execute_unprepared(
            r#"
                ALTER TABLE projects
                ADD CONSTRAINT fk_projects_preview_environment
                FOREIGN KEY (preview_environment_id)
                REFERENCES environments(id)
                ON DELETE SET NULL
                ON UPDATE CASCADE
            "#,
        )
        .await?;

        // Create index for preview_environment_id for query performance
        db.execute_unprepared(
            r#"
                CREATE INDEX idx_projects_preview_environment
                ON projects(preview_environment_id)
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop the index
        db.execute_unprepared(
            r#"
                DROP INDEX IF EXISTS idx_projects_preview_environment
            "#,
        )
        .await?;

        // Drop the foreign key
        db.execute_unprepared(
            r#"
                ALTER TABLE projects
                DROP CONSTRAINT IF EXISTS fk_projects_preview_environment
            "#,
        )
        .await?;

        // Drop the column
        db.execute_unprepared(
            r#"
                ALTER TABLE projects
                DROP COLUMN IF EXISTS preview_environment_id
            "#,
        )
        .await?;

        Ok(())
    }
}
