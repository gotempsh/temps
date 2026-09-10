// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Replace the environment route change trigger function to also fire
        // when deployment_config changes (e.g. password protection updates).
        // Previously it only fired when current_deployment_id changed, which
        // meant settings updates (password protection, security headers, etc.)
        // were invisible to the proxy until the next deployment.
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_environment_route_change()
                RETURNS TRIGGER AS $$
                BEGIN
                    IF TG_OP = 'UPDATE' THEN
                        -- Notify when current_deployment_id changes (new deployment goes live)
                        -- OR when deployment_config changes (settings update e.g. password protection)
                        IF (OLD.current_deployment_id IS DISTINCT FROM NEW.current_deployment_id)
                           OR (OLD.deployment_config IS DISTINCT FROM NEW.deployment_config) THEN
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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Restore original trigger that only watches current_deployment_id
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_environment_route_change()
                RETURNS TRIGGER AS $$
                BEGIN
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

        Ok(())
    }
}
