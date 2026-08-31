// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared row builders for the ingest-key unit tests.
//!
//! Kept in one place so a new column on `projects`/`environments` breaks a
//! single fixture instead of every test module in this directory.

use chrono::Utc;
use temps_entities::{analytics_ingest_keys, environments, projects, users};

pub fn key_model(
    id: i32,
    project_id: i32,
    environment_id: Option<i32>,
) -> analytics_ingest_keys::Model {
    let now = Utc::now();
    analytics_ingest_keys::Model {
        id,
        project_id,
        environment_id,
        name: "Default ingest key".to_string(),
        public_key: format!("pa_{}", "0".repeat(64)),
        is_active: true,
        revoked_at: None,
        rate_limit_per_minute: Some(600),
        allowed_origins: None,
        event_count: 0,
        last_used_at: None,
        created_by_user_id: Some(1),
        created_at: now,
        updated_at: now,
    }
}

pub fn project_model(id: i32) -> projects::Model {
    let now = Utc::now();
    projects::Model {
        id,
        name: format!("project-{id}"),
        repo_name: "test-repo".to_string(),
        repo_owner: "test-owner".to_string(),
        directory: String::new(),
        main_branch: "main".to_string(),
        preset: temps_entities::preset::Preset::NextJs,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: format!("project-{id}"),
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: false,
        git_url: None,
        git_provider_connection_id: None,
        attack_mode: false,
        ai_alert_summaries_enabled: None,
        ai_debug_chat_enabled: None,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 300,
        preview_envs_wake_timeout_seconds: 30,
        source_type: temps_entities::source_type::SourceType::Git,
        allow_alternate_sources: None,
        template_slug: None,
        gitlab_webhook_id: None,
        gitlab_webhook_signing_token: None,
        gitea_webhook_signing_token: None,
        bitbucket_webhook_token: None,
        bitbucket_webhook_hook_id: None,
        generic_webhook_token: None,
        cross_project_trace_sharing: true,
        ai_api_traffic_summary_enabled: None,
        image_retention_hours: None,
    }
}

pub fn environment_model(
    id: i32,
    project_id: i32,
    current_deployment_id: Option<i32>,
) -> environments::Model {
    let now = Utc::now();
    environments::Model {
        id,
        name: "production".to_string(),
        slug: "production".to_string(),
        subdomain: "prod".to_string(),
        last_deployment: None,
        host: "app.example.com".to_string(),
        upstreams: Default::default(),
        created_at: now,
        updated_at: now,
        project_id,
        current_deployment_id,
        branch: None,
        deleted_at: None,
        deployment_config: None,
        is_preview: false,
        protected: false,
        sleeping: false,
        attack_mode: None,
        force_https: None,
        last_activity_at: None,
    }
}

pub fn user_model(id: i32) -> users::Model {
    let now = Utc::now();
    users::Model {
        id,
        name: "Test User".to_string(),
        email: format!("user{id}@example.com"),
        password_hash: None,
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        must_change_password: false,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        oidc_subject: None,
        oidc_provider_id: None,
        created_at: now,
        updated_at: now,
    }
}
