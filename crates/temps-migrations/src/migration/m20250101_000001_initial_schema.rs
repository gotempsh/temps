use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

/// Simple SeaORM migration demonstrating table creation and relationships
/// This focuses on core entities: users, projects, environments, deployments
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            // Enable pgvector extension if not already enabled
            manager
                .get_connection()
                .execute_unprepared("CREATE EXTENSION IF NOT EXISTS vector")
                .await?;
        }
        // Create users table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("users"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("email")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("mfa_secret")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("mfa_enabled"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("mfa_recovery_codes"))
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_encrypted"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("password_hash")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("email_verified"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("email_verification_token"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("email_verification_expires"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("password_reset_token"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("password_reset_expires"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create projects table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("projects"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("repo_name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("repo_owner")).string().not_null())
                    .col(ColumnDef::new(Alias::new("directory")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("main_branch"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("preset")).string().not_null())
                    .col(ColumnDef::new(Alias::new("preset_config")).json_binary().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_config"))
                            .json_binary()
                            .null()
                            .comment("Deployment configuration including CPU, memory, ports, and feature flags"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("slug"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_deleted"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_deployment"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_public_repo"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("git_url")).string().null()) // For public repos not managed by user
                    .col(
                        ColumnDef::new(Alias::new("git_provider_connection_id"))
                            .integer()
                            .null(),
                    ) // Optional - null for public repos
                    .to_owned(),
            )
            .await?;

        // Create environments table with foreign key to projects
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("environments"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("slug")).string().not_null())
                    .col(ColumnDef::new(Alias::new("subdomain")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("last_deployment"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("host")).string().not_null())
                    .col(ColumnDef::new(Alias::new("upstreams")).json().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("current_deployment_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("branch")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_config"))
                            .json_binary()
                            .null()
                            .comment("Environment-specific deployment configuration (overrides project defaults)"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("payment_provider_live_mode"))
                            .boolean()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("use_default_wildcard"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(Alias::new("custom_domain")).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_environments_project_id")
                            .from(Alias::new("environments"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // Note: foreign key to deployments will be added after deployments table is created
                    .to_owned(),
            )
            .await?;

        // Create deployments table with foreign keys to projects and environments
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("deployments"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("slug")).string().not_null())
                    .col(ColumnDef::new(Alias::new("state")).string().not_null())
                    .col(ColumnDef::new(Alias::new("metadata")).json().not_null())
                    .col(
                        ColumnDef::new(Alias::new("deploying_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ready_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("static_dir_location"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("screenshot_location"))
                            .string()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("container_id")).text().null())
                    .col(ColumnDef::new(Alias::new("container_name")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("container_port"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("image_name")).string().null())
                    .col(ColumnDef::new(Alias::new("cpu_request")).integer().null())
                    .col(ColumnDef::new(Alias::new("cpu_limit")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("memory_request"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("memory_limit")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("finished_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("context_vars")).json().null())
                    .col(ColumnDef::new(Alias::new("branch_ref")).string().null())
                    .col(ColumnDef::new(Alias::new("tag_ref")).string().null())
                    .col(ColumnDef::new(Alias::new("commit_sha")).string().null())
                    .col(ColumnDef::new(Alias::new("commit_message")).text().null())
                    .col(ColumnDef::new(Alias::new("commit_json")).json().null())
                    .col(ColumnDef::new(Alias::new("commit_author")).text().null())
                    .col(ColumnDef::new(Alias::new("cancelled_reason")).text().null())
                    .col(ColumnDef::new(Alias::new("pipeline_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("log_id")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_config"))
                            .json()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployments_project_id")
                            .from(Alias::new("deployments"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployments_environment_id")
                            .from(Alias::new("deployments"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Add foreign key from environments to deployments (circular dependency resolved)
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_environments_current_deployment_id")
                    .from(
                        Alias::new("environments"),
                        Alias::new("current_deployment_id"),
                    )
                    .to(Alias::new("deployments"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Create deployment_domains table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("deployment_domains"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("domain")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("is_calculated"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployment_domains_deployment_id")
                            .from(
                                Alias::new("deployment_domains"),
                                Alias::new("deployment_id"),
                            )
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create sessions table with foreign key to users
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("sessions"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("user_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("session_token"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_user_id")
                            .from(Alias::new("sessions"), Alias::new("user_id"))
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for better performance
        manager
            .create_index(
                Index::create()
                    .name("idx_users_email")
                    .table(Alias::new("users"))
                    .col(Alias::new("email"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_projects_slug")
                    .table(Alias::new("projects"))
                    .col(Alias::new("slug"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_environments_slug")
                    .table(Alias::new("environments"))
                    .col(Alias::new("slug"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_token")
                    .table(Alias::new("sessions"))
                    .col(Alias::new("session_token"))
                    .to_owned(),
            )
            .await?;

        // Create roles table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("roles"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create ip_geolocations table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("ip_geolocations"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ip_address"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Alias::new("latitude")).double().null())
                    .col(ColumnDef::new(Alias::new("longitude")).double().null())
                    .col(ColumnDef::new(Alias::new("region")).string().null())
                    .col(ColumnDef::new(Alias::new("city")).string().null())
                    .col(ColumnDef::new(Alias::new("country")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("country_code"))
                            .char_len(2)
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("timezone")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("is_eu"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("asn")).text().null())
                    .col(ColumnDef::new(Alias::new("isp")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("last_verified"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for ip_geolocations
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_ip_geolocations_country_code")
                    .table(Alias::new("ip_geolocations"))
                    .col(Alias::new("country_code"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_ip_geolocations_updated")
                    .table(Alias::new("ip_geolocations"))
                    .col(Alias::new("updated_at"))
                    .to_owned(),
            )
            .await?;

        // Create notification_preferences table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("notification_preferences"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("preferences")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create notifications table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("notifications"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("notification_id"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("title")).string().not_null())
                    .col(ColumnDef::new(Alias::new("message")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("notification_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("priority")).string().not_null())
                    .col(ColumnDef::new(Alias::new("metadata")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("sent_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("batch_key")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("occurrence_count"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("next_allowed_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_read"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("read_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create notification_providers table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("notification_providers"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("provider_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("config")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("enabled"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create visitor table (must be created before tables that reference it)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("visitor"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("visitor_id")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("first_seen"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_seen"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("user_agent")).string().null())
                    .col(ColumnDef::new(Alias::new("ip_address_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("is_crawler"))
                            .boolean()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("crawler_name")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("custom_data")).text().null())
                    .to_owned(),
            )
            .await?;

        // Create session_replay_sessions table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_replay_sessions"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("session_replay_id"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("visitor_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("user_agent")).text().null())
                    .col(ColumnDef::new(Alias::new("browser")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("browser_version"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("operating_system"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("operating_system_version"))
                            .string()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("device_type")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("viewport_width"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("viewport_height"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("screen_width")).integer().null())
                    .col(ColumnDef::new(Alias::new("screen_height")).integer().null())
                    .col(ColumnDef::new(Alias::new("language")).string().null())
                    .col(ColumnDef::new(Alias::new("timezone")).string().null())
                    .col(ColumnDef::new(Alias::new("url")).text().null())
                    .col(ColumnDef::new(Alias::new("duration")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_sessions_visitor_id")
                            .from(
                                Alias::new("session_replay_sessions"),
                                Alias::new("visitor_id"),
                            )
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_sessions_project_id")
                            .from(
                                Alias::new("session_replay_sessions"),
                                Alias::new("project_id"),
                            )
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_sessions_environment_id")
                            .from(
                                Alias::new("session_replay_sessions"),
                                Alias::new("environment_id"),
                            )
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_sessions_deployment_id")
                            .from(
                                Alias::new("session_replay_sessions"),
                                Alias::new("deployment_id"),
                            )
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create session_replay_events table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_replay_events"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .integer()
                            .not_null(),
                    ) // Changed from string to integer
                    .col(ColumnDef::new(Alias::new("data")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("type")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_events_session_id")
                            .from(
                                Alias::new("session_replay_events"),
                                Alias::new("session_id"),
                            )
                            .to(Alias::new("session_replay_sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for session replay tables
        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_visitor_id")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("visitor_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_project_id")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_environment_id")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("environment_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_deployment_id")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("deployment_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_session_replay_id")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("session_replay_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_sessions_created_at")
                    .table(Alias::new("session_replay_sessions"))
                    .col(Alias::new("created_at"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_events_session_id")
                    .table(Alias::new("session_replay_events"))
                    .col(Alias::new("session_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_events_timestamp")
                    .table(Alias::new("session_replay_events"))
                    .col(Alias::new("timestamp"))
                    .to_owned(),
            )
            .await?;

        // Create request_sessions table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("request_sessions"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_accessed_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ip_address")).string().null())
                    .col(ColumnDef::new(Alias::new("user_agent")).string().null())
                    .col(ColumnDef::new(Alias::new("referrer")).string().null())
                    .col(ColumnDef::new(Alias::new("data")).string().not_null())
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_sessions_visitor_id")
                            .from(Alias::new("request_sessions"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create performance_metrics table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("performance_metrics"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ttfb")).float().null())
                    .col(ColumnDef::new(Alias::new("lcp")).float().null())
                    .col(ColumnDef::new(Alias::new("fid")).float().null())
                    .col(ColumnDef::new(Alias::new("fcp")).float().null())
                    .col(ColumnDef::new(Alias::new("cls")).float().null())
                    .col(ColumnDef::new(Alias::new("inp")).float().null())
                    .col(
                        ColumnDef::new(Alias::new("recorded_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ip_address_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("session_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("is_crawler"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    // Request context columns
                    .col(
                        ColumnDef::new(Alias::new("pathname"))
                            .string_len(2048)
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("query")).text().null())
                    .col(ColumnDef::new(Alias::new("host")).string_len(255).null())
                    // Browser information columns
                    .col(ColumnDef::new(Alias::new("browser")).string_len(100).null())
                    .col(
                        ColumnDef::new(Alias::new("browser_version"))
                            .string_len(50)
                            .null(),
                    )
                    // Operating system columns
                    .col(
                        ColumnDef::new(Alias::new("operating_system"))
                            .string_len(100)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("operating_system_version"))
                            .string_len(50)
                            .null(),
                    )
                    // Device information columns
                    .col(
                        ColumnDef::new(Alias::new("device_type"))
                            .string_len(50)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("screen_width"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("screen_height"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("viewport_width"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("viewport_height"))
                            .small_integer()
                            .null(),
                    )
                    // Language column
                    .col(ColumnDef::new(Alias::new("language")).string_len(10).null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_performance_metrics_project_id")
                            .from(Alias::new("performance_metrics"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_performance_metrics_environment_id")
                            .from(
                                Alias::new("performance_metrics"),
                                Alias::new("environment_id"),
                            )
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_performance_metrics_deployment_id")
                            .from(
                                Alias::new("performance_metrics"),
                                Alias::new("deployment_id"),
                            )
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_performance_metrics_session_id")
                            .from(Alias::new("performance_metrics"), Alias::new("session_id"))
                            .to(Alias::new("request_sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_performance_metrics_visitor_id")
                            .from(Alias::new("performance_metrics"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for performance_metrics commonly filtered columns
        manager
            .create_index(
                Index::create()
                    .name("idx_performance_metrics_pathname")
                    .table(Alias::new("performance_metrics"))
                    .col(Alias::new("pathname"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_performance_metrics_browser")
                    .table(Alias::new("performance_metrics"))
                    .col(Alias::new("browser"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_performance_metrics_device_type")
                    .table(Alias::new("performance_metrics"))
                    .col(Alias::new("device_type"))
                    .to_owned(),
            )
            .await?;

        // Create domains table (needed for project_custom_domains and custom_routes)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("domains"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("domain"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Alias::new("certificate")).text().null())
                    .col(ColumnDef::new(Alias::new("private_key")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("expiration_time"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_renewed"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("status")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("dns_challenge_token"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("dns_challenge_value"))
                            .string()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("last_error")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("last_error_type"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_wildcard"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("verification_method"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("http_challenge_token"))
                            .string()
                            .unique_key()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("http_challenge_key_authorization"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create project_custom_domains table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_custom_domains"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("domain")).string().not_null())
                    .col(ColumnDef::new(Alias::new("redirect_to")).string().null())
                    .col(ColumnDef::new(Alias::new("status_code")).integer().null())
                    .col(ColumnDef::new(Alias::new("branch")).string().null())
                    .col(ColumnDef::new(Alias::new("status")).string().not_null())
                    .col(ColumnDef::new(Alias::new("message")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("certificate_id"))
                            .integer()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_custom_domains_project_id")
                            .from(
                                Alias::new("project_custom_domains"),
                                Alias::new("project_id"),
                            )
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_custom_domains_environment_id")
                            .from(
                                Alias::new("project_custom_domains"),
                                Alias::new("environment_id"),
                            )
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_custom_domains_certificate_id")
                            .from(
                                Alias::new("project_custom_domains"),
                                Alias::new("certificate_id"),
                            )
                            .to(Alias::new("domains"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create external_services table (needed for external_service_backups and project_services)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("external_services"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("service_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("version")).string().null())
                    .col(ColumnDef::new(Alias::new("status")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("slug")).string().null())
                    .col(ColumnDef::new(Alias::new("config")).text().null())
                    .to_owned(),
            )
            .await?;

        // Create s3_sources table (needed for backups)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("s3_sources"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("bucket_name"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("region")).string().not_null())
                    .col(ColumnDef::new(Alias::new("endpoint")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("bucket_path"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("access_key_id"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("secret_key")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("force_path_style"))
                            .boolean()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create backups table (needed for external_service_backups)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("backups"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("backup_id")).string().not_null())
                    .col(ColumnDef::new(Alias::new("schedule_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("backup_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("state")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("finished_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("size_bytes")).integer().null())
                    .col(ColumnDef::new(Alias::new("file_count")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("s3_source_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("s3_location"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("error_message")).string().null())
                    .col(ColumnDef::new(Alias::new("metadata")).string().not_null())
                    .col(ColumnDef::new(Alias::new("checksum")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("compression_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("created_by")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_encrypted"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("tags")).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backups_s3_source_id")
                            .from(Alias::new("backups"), Alias::new("s3_source_id"))
                            .to(Alias::new("s3_sources"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backups_created_by")
                            .from(Alias::new("backups"), Alias::new("created_by"))
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create external_service_backups table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("external_service_backups"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("service_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("backup_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("backup_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("state")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("finished_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("size_bytes")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("s3_location"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("error_message")).string().null())
                    .col(ColumnDef::new(Alias::new("metadata")).json().not_null())
                    .col(ColumnDef::new(Alias::new("checksum")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("compression_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("created_by")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_encrypted"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_external_service_backups_service_id")
                            .from(
                                Alias::new("external_service_backups"),
                                Alias::new("service_id"),
                            )
                            .to(Alias::new("external_services"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_external_service_backups_backup_id")
                            .from(
                                Alias::new("external_service_backups"),
                                Alias::new("backup_id"),
                            )
                            .to(Alias::new("backups"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_external_service_backups_created_by")
                            .from(
                                Alias::new("external_service_backups"),
                                Alias::new("created_by"),
                            )
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create audit_logs table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("audit_logs"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("user_id")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("user_agent")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("operation_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("ip_address_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("audit_date"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("data")).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_audit_logs_user_id")
                            .from(Alias::new("audit_logs"), Alias::new("user_id"))
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create git_providers table for multi-provider support
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("git_providers"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("provider_type"))
                            .string()
                            .not_null(),
                    ) // github, gitlab, bitbucket, gitea, generic
                    .col(ColumnDef::new(Alias::new("base_url")).string().null()) // For self-hosted instances (web UI URL)
                    .col(ColumnDef::new(Alias::new("api_url")).string().null()) // API endpoint URL (different from base_url for GitHub Apps)
                    .col(
                        ColumnDef::new(Alias::new("auth_method"))
                            .string()
                            .not_null(),
                    ) // app, oauth, pat, basic, ssh
                    .col(ColumnDef::new(Alias::new("auth_config")).json().not_null()) // JSON with provider-specific auth config
                    .col(ColumnDef::new(Alias::new("webhook_secret")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_default"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create git_provider_connections table (replaces github_app_installations)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("git_provider_connections"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("provider_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("user_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("account_name"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("account_type"))
                            .string()
                            .not_null(),
                    ) // User, Organization
                    .col(ColumnDef::new(Alias::new("access_token")).text().null())
                    .col(ColumnDef::new(Alias::new("refresh_token")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("token_expires_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("refresh_token_expires_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("installation_id"))
                            .string()
                            .null(),
                    ) // Provider-specific installation ID
                    .col(ColumnDef::new(Alias::new("metadata")).json().null()) // Provider-specific metadata
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("syncing"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_synced_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_git_provider_connections_provider_id")
                            .from(
                                Alias::new("git_provider_connections"),
                                Alias::new("provider_id"),
                            )
                            .to(Alias::new("git_providers"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_git_provider_connections_user_id")
                            .from(
                                Alias::new("git_provider_connections"),
                                Alias::new("user_id"),
                            )
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create user_roles table (junction table for users and roles)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("user_roles"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("user_id")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("role_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_roles_user_id")
                            .from(Alias::new("user_roles"), Alias::new("user_id"))
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_roles_role_id")
                            .from(Alias::new("user_roles"), Alias::new("role_id"))
                            .to(Alias::new("roles"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create tls_acme_certificates table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("tls_acme_certificates"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("domain")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("certificate"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("private_key"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("issued_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create request_logs table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("request_logs"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("date")).string().not_null())
                    .col(ColumnDef::new(Alias::new("host")).string().not_null())
                    .col(ColumnDef::new(Alias::new("method")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("request_path"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("message")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("status_code"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("branch")).string().null())
                    .col(ColumnDef::new(Alias::new("commit")).string().null())
                    .col(ColumnDef::new(Alias::new("request_id")).string().not_null())
                    .col(ColumnDef::new(Alias::new("level")).string().not_null())
                    .col(ColumnDef::new(Alias::new("user_agent")).string().not_null())
                    .col(ColumnDef::new(Alias::new("started_at")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("finished_at"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("elapsed_time")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("is_static_file"))
                            .boolean()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("referrer")).string().null())
                    .col(ColumnDef::new(Alias::new("ip_address")).string().null())
                    .col(ColumnDef::new(Alias::new("session_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("headers")).string().null())
                    .col(ColumnDef::new(Alias::new("ip_address_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("browser")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("browser_version"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("operating_system"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_mobile"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_entry_page"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_crawler"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("crawler_name")).string().null())
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("request_headers")).text().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_logs_project_id")
                            .from(Alias::new("request_logs"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_logs_environment_id")
                            .from(Alias::new("request_logs"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_logs_deployment_id")
                            .from(Alias::new("request_logs"), Alias::new("deployment_id"))
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_logs_session_id")
                            .from(Alias::new("request_logs"), Alias::new("session_id"))
                            .to(Alias::new("request_sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_request_logs_visitor_id")
                            .from(Alias::new("request_logs"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create repositories table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("repositories"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("git_provider_connection_id"))
                            .integer()
                            .not_null(),
                    ) // Foreign key to git_provider_connections - repositories are always linked to a connection
                    .col(ColumnDef::new(Alias::new("owner")).string().not_null())
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("full_name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("description")).string().null())
                    .col(ColumnDef::new(Alias::new("private")).boolean().not_null())
                    .col(ColumnDef::new(Alias::new("fork")).boolean().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("pushed_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("size")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("stargazers_count"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("watchers_count"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("language")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("default_branch"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("open_issues_count"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("topics")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("repo_object"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("installation_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("clone_url")).string().null()) // HTTPS clone URL
                    .col(ColumnDef::new(Alias::new("ssh_url")).string().null()) // SSH clone URL
                    .col(ColumnDef::new(Alias::new("preset")).json_binary().null()) // Preset cache (JSON array for monorepo support)
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_repositories_git_provider_connection_id")
                            .from(
                                Alias::new("repositories"),
                                Alias::new("git_provider_connection_id"),
                            )
                            .to(Alias::new("git_provider_connections"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create project_services table (junction table for projects and external_services)
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_services"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("service_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_services_project_id")
                            .from(Alias::new("project_services"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_services_service_id")
                            .from(Alias::new("project_services"), Alias::new("service_id"))
                            .to(Alias::new("external_services"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create custom_routes table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("custom_routes"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("domain")).string().not_null())
                    .col(ColumnDef::new(Alias::new("host")).string().not_null())
                    .col(ColumnDef::new(Alias::new("port")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("domain_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("enabled"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_custom_routes_domain_id")
                            .from(Alias::new("custom_routes"), Alias::new("domain_id"))
                            .to(Alias::new("domains"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create deployment_containers table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("deployment_containers"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("container_id"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("container_name"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("container_port"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployed_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ready_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployment_containers_deployment_id")
                            .from(
                                Alias::new("deployment_containers"),
                                Alias::new("deployment_id"),
                            )
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create crons table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("crons"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("path")).string().not_null())
                    .col(ColumnDef::new(Alias::new("schedule")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("next_run"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_crons_project_id")
                            .from(Alias::new("crons"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_crons_environment_id")
                            .from(Alias::new("crons"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create cron_executions table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("cron_executions"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("cron_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("executed_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("url")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("status_code"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("headers")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("response_time_ms"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("error_message")).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_cron_executions_cron_id")
                            .from(Alias::new("cron_executions"), Alias::new("cron_id"))
                            .to(Alias::new("crons"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create backup_schedules table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("backup_schedules"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("backup_type"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("retention_period"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("s3_source_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("schedule_expression"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("enabled"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_run"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("next_run"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("description")).string().null())
                    .col(ColumnDef::new(Alias::new("tags")).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_schedules_s3_source_id")
                            .from(Alias::new("backup_schedules"), Alias::new("s3_source_id"))
                            .to(Alias::new("s3_sources"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create acme_accounts table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("acme_accounts"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("email")).string().not_null())
                    .col(ColumnDef::new(Alias::new("url")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("environment"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("account_data"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create env_vars table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("env_vars"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("key")).string().not_null())
                    .col(ColumnDef::new(Alias::new("value")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_env_vars_project_id")
                            .from(Alias::new("env_vars"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create env_var_environments junction table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("env_var_environments"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("env_var_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_env_var_environments_env_var_id")
                            .from(Alias::new("env_var_environments"), Alias::new("env_var_id"))
                            .to(Alias::new("env_vars"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_env_var_environments_environment_id")
                            .from(
                                Alias::new("env_var_environments"),
                                Alias::new("environment_id"),
                            )
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create environment_domains table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("environment_domains"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("domain")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_environment_domains_environment_id")
                            .from(
                                Alias::new("environment_domains"),
                                Alias::new("environment_id"),
                            )
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create api_keys table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("api_keys"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("key_hash"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Alias::new("key_prefix")).string().not_null())
                    .col(ColumnDef::new(Alias::new("user_id")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("role_type")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(Alias::new("expires_at")).timestamp_with_time_zone())
                    .col(ColumnDef::new(Alias::new("last_used_at")).timestamp_with_time_zone())
                    .col(ColumnDef::new(Alias::new("permissions")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-api_keys-user_id")
                            .from(Alias::new("api_keys"), Alias::new("user_id"))
                            .to(Alias::new("users"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create magic_link_tokens table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("magic_link_tokens"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("email")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("token"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("used"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create settings table with single row and JSON data column
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("settings"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .default(1)
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("data")).json().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create funnels table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("funnels"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("description")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_funnels_project_id")
                            .from(Alias::new("funnels"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create funnel_steps table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("funnel_steps"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("funnel_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("step_order"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("event_name")).string().not_null())
                    .col(ColumnDef::new(Alias::new("event_filter")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_funnel_steps_funnel_id")
                            .from(Alias::new("funnel_steps"), Alias::new("funnel_id"))
                            .to(Alias::new("funnels"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Add api_endpoint and api_enabled columns to deployments table
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("api_endpoint"))
                            .string()
                            .null()
                            .unique_key(), // Unique API endpoint for each deployment
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("api_enabled"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Create events table for unified analytics
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("events"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Session tracking
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer().null())
                    // Page data
                    .col(ColumnDef::new(Alias::new("hostname")).text().not_null())
                    .col(ColumnDef::new(Alias::new("pathname")).text().not_null())
                    .col(ColumnDef::new(Alias::new("page_path")).text().not_null()) // For analytics grouping
                    .col(ColumnDef::new(Alias::new("href")).text().not_null())
                    .col(ColumnDef::new(Alias::new("querystring")).text().null())
                    .col(ColumnDef::new(Alias::new("page_title")).text().null())
                    .col(ColumnDef::new(Alias::new("referrer")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("referrer_hostname"))
                            .text()
                            .null(),
                    )
                    // Session flow tracking
                    .col(
                        ColumnDef::new(Alias::new("is_entry"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_exit"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_bounce"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("time_on_page")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("session_page_number"))
                            .integer()
                            .null(),
                    )
                    // User interaction metrics
                    .col(ColumnDef::new(Alias::new("scroll_depth")).integer().null())
                    .col(ColumnDef::new(Alias::new("clicks")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("custom_properties"))
                            .json()
                            .null(),
                    )
                    // Performance metrics
                    .col(ColumnDef::new(Alias::new("lcp")).float().null())
                    .col(ColumnDef::new(Alias::new("cls")).float().null())
                    .col(ColumnDef::new(Alias::new("inp")).float().null())
                    .col(ColumnDef::new(Alias::new("fcp")).float().null())
                    .col(ColumnDef::new(Alias::new("ttfb")).float().null())
                    .col(ColumnDef::new(Alias::new("fid")).float().null())
                    // Device/Browser
                    .col(ColumnDef::new(Alias::new("browser")).text().null())
                    .col(ColumnDef::new(Alias::new("browser_version")).text().null())
                    .col(ColumnDef::new(Alias::new("operating_system")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("operating_system_version"))
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("device_type")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("screen_width"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("screen_height"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("viewport_width"))
                            .small_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("viewport_height"))
                            .small_integer()
                            .null(),
                    )
                    // Geography (cached)
                    .col(
                        ColumnDef::new(Alias::new("ip_geolocation_id"))
                            .integer()
                            .null(),
                    )
                    // Traffic source
                    .col(ColumnDef::new(Alias::new("channel")).text().null())
                    .col(ColumnDef::new(Alias::new("utm_source")).text().null())
                    .col(ColumnDef::new(Alias::new("utm_medium")).text().null())
                    .col(ColumnDef::new(Alias::new("utm_campaign")).text().null())
                    .col(ColumnDef::new(Alias::new("utm_term")).text().null())
                    .col(ColumnDef::new(Alias::new("utm_content")).text().null())
                    // Event details
                    .col(
                        ColumnDef::new(Alias::new("event_type"))
                            .text()
                            .not_null()
                            .default("pageview"),
                    )
                    .col(ColumnDef::new(Alias::new("event_name")).text().null())
                    .col(ColumnDef::new(Alias::new("props")).json_binary().null())
                    // Analytics compatibility fields
                    .col(ColumnDef::new(Alias::new("event_data")).text().null())
                    .col(ColumnDef::new(Alias::new("request_query")).text().null())
                    // Metadata
                    .col(ColumnDef::new(Alias::new("user_agent")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("is_crawler"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Alias::new("crawler_name")).text().null())
                    .col(ColumnDef::new(Alias::new("language")).text().null())
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("timestamp")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_events_project_id")
                            .from(Alias::new("events"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_events_environment_id")
                            .from(Alias::new("events"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_events_deployment_id")
                            .from(Alias::new("events"), Alias::new("deployment_id"))
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_events_visitor_id")
                            .from(Alias::new("events"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_events_ip_geolocation_id")
                            .from(Alias::new("events"), Alias::new("ip_geolocation_id"))
                            .to(Alias::new("ip_geolocations"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for events table
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_events_project_time")
                    .table(Alias::new("events"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("timestamp"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_events_session_time")
                    .table(Alias::new("events"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("timestamp"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_events_href")
                    .table(Alias::new("events"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("href"))
                    .col(Alias::new("timestamp"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_events_session_flow")
                    .table(Alias::new("events"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("session_page_number"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_events_geo")
                    .table(Alias::new("events"))
                    .col(Alias::new("ip_geolocation_id"))
                    .to_owned(),
            )
            .await?;

        // Configure TimescaleDB for time-series tables
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            // Configure TimescaleDB for deployment_metrics and events
            let sql = r#"
                -- Configure TimescaleDB for events table with id segmenting (space partitioning)
                SELECT create_hypertable('events', 'timestamp',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);

                -- Create indexes for events
                CREATE INDEX IF NOT EXISTS idx_events_project_timestamp
                    ON events (project_id, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_events_session_timestamp
                    ON events (session_id, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_events_page_timestamp
                    ON events (page_path, timestamp DESC);

                -- Enable compression for events after 7 days
                ALTER TABLE events SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,event_type',
                    timescaledb.compress_orderby = 'timestamp DESC'
                );

                SELECT add_compression_policy('events', INTERVAL '7 days', if_not_exists => TRUE);

                -- Data retention for events (keep for 90 days)
                SELECT add_retention_policy('events', INTERVAL '90 days', if_not_exists => TRUE);
            "#;

            manager
                .get_connection()
                .execute_unprepared(sql)
                .await
                .map_err(|e| DbErr::Custom(format!("Failed to configure TimescaleDB: {}", e)))?;
        }

        // Create error_groups table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("error_groups"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("title")).string().not_null())
                    .col(ColumnDef::new(Alias::new("error_type")).string().not_null())
                    .col(ColumnDef::new(Alias::new("message_template")).string())
                    .col(ColumnDef::new(Alias::new("embedding")).custom(Alias::new("vector(384)")))
                    .col(
                        ColumnDef::new(Alias::new("first_seen"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_seen"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("total_count"))
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status"))
                            .string()
                            .not_null()
                            .default("unresolved"),
                    )
                    .col(ColumnDef::new(Alias::new("assigned_to")).string())
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("environment_id")).integer())
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer())
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_groups_project_id")
                            .from(Alias::new("error_groups"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_groups_environment_id")
                            .from(Alias::new("error_groups"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_groups_deployment_id")
                            .from(Alias::new("error_groups"), Alias::new("deployment_id"))
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_groups_visitor_id")
                            .from(Alias::new("error_groups"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .to_owned(),
            )
            .await?;

        // Create error_events table (simplified with single data JSONB column)
        // NOTE: Uses composite primary key (id, timestamp) for TimescaleDB hypertable compatibility
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("error_events"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment(),
                    )
                    // Foreign Keys (ACID - referential integrity)
                    .col(
                        ColumnDef::new(Alias::new("error_group_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("environment_id")).integer())
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer())
                    .col(ColumnDef::new(Alias::new("visitor_id")).integer())
                    .col(ColumnDef::new(Alias::new("ip_geolocation_id")).integer())
                    .col(ColumnDef::new(Alias::new("source")).text().not_null())
                    // Indexed fields (ACID - fast queries)
                    .col(
                        ColumnDef::new(Alias::new("fingerprint_hash"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    // Composite primary key for TimescaleDB hypertable support
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("timestamp")),
                    )
                    // Core error data (frequently displayed)
                    .col(
                        ColumnDef::new(Alias::new("exception_type"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("exception_value")).text())
                    // ALL STRUCTURED DATA IN ONE JSONB COLUMN
                    // Contains: user, device, request, stack_trace, environment, trace contexts
                    .col(ColumnDef::new(Alias::new("data")).json_binary())
                    // Metadata
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_error_group_id")
                            .from(Alias::new("error_events"), Alias::new("error_group_id"))
                            .to(Alias::new("error_groups"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_project_id")
                            .from(Alias::new("error_events"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_environment_id")
                            .from(Alias::new("error_events"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_deployment_id")
                            .from(Alias::new("error_events"), Alias::new("deployment_id"))
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_visitor_id")
                            .from(Alias::new("error_events"), Alias::new("visitor_id"))
                            .to(Alias::new("visitor"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_error_events_ip_geolocation_id")
                            .from(Alias::new("error_events"), Alias::new("ip_geolocation_id"))
                            .to(Alias::new("ip_geolocations"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::NoAction),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for performance

        // Index for error groups
        manager
            .create_index(
                Index::create()
                    .name("idx_error_groups_project_id")
                    .table(Alias::new("error_groups"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_error_groups_status")
                    .table(Alias::new("error_groups"))
                    .col(Alias::new("status"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_error_groups_last_seen")
                    .table(Alias::new("error_groups"))
                    .col(Alias::new("last_seen"))
                    .to_owned(),
            )
            .await?;

        // Vector similarity index for error groups
        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX IF NOT EXISTS idx_error_groups_embedding ON error_groups USING ivfflat (embedding vector_cosine_ops)")
            .await?;

        // Indexes for error events
        manager
            .create_index(
                Index::create()
                    .name("idx_error_events_error_group_id")
                    .table(Alias::new("error_events"))
                    .col(Alias::new("error_group_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_error_events_fingerprint_hash")
                    .table(Alias::new("error_events"))
                    .col(Alias::new("fingerprint_hash"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_error_events_project_id")
                    .table(Alias::new("error_events"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_error_events_timestamp")
                    .table(Alias::new("error_events"))
                    .col(Alias::new("timestamp"))
                    .to_owned(),
            )
            .await?;

        // Convert error_events to TimescaleDB hypertable for time-series optimization
        // This enables efficient time-range queries, compression, and continuous aggregates
        if manager.get_database_backend() == sea_orm::DatabaseBackend::Postgres {
            // 1. Convert to hypertable with 1-day chunks and id segmenting (space partitioning)
            let create_hypertable_sql = r#"
                SELECT create_hypertable(
                    'error_events',
                    'timestamp',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE,
                    migrate_data => TRUE
                );
            "#;

            manager
                .get_connection()
                .execute_unprepared(create_hypertable_sql)
                .await?;

            // 2. Drop existing simple indexes and create time-series optimized composite indexes
            manager
                .get_connection()
                .execute_unprepared("DROP INDEX IF EXISTS idx_error_events_timestamp")
                .await?;

            manager
                .get_connection()
                .execute_unprepared("DROP INDEX IF EXISTS idx_error_events_project_id")
                .await?;

            let create_timeseries_indexes_sql = r#"
                -- Composite index for project + time-range queries (most common pattern)
                CREATE INDEX IF NOT EXISTS idx_error_events_project_timestamp
                    ON error_events (project_id, timestamp DESC);

                -- Composite index for error group + time-range queries
                CREATE INDEX IF NOT EXISTS idx_error_events_group_timestamp
                    ON error_events (error_group_id, timestamp DESC);

                -- Composite index for project + environment + time queries
                CREATE INDEX IF NOT EXISTS idx_error_events_project_env_timestamp
                    ON error_events (project_id, environment_id, timestamp DESC)
                    WHERE environment_id IS NOT NULL;

                -- Index for fingerprint lookups with time ordering (used in error grouping)
                CREATE INDEX IF NOT EXISTS idx_error_events_fingerprint_timestamp
                    ON error_events (fingerprint_hash, timestamp DESC);
            "#;

            manager
                .get_connection()
                .execute_unprepared(create_timeseries_indexes_sql)
                .await?;

            // 3. Enable compression for data older than 7 days
            let enable_compression_sql = r#"
                ALTER TABLE error_events SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,error_group_id,environment_id',
                    timescaledb.compress_orderby = 'timestamp DESC'
                );

                SELECT add_compression_policy('error_events', INTERVAL '7 days');
            "#;

            manager
                .get_connection()
                .execute_unprepared(enable_compression_sql)
                .await?;

            // 4. Create continuous aggregate for hourly error statistics
            let create_hourly_aggregate_sql = r#"
                CREATE MATERIALIZED VIEW error_events_hourly
                WITH (timescaledb.continuous) AS
                SELECT
                    time_bucket('1 hour', timestamp) AS bucket,
                    project_id,
                    environment_id,
                    error_group_id,
                    COUNT(*) as error_count,
                    COUNT(DISTINCT fingerprint_hash) as unique_fingerprints
                FROM error_events
                GROUP BY bucket, project_id, environment_id, error_group_id
                WITH NO DATA;

                CREATE INDEX IF NOT EXISTS idx_error_events_hourly_project_bucket
                    ON error_events_hourly (project_id, bucket DESC);

                SELECT add_continuous_aggregate_policy('error_events_hourly',
                    start_offset => INTERVAL '3 hours',
                    end_offset => INTERVAL '1 hour',
                    schedule_interval => INTERVAL '1 hour');
            "#;

            manager
                .get_connection()
                .execute_unprepared(create_hourly_aggregate_sql)
                .await?;

            // 5. Create continuous aggregate for daily error statistics
            let create_daily_aggregate_sql = r#"
                CREATE MATERIALIZED VIEW error_events_daily
                WITH (timescaledb.continuous) AS
                SELECT
                    time_bucket('1 day', timestamp) AS bucket,
                    project_id,
                    environment_id,
                    COUNT(*) as error_count,
                    COUNT(DISTINCT error_group_id) as unique_error_groups,
                    COUNT(DISTINCT fingerprint_hash) as unique_fingerprints
                FROM error_events
                GROUP BY bucket, project_id, environment_id
                WITH NO DATA;

                CREATE INDEX IF NOT EXISTS idx_error_events_daily_project_bucket
                    ON error_events_daily (project_id, bucket DESC);

                SELECT add_continuous_aggregate_policy('error_events_daily',
                    start_offset => INTERVAL '7 days',
                    end_offset => INTERVAL '1 day',
                    schedule_interval => INTERVAL '1 day');
            "#;

            manager
                .get_connection()
                .execute_unprepared(create_daily_aggregate_sql)
                .await?;
        }

        // Create project_dsns table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_dsns"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("environment_id")).integer())
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer())
                    .col(
                        ColumnDef::new(Alias::new("name"))
                            .string()
                            .not_null()
                            .default("Default DSN"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("public_key"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Alias::new("secret_key")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("rate_limit_per_minute"))
                            .integer()
                            .default(1000),
                    )
                    .col(ColumnDef::new(Alias::new("allowed_origins")).json())
                    .col(ColumnDef::new(Alias::new("last_used_at")).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(Alias::new("event_count"))
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT NOW()"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT NOW()"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_dsns_project")
                            .from(Alias::new("project_dsns"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_dsns_environment")
                            .from(Alias::new("project_dsns"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_dsns_deployment")
                            .from(Alias::new("project_dsns"), Alias::new("deployment_id"))
                            .to(Alias::new("deployments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for project_dsns
        manager
            .create_index(
                Index::create()
                    .name("idx_project_dsns_project_env_deploy")
                    .table(Alias::new("project_dsns"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("environment_id"))
                    .col(Alias::new("deployment_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_project_dsns_public_key")
                    .table(Alias::new("project_dsns"))
                    .col(Alias::new("public_key"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_project_dsns_is_active")
                    .table(Alias::new("project_dsns"))
                    .col(Alias::new("is_active"))
                    .to_owned(),
            )
            .await?;

        // Create deployment_jobs table
        manager
            .create_table(
                Table::create()
                    .table(DeploymentJobs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeploymentJobs::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DeploymentJobs::DeploymentId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DeploymentJobs::JobId).string().not_null()) // User-defined job identifier
                    .col(ColumnDef::new(DeploymentJobs::JobType).string().not_null()) // Job type (e.g., "DownloadRepoJob")
                    .col(ColumnDef::new(DeploymentJobs::Name).string().not_null())
                    .col(ColumnDef::new(DeploymentJobs::Description).text().null())
                    .col(
                        ColumnDef::new(DeploymentJobs::Status)
                            .integer()
                            .not_null()
                            .default(0),
                    ) // JobStatus enum: 0=Pending, 1=Waiting, 2=Running, 3=Success, 4=Failure, 5=Cancelled, 6=Skipped
                    .col(ColumnDef::new(DeploymentJobs::LogId).string().not_null()) // Log identifier for temps-logs service
                    .col(ColumnDef::new(DeploymentJobs::JobConfig).json().null()) // Job-specific configuration
                    .col(ColumnDef::new(DeploymentJobs::Outputs).json().null()) // Job outputs as key-value pairs
                    .col(ColumnDef::new(DeploymentJobs::Dependencies).json().null()) // List of job IDs this job depends on
                    .col(
                        ColumnDef::new(DeploymentJobs::ExecutionOrder)
                            .integer()
                            .null(),
                    ) // Calculated execution order
                    .col(
                        ColumnDef::new(DeploymentJobs::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentJobs::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(DeploymentJobs::ErrorMessage).text().null())
                    .col(
                        ColumnDef::new(DeploymentJobs::RetryCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DeploymentJobs::MaxRetries)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    .col(
                        ColumnDef::new(DeploymentJobs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeploymentJobs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_deployment_jobs_deployment_id")
                            .from(DeploymentJobs::Table, DeploymentJobs::DeploymentId)
                            .to(Deployments::Table, Deployments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes separately
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_jobs_deployment_id")
                    .table(DeploymentJobs::Table)
                    .col(DeploymentJobs::DeploymentId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_jobs_status")
                    .table(DeploymentJobs::Table)
                    .col(DeploymentJobs::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_jobs_job_id")
                    .table(DeploymentJobs::Table)
                    .col(DeploymentJobs::JobId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_jobs_execution_order")
                    .table(DeploymentJobs::Table)
                    .col(DeploymentJobs::ExecutionOrder)
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();

        // Create notification function that will be triggered on route table changes
        db.execute_unprepared(
            r#"
                CREATE OR REPLACE FUNCTION notify_route_table_change()
                RETURNS trigger AS $$
                BEGIN
                    PERFORM pg_notify('route_table_changes', '');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                "#,
        )
        .await?;

        // Create trigger on environment_domains table
        db.execute_unprepared(
            r#"
                CREATE TRIGGER environment_domains_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON environment_domains
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        // Create trigger on custom_routes table
        db.execute_unprepared(
            r#"
                CREATE TRIGGER custom_routes_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON custom_routes
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        // Create trigger on project_custom_domains table
        db.execute_unprepared(
            r#"
                CREATE TRIGGER project_custom_domains_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON project_custom_domains
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        // Create trigger on environments table (backend URLs are stored here)
        db.execute_unprepared(
            r#"
                CREATE TRIGGER environment_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON environments
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        // Create trigger on settings table (preview_domain is stored here)
        db.execute_unprepared(
            r#"
                CREATE TRIGGER settings_changes_trigger
                AFTER INSERT OR UPDATE OR DELETE ON settings
                FOR EACH STATEMENT
                EXECUTE FUNCTION notify_route_table_change();
                "#,
        )
        .await?;

        // Create index on event_type and event_name for web_vitals queries
        // This optimizes filtering performance metrics (event_type = 'web_vitals')
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_events_web_vitals
            ON events (project_id, event_type, timestamp DESC)
            WHERE event_type = 'web_vitals';
            "#,
        )
        .await?;

        // Create index for session-based performance metrics queries
        // Optimizes lookups when updating late-arriving metrics (CLS, INP)
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_events_web_vitals_session
            ON events (session_id, event_type, timestamp DESC)
            WHERE event_type = 'web_vitals' AND session_id IS NOT NULL;
            "#,
        )
        .await?;

        // Create composite index for environment-filtered web vitals queries
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_events_web_vitals_env
            ON events (project_id, environment_id, event_type, timestamp DESC)
            WHERE event_type = 'web_vitals' AND environment_id IS NOT NULL;
            "#,
        )
        .await?;

        // Create index for deployment-filtered web vitals queries
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_events_web_vitals_deployment
            ON events (project_id, deployment_id, event_type, timestamp DESC)
            WHERE event_type = 'web_vitals' AND deployment_id IS NOT NULL;
            "#,
        )
        .await?;

        // Create TimescaleDB hypertable index for time-based queries on performance metrics
        // This optimizes time-range queries for performance analytics
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_events_web_vitals_time_bucket
            ON events (project_id, time_bucket('1 hour', timestamp), event_type)
            WHERE event_type = 'web_vitals';
            "#,
        )
        .await?;

        // Create status_monitors table for monitoring and incident management
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("status_monitors"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("name"))
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("monitor_type"))
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("check_interval_seconds"))
                            .integer()
                            .not_null()
                            .default(60),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_monitors_project_id")
                            .from(Alias::new("status_monitors"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_monitors_environment_id")
                            .from(Alias::new("status_monitors"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for status_monitors
        manager
            .create_index(
                Index::create()
                    .name("idx_status_monitors_project_id")
                    .table(Alias::new("status_monitors"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_status_monitors_environment_id")
                    .table(Alias::new("status_monitors"))
                    .col(Alias::new("environment_id"))
                    .to_owned(),
            )
            .await?;

        // Create status_checks table with composite primary key for TimescaleDB partitioning
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("status_checks"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("monitor_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("status")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("response_time_ms"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("checked_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Alias::new("error_message")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .primary_key(
                        Index::create()
                            .name("pk_status_checks")
                            .col(Alias::new("id"))
                            .col(Alias::new("checked_at"))
                            .primary(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_checks_monitor_id")
                            .from(Alias::new("status_checks"), Alias::new("monitor_id"))
                            .to(Alias::new("status_monitors"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for status_checks
        manager
            .create_index(
                Index::create()
                    .name("idx_status_checks_monitor_id")
                    .table(Alias::new("status_checks"))
                    .col(Alias::new("monitor_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_status_checks_checked_at")
                    .table(Alias::new("status_checks"))
                    .col(Alias::new("checked_at"))
                    .to_owned(),
            )
            .await?;

        // Create status_incidents table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("status_incidents"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("monitor_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("title"))
                            .string_len(500)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("description")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("severity"))
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status"))
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("resolved_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_incidents_project_id")
                            .from(Alias::new("status_incidents"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_incidents_environment_id")
                            .from(Alias::new("status_incidents"), Alias::new("environment_id"))
                            .to(Alias::new("environments"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_incidents_monitor_id")
                            .from(Alias::new("status_incidents"), Alias::new("monitor_id"))
                            .to(Alias::new("status_monitors"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for status_incidents
        manager
            .create_index(
                Index::create()
                    .name("idx_status_incidents_project_id")
                    .table(Alias::new("status_incidents"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_status_incidents_environment_id")
                    .table(Alias::new("status_incidents"))
                    .col(Alias::new("environment_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_status_incidents_status")
                    .table(Alias::new("status_incidents"))
                    .col(Alias::new("status"))
                    .to_owned(),
            )
            .await?;

        // Create status_incident_updates table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("status_incident_updates"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("incident_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status"))
                            .string_len(50)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("message")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_status_incident_updates_incident_id")
                            .from(
                                Alias::new("status_incident_updates"),
                                Alias::new("incident_id"),
                            )
                            .to(Alias::new("status_incidents"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index for status_incident_updates
        manager
            .create_index(
                Index::create()
                    .name("idx_status_incident_updates_incident_id")
                    .table(Alias::new("status_incident_updates"))
                    .col(Alias::new("incident_id"))
                    .to_owned(),
            )
            .await?;

        // Convert status_checks to TimescaleDB hypertable with id segmenting (space partitioning)
        db.execute_unprepared(
            r#"
                SELECT create_hypertable('status_checks', 'checked_at',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);
                "#,
        )
        .await?;

        // Create composite index for efficient time-range queries
        db.execute_unprepared(
            r#"
                CREATE INDEX IF NOT EXISTS idx_status_checks_monitor_time
                    ON status_checks (monitor_id, checked_at DESC);
                "#,
        )
        .await?;

        // Enable compression after 30 days
        db.execute_unprepared(
            r#"
                ALTER TABLE status_checks SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'monitor_id,status',
                    timescaledb.compress_orderby = 'checked_at DESC'
                );
                "#,
        )
        .await?;

        db.execute_unprepared(
                r#"
                SELECT add_compression_policy('status_checks', INTERVAL '30 days', if_not_exists => TRUE);
                "#,
            )
            .await?;

        // Create continuous aggregate for 5-minute buckets
        db.execute_unprepared(
                r#"
                CREATE MATERIALIZED VIEW IF NOT EXISTS status_checks_5min
                WITH (timescaledb.continuous) AS
                SELECT
                    monitor_id,
                    time_bucket('5 minutes', checked_at) AS bucket,
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as operational_count,
                    COUNT(*) FILTER (WHERE status = 'degraded') as degraded_count,
                    COUNT(*) FILTER (WHERE status = 'down') as down_count,
                    AVG(response_time_ms) as avg_response_time_ms,
                    MIN(response_time_ms) as min_response_time_ms,
                    MAX(response_time_ms) as max_response_time_ms,
                    PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY response_time_ms) as p50_response_time_ms,
                    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY response_time_ms) as p95_response_time_ms,
                    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY response_time_ms) as p99_response_time_ms
                FROM status_checks
                GROUP BY monitor_id, bucket
                WITH NO DATA;
                "#,
            )
            .await?;

        // Create continuous aggregate for hourly buckets
        db.execute_unprepared(
                r#"
                CREATE MATERIALIZED VIEW IF NOT EXISTS status_checks_hourly
                WITH (timescaledb.continuous) AS
                SELECT
                    monitor_id,
                    time_bucket('1 hour', checked_at) AS bucket,
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as operational_count,
                    COUNT(*) FILTER (WHERE status = 'degraded') as degraded_count,
                    COUNT(*) FILTER (WHERE status = 'down') as down_count,
                    AVG(response_time_ms) as avg_response_time_ms,
                    MIN(response_time_ms) as min_response_time_ms,
                    MAX(response_time_ms) as max_response_time_ms,
                    PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY response_time_ms) as p50_response_time_ms,
                    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY response_time_ms) as p95_response_time_ms,
                    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY response_time_ms) as p99_response_time_ms
                FROM status_checks
                GROUP BY monitor_id, bucket
                WITH NO DATA;
                "#,
            )
            .await?;

        // Create continuous aggregate for daily buckets
        db.execute_unprepared(
                r#"
                CREATE MATERIALIZED VIEW IF NOT EXISTS status_checks_daily
                WITH (timescaledb.continuous) AS
                SELECT
                    monitor_id,
                    time_bucket('1 day', checked_at) AS bucket,
                    COUNT(*) as total_checks,
                    COUNT(*) FILTER (WHERE status = 'operational') as operational_count,
                    COUNT(*) FILTER (WHERE status = 'degraded') as degraded_count,
                    COUNT(*) FILTER (WHERE status = 'down') as down_count,
                    AVG(response_time_ms) as avg_response_time_ms,
                    MIN(response_time_ms) as min_response_time_ms,
                    MAX(response_time_ms) as max_response_time_ms,
                    PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY response_time_ms) as p50_response_time_ms,
                    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY response_time_ms) as p95_response_time_ms,
                    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY response_time_ms) as p99_response_time_ms
                FROM status_checks
                GROUP BY monitor_id, bucket
                WITH NO DATA;
                "#,
            )
            .await?;

        // Add refresh policies for continuous aggregates
        db.execute_unprepared(
            r#"
                SELECT add_continuous_aggregate_policy('status_checks_5min',
                    start_offset => INTERVAL '1 hour',
                    end_offset => INTERVAL '5 minutes',
                    schedule_interval => INTERVAL '5 minutes',
                    if_not_exists => TRUE);
                "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
                SELECT add_continuous_aggregate_policy('status_checks_hourly',
                    start_offset => INTERVAL '1 day',
                    end_offset => INTERVAL '1 hour',
                    schedule_interval => INTERVAL '1 hour',
                    if_not_exists => TRUE);
                "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
                SELECT add_continuous_aggregate_policy('status_checks_daily',
                    start_offset => INTERVAL '7 days',
                    end_offset => INTERVAL '1 day',
                    schedule_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);
                "#,
        )
        .await?;

        // Add retention policy - keep raw data for 90 days
        db.execute_unprepared(
                r#"
                SELECT add_retention_policy('status_checks', INTERVAL '90 days', if_not_exists => TRUE);
                "#,
            )
            .await?;

        // Create indexes on continuous aggregates
        db.execute_unprepared(
            r#"
                CREATE INDEX IF NOT EXISTS idx_status_checks_5min_monitor_bucket
                    ON status_checks_5min (monitor_id, bucket DESC);
                "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
                CREATE INDEX IF NOT EXISTS idx_status_checks_hourly_monitor_bucket
                    ON status_checks_hourly (monitor_id, bucket DESC);
                "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
                CREATE INDEX IF NOT EXISTS idx_status_checks_daily_monitor_bucket
                    ON status_checks_daily (monitor_id, bucket DESC);
                "#,
        )
        .await?;

        // Create acme_orders table
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS acme_orders (
                id SERIAL PRIMARY KEY,
                order_url TEXT NOT NULL UNIQUE,
                domain_id INTEGER NOT NULL REFERENCES domains(id) ON DELETE CASCADE,
                email TEXT NOT NULL,
                status TEXT NOT NULL,
                identifiers JSONB NOT NULL,
                authorizations JSONB,
                finalize_url TEXT,
                certificate_url TEXT,
                error TEXT,
                error_type TEXT,
                token TEXT,
                key_authorization TEXT,
                created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                expires_at TIMESTAMP WITH TIME ZONE
            );
            "#,
        )
        .await?;

        // Create indexes for efficient queries
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_acme_orders_domain_id ON acme_orders(domain_id);
            CREATE INDEX IF NOT EXISTS idx_acme_orders_status ON acme_orders(status);
            CREATE INDEX IF NOT EXISTS idx_acme_orders_email ON acme_orders(email);
            CREATE INDEX IF NOT EXISTS idx_acme_orders_expires_at ON acme_orders(expires_at);
            CREATE INDEX IF NOT EXISTS idx_acme_orders_token ON acme_orders(token);
            "#,
        )
        .await?;

        // Remove is_encrypted from users table
        db.execute_unprepared(
            r#"
            ALTER TABLE users
            DROP COLUMN IF EXISTS is_encrypted;
            "#,
        )
        .await?;

        // Remove is_encrypted from backups table
        db.execute_unprepared(
            r#"
            ALTER TABLE backups
            DROP COLUMN IF EXISTS is_encrypted;
            "#,
        )
        .await?;

        // Remove is_encrypted from external_service_backups table
        db.execute_unprepared(
            r#"
            ALTER TABLE external_service_backups
            DROP COLUMN IF EXISTS is_encrypted;
            "#,
        )
        .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("container_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("container_name"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("container_port"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("cpu_request"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("cpu_limit"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("memory_request"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployments"))
                    .drop_column(Alias::new("memory_limit"))
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployment_containers"))
                    .add_column(ColumnDef::new(Alias::new("host_port")).integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployment_containers"))
                    .add_column(ColumnDef::new(Alias::new("image_name")).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deployment_containers"))
                    .add_column(ColumnDef::new(Alias::new("status")).string().null())
                    .to_owned(),
            )
            .await?;

        // Create proxy_logs table
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("proxy_logs"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("method")).text().not_null())
                    .col(ColumnDef::new(Alias::new("path")).text().not_null())
                    .col(ColumnDef::new(Alias::new("query_string")).text().null())
                    .col(ColumnDef::new(Alias::new("host")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("status_code"))
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("response_time_ms"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("request_source"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("is_system_request"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("routing_status"))
                            .text()
                            .not_null(),
                    )
                    // Context (nullable for unrouted requests)
                    .col(ColumnDef::new(Alias::new("project_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("container_id")).text().null())
                    .col(ColumnDef::new(Alias::new("upstream_host")).text().null())
                    // Error tracking
                    .col(ColumnDef::new(Alias::new("error_message")).text().null())
                    // Client information
                    .col(ColumnDef::new(Alias::new("client_ip")).text().null())
                    .col(ColumnDef::new(Alias::new("user_agent")).text().null())
                    .col(ColumnDef::new(Alias::new("referrer")).text().null())
                    .col(ColumnDef::new(Alias::new("request_id")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("ip_geolocation_id"))
                            .integer()
                            .null(),
                    )
                    // User agent parsing
                    .col(ColumnDef::new(Alias::new("browser")).text().null())
                    .col(ColumnDef::new(Alias::new("browser_version")).text().null())
                    .col(ColumnDef::new(Alias::new("operating_system")).text().null())
                    .col(ColumnDef::new(Alias::new("device_type")).text().null())
                    .col(ColumnDef::new(Alias::new("is_bot")).boolean().null())
                    .col(ColumnDef::new(Alias::new("bot_name")).text().null())
                    // Additional metadata
                    .col(
                        ColumnDef::new(Alias::new("request_size_bytes"))
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("response_size_bytes"))
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("cache_status")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("request_headers"))
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("response_headers"))
                            .json_binary()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("created_date")).date().not_null())
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("timestamp")),
                    )
                    .to_owned(),
            )
            .await?;

        // Create foreign key constraints
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_project")
                    .from(Alias::new("proxy_logs"), Alias::new("project_id"))
                    .to(Alias::new("projects"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_environment")
                    .from(Alias::new("proxy_logs"), Alias::new("environment_id"))
                    .to(Alias::new("environments"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_deployment")
                    .from(Alias::new("proxy_logs"), Alias::new("deployment_id"))
                    .to(Alias::new("deployments"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_proxy_logs_ip_geolocation")
                    .from(Alias::new("proxy_logs"), Alias::new("ip_geolocation_id"))
                    .to(Alias::new("ip_geolocations"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        // Convert to TimescaleDB hypertable with id segmenting (space partitioning)
        manager
            .get_connection()
            .execute_unprepared(
                "SELECT create_hypertable('proxy_logs', 'timestamp',
         partitioning_column => 'id',
         number_partitions => 4,
         chunk_time_interval => INTERVAL '1 day',
         if_not_exists => TRUE);",
            )
            .await?;

        // Create a continuous aggregate for hourly stats (optional but useful)
        db.execute_unprepared(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS status_checks_hourly
            WITH (timescaledb.continuous) AS
            SELECT
                monitor_id,
                time_bucket('1 hour', checked_at) AS bucket,
                COUNT(*) as total_checks,
                COUNT(*) FILTER (WHERE status = 'operational') as successful_checks,
                AVG(response_time_ms) as avg_response_time_ms,
                MAX(response_time_ms) as max_response_time_ms,
                MIN(response_time_ms) as min_response_time_ms
            FROM status_checks
            GROUP BY monitor_id, bucket
            WITH NO DATA",
        )
        .await?;

        // Add retention policy to automatically delete old data after 90 days
        db.execute_unprepared(
            "SELECT add_retention_policy(
                'status_checks',
                drop_after => INTERVAL '90 days',
                if_not_exists => TRUE
            )",
        )
        .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(GitProviderConnections::Table)
                    .add_column(
                        ColumnDef::new(GitProviderConnections::IsExpired)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop tables in reverse order to handle foreign key constraints

        // Drop status page tables first (due to foreign key dependencies)
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("status_incident_updates"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("status_incidents"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("status_checks")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("status_monitors"))
                    .to_owned(),
            )
            .await?;

        // Drop new error tracking and DSN tables first
        manager
            .drop_table(Table::drop().table(Alias::new("project_dsns")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("error_events")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("error_groups")).to_owned())
            .await?;

        // Drop funnel tables (due to foreign key dependencies)
        manager
            .drop_table(Table::drop().table(Alias::new("funnel_steps")).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("funnels")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("magic_link_tokens"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("dev_projects")).to_owned())
            .await?;

        // Drop session replay tables first (they depend on visitor)
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("session_replay_events"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("session_replay_sessions"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("visitor")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("api_keys")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("project_services"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("env_var_environments"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("backup_schedules"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("external_service_backups"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("cron_executions"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("deployment_containers"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("deployment_domains"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("repositories")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("user_roles")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("audit_logs")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("request_logs")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("performance_metrics"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("project_custom_domains"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("deployment_metrics"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("crons")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("env_vars")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("environment_domains"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("custom_routes")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("domains")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("acme_accounts")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("backups")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("s3_sources")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("request_sessions"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("tls_acme_certificates"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("external_services"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("git_provider_connections"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("git_providers")).to_owned())
            .await?;

        // Drop original tables
        manager
            .drop_table(Table::drop().table(Alias::new("sessions")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("deployments")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("environments")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("projects")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("users")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("roles")).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("events")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("ip_geolocations"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("notification_preferences"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("notifications")).to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("notification_providers"))
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Alias::new("settings")).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeploymentJobs {
    Table,
    Id,
    DeploymentId,
    JobId,
    JobType,
    Name,
    Description,
    Status,
    LogId,
    JobConfig,
    Outputs,
    Dependencies,
    ExecutionOrder,
    StartedAt,
    FinishedAt,
    ErrorMessage,
    RetryCount,
    MaxRetries,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum GitProviderConnections {
    Table,
    IsExpired,
}
