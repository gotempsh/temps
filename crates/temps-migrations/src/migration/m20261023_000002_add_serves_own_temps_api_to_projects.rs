// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project opt-in to receive Temps' own public paths.
//!
//! Adds `projects.serves_own_temps_api` (default false). The proxy sends
//! `/api/_temps*` and `/api/otel*` to this instance's console on every
//! hostname, because every deployed app's analytics SDK and OTLP exporter
//! post to their own domain. A project that is itself a Temps console or API
//! serves those paths, so with the flag on the proxy hands them to the
//! project's upstream instead.
//!
//! The proxy reads the flag from the route table's cached project rows, so
//! changing it must reload routes: `notify_project_route_change()` is
//! redefined to notify when this column changes too. Otherwise identical to
//! the function `m20250205_000003_add_projects_route_trigger` created.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `notify_project_route_change()`, with `extra_update_condition` OR-ed into
/// the list of routing-relevant columns whose change notifies on UPDATE.
fn notify_function_sql(extra_update_condition: &str) -> String {
    format!(
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
            IF TG_OP = 'UPDATE' THEN
                IF (OLD.is_deleted IS DISTINCT FROM NEW.is_deleted)
                   OR (OLD.slug IS DISTINCT FROM NEW.slug)
                   OR (OLD.deployment_config IS DISTINCT FROM NEW.deployment_config){extra_update_condition}
                THEN
                    PERFORM pg_notify('project_route_change', json_build_object(
                        'action', 'UPDATE',
                        'project_id', NEW.id,
                        'is_deleted', NEW.is_deleted,
                        'slug', NEW.slug,
                        'deployment_config_changed', (OLD.deployment_config IS DISTINCT FROM NEW.deployment_config),
                        'timestamp', CURRENT_TIMESTAMP
                    )::text);
                END IF;
                RETURN NEW;
            END IF;

            RETURN COALESCE(NEW, OLD);
        END;
        $$ LANGUAGE plpgsql;
        "#
    )
}

const SERVES_OWN_TEMPS_API_CHANGED: &str =
    "\n                   OR (OLD.serves_own_temps_api IS DISTINCT FROM NEW.serves_own_temps_api)";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::ServesOwnTempsApi)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&notify_function_sql(SERVES_OWN_TEMPS_API_CHANGED))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Restore the function first: it must not reference the column once
        // the column is gone.
        manager
            .get_connection()
            .execute_unprepared(&notify_function_sql(""))
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::ServesOwnTempsApi)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    ServesOwnTempsApi,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_notifies_a_route_reload_only_after_up() {
        let up = notify_function_sql(SERVES_OWN_TEMPS_API_CHANGED);
        assert!(up.contains("OLD.serves_own_temps_api IS DISTINCT FROM NEW.serves_own_temps_api"));
        // The pre-existing triggers are kept.
        for column in ["is_deleted", "slug", "deployment_config"] {
            assert!(up.contains(&format!("OLD.{column} IS DISTINCT FROM NEW.{column}")));
        }
        assert!(!notify_function_sql("").contains("serves_own_temps_api"));
    }
}
