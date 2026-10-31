// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fixtures shared by the manager's unit tests.

use super::*;
use temps_core::{async_trait::async_trait, Job, JobReceiver, QueueError};

pub(super) fn provider_model(
    provider_type: &str,
    base_url: Option<&str>,
    api_url: Option<&str>,
) -> git_providers::Model {
    let now = chrono::Utc::now();
    git_providers::Model {
        id: 1,
        name: "test-provider".to_string(),
        provider_type: provider_type.to_string(),
        base_url: base_url.map(str::to_string),
        api_url: api_url.map(str::to_string),
        auth_method: "pat".to_string(),
        auth_config: serde_json::json!({}),
        webhook_secret: None,
        is_active: true,
        is_default: false,
        created_at: now,
        updated_at: now,
    }
}

// Mock implementations for tests
pub(super) struct MockJobQueue;

#[async_trait]
impl JobQueue for MockJobQueue {
    async fn send(&self, _job: Job) -> Result<(), QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn JobReceiver> {
        panic!("subscribe not needed for tests")
    }
}

pub(super) fn repository_fixture(connection_id: i32) -> repositories::Model {
    let now = chrono::Utc::now();
    repositories::Model {
        id: 42,
        git_provider_connection_id: connection_id,
        owner: "example-owner".to_string(),
        name: "example-repository".to_string(),
        full_name: "example-owner/example-repository".to_string(),
        description: None,
        private: true,
        fork: false,
        created_at: now,
        updated_at: now,
        pushed_at: now,
        size: 0,
        stargazers_count: 0,
        watchers_count: 0,
        language: None,
        default_branch: "main".to_string(),
        open_issues_count: 0,
        topics: String::new(),
        repo_object: "{}".to_string(),
        installation_id: None,
        clone_url: None,
        ssh_url: None,
        preset: None,
    }
}

pub(super) fn connection_fixture(id: i32, user_id: Option<i32>) -> git_provider_connections::Model {
    let now = chrono::Utc::now();
    git_provider_connections::Model {
        id,
        provider_id: 7,
        user_id,
        account_name: "example-account".to_string(),
        account_type: "User".to_string(),
        access_token: None,
        refresh_token: None,
        token_expires_at: None,
        refresh_token_expires_at: None,
        installation_id: None,
        metadata: None,
        is_active: true,
        is_expired: false,
        syncing: false,
        last_synced_at: None,
        synced_repository_count: 0,
        health_status: "healthy".to_string(),
        health_message: None,
        last_health_check_at: None,
        consecutive_health_failures: 0,
        last_sync_error: None,
        last_sync_error_at: None,
        created_at: now,
        updated_at: now,
    }
}

pub(super) fn github_provider_fixture() -> git_providers::Model {
    let now = chrono::Utc::now();
    git_providers::Model {
        id: 7,
        name: "GitHub".to_string(),
        provider_type: "github".to_string(),
        base_url: Some("https://github.com".to_string()),
        api_url: Some("https://api.github.com".to_string()),
        auth_method: "pat".to_string(),
        auth_config: serde_json::json!({}),
        webhook_secret: None,
        is_active: true,
        is_default: true,
        created_at: now,
        updated_at: now,
    }
}

pub(super) fn one_file(path: &str) -> Vec<(String, Vec<u8>)> {
    vec![(path.to_string(), b"package main\n".to_vec())]
}

// Helper function to create a test ConfigService
pub(super) fn create_test_config_service(
    db: Arc<DatabaseConnection>,
) -> Arc<temps_config::ConfigService> {
    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            Some("127.0.0.1:3001".to_string()),
        )
        .unwrap(),
    );
    Arc::new(temps_config::ConfigService::new(server_config, db))
}
