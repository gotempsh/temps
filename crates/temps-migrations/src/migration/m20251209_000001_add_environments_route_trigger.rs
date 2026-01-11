use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop the old statement-level trigger that fires on all environment changes
        // This trigger was too broad and sent empty payloads
        db.execute_unprepared(
            r#"
                DROP TRIGGER IF EXISTS environment_changes_trigger ON environments;
                "#,
        )
        .await?;

        // Create a function that notifies when current_deployment_id changes
        // This ensures the proxy learns about new deployments and reloads routes
        // Uses the same 'project_route_change' channel as project triggers for consistency
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_environment_route_change()
                RETURNS TRIGGER AS $$
                BEGIN
                    -- Only notify if current_deployment_id changed
                    -- This field determines which deployment is receiving traffic
                    IF TG_OP = 'UPDATE' THEN
                        IF (OLD.current_deployment_id IS DISTINCT FROM NEW.current_deployment_id) THEN
                            PERFORM pg_notify('project_route_change', json_build_object(
                                'action', 'ENVIRONMENT_UPDATE',
                                'environment_id', NEW.id,
                                'project_id', NEW.project_id,
                                'deployment_id', NEW.current_deployment_id,
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

        // Create row-level trigger on environments table
        // Fires after UPDATE to detect current_deployment_id changes
        db.execute_unprepared(
            r#"
                CREATE TRIGGER environments_route_change_trigger
                AFTER UPDATE ON environments
                FOR EACH ROW
                EXECUTE FUNCTION notify_environment_route_change();
                "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop the new row-level trigger
        db.execute_unprepared(
            r#"
                DROP TRIGGER IF EXISTS environments_route_change_trigger ON environments;
                DROP FUNCTION IF EXISTS notify_environment_route_change();
                "#,
        )
        .await?;

        // Restore the old statement-level trigger
        db.execute_unprepared(
            r#"
                CREATE TRIGGER environment_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON environments
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        Ok(())
    }
}
