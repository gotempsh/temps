// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`GitProviderManager`]: git providers, their connections and repositories.
//!
//! The manager is one type whose `impl` is spread over the submodules below,
//! one responsibility each. Shared imports live here; submodules start with
//! `use super::*;`.

use super::git_provider::{
    AuthMethod, GitProviderError, GitProviderFactory, GitProviderService, GitProviderType,
};
use reqwest::Url;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::{JobQueue, UtcDateTime};
use temps_entities::{git_provider_connections, git_providers, projects, repositories};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

mod connections;
mod error;
mod git_push;
mod health;
mod installations;
mod manager_trait;
mod oauth;
mod presets;
mod providers;
mod push_events;
mod repo_write;
mod repository_queries;
mod sync;
#[cfg(test)]
mod test_support;
mod tokens;
mod types;
mod validation;

pub use error::GitProviderManagerError;
pub(crate) use git_push::CommitCancellation;
pub use types::{
    ComposeServicePreviewDomain, EnvExampleVariableDomain, ProjectPresetDomain, ProjectUsageInfo,
    ProviderDeletionCheck, RepositoryComposePreviewDomain, RepositoryComposeServicesDomain,
    RepositoryComposeSourceDomain, RepositoryEnvExampleDomain, RepositoryPresetDomain,
};
pub(crate) use validation::{is_safe_name_segment, MAX_REPOSITORY_NAME_LEN};
pub use validation::{MAX_COMMIT_BYTES, MAX_COMMIT_FILES};

use error::foreign_or_missing_connection;
use git_push::GitPushRequest;
use validation::{
    is_commit_id, validate_branch_name, validate_commit_request, validate_new_repository,
    validate_repository_ref, COMMIT_FILES_TIMEOUT, MAX_PULL_REQUEST_BODY_LEN,
    MAX_PULL_REQUEST_TITLE_LEN,
};

#[derive(Clone)]
pub struct GitProviderManager {
    db: Arc<DatabaseConnection>,
    providers_cache: Arc<RwLock<HashMap<i32, Arc<dyn GitProviderService>>>>,
    encryption_service: Arc<temps_core::EncryptionService>,
    queue_service: Arc<dyn JobQueue>,
    config_service: Arc<ConfigService>,
}

impl GitProviderManager {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
        queue_service: Arc<dyn JobQueue>,
        config_service: Arc<ConfigService>,
    ) -> Self {
        Self {
            db,
            providers_cache: Arc::new(RwLock::new(HashMap::new())),
            encryption_service,
            queue_service,
            config_service,
        }
    }

    /// Expose the underlying database connection for crate-internal use.
    pub fn db(&self) -> &DatabaseConnection {
        self.db.as_ref()
    }

    /// HTTPS clone username for a stored provider type.
    fn clone_username_for_provider_type(provider_type: &str) -> &'static str {
        match provider_type {
            "gitlab" => "oauth2",
            "bitbucket" => "x-token-auth",
            _ => "x-access-token",
        }
    }
}

/// Replace every occurrence of `secret` in `text`, so a provider or git error
/// that echoes the authenticated URL cannot carry the token to a response.
fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacting_a_secret_removes_every_occurrence_and_ignores_an_empty_one() {
        let text = "clone https://x-access-token:s3cr3t@host/o/r failed: s3cr3t rejected";
        let redacted = redact_secret(text, "s3cr3t");
        assert!(!redacted.contains("s3cr3t"));
        assert_eq!(redact_secret(text, ""), text);
    }
}
