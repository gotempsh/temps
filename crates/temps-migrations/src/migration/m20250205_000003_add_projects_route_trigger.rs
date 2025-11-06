use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Create a function that checks if routing-relevant fields changed
        // Only notifies if fields that actually affect routing are modified
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_project_route_change()
                RETURNS TRIGGER AS $$
                BEGIN
                    -- For INSERT and DELETE, always notify (new project or removed from routing)
                    IF TG_OP IN ('INSERT', 'DELETE') THEN
                        PERFORM pg_notify('project_route_change', json_build_object(
                            'action', TG_OP,
                            'project_id', COALESCE(NEW.id, OLD.id),
                            'field', 'project'
                        )::text);
                        RETURN COALESCE(NEW, OLD);
                    END IF;

                    -- For UPDATE, only notify if routing-relevant fields changed
                    -- Routing-relevant fields:
                    --   - is_deleted: Project must be removed from routing when deleted
                    --   - slug: Used in environment preview domain routing (e.g., preview-{slug}.temps.dev)
                    -- NOT routing-relevant: attack_mode, deployment_config, preset_config, git_url, etc.
                    IF TG_OP = 'UPDATE' THEN
                        IF (OLD.is_deleted IS DISTINCT FROM NEW.is_deleted)
                           OR (OLD.slug IS DISTINCT FROM NEW.slug)
                        THEN
                            PERFORM pg_notify('project_route_change', json_build_object(
                                'action', 'UPDATE',
                                'project_id', NEW.id,
                                'is_deleted', NEW.is_deleted,
                                'slug', NEW.slug,
                                'timestamp', CURRENT_TIMESTAMP
                            )::text);
                        END IF;
                        RETURN NEW;
                    END IF;

                    RETURN COALESCE(NEW, OLD);
                END;
                $$ LANGUAGE plpgsql;
                "#,
        )
        .await?;

        // Create row-level trigger that fires for each affected row
        // This ensures granular control - notifications only when needed
        db.execute_unprepared(
            r#"
                CREATE TRIGGER projects_route_change_trigger
                AFTER INSERT OR UPDATE OR DELETE ON projects
                FOR EACH ROW
                EXECUTE FUNCTION notify_project_route_change();
                "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
                DROP TRIGGER IF EXISTS projects_route_change_trigger ON projects;
                DROP FUNCTION IF EXISTS notify_project_route_change();
                "#,
        )
        .await?;

        Ok(())
    }
}
