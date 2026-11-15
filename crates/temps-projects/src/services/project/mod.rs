// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`ProjectService`]: project CRUD, settings, Git wiring and deployment
//! triggers.
//!
//! The service is defined here; its behaviour is split by concern:
//!
//! - `create`: project creation, template-service instantiation and the
//!   initial deployment
//! - `naming`: display-name validation, slug generation and renames
//! - `guards`: reserved-slug claims, Docker-socket grants and tenant ceilings
//! - `queries`: project reads, search, pagination and statistics
//! - `project_settings`: project, deployment and Compose-security settings updates
//! - `git_settings`: Git source updates
//! - `source_directory`: project root-directory validation
//! - `presets`: preset resolution, merging and validation
//! - `templates`: service-template provenance, runtime and upgrades
//! - `hosting`: source type and hosting toggles
//! - `deletion`: project deletion and its delivery-binding guard
//! - `delivery`: CDN delivery provider selection
//! - `webhooks`: provider webhook install/remove (GitLab, Bitbucket, Gitea,
//!   GitHub, generic)
//! - `variables`: environment-variable passthroughs to [`EnvVarService`]
//! - `pipeline`: manual pipeline triggers

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use temps_core::docker_socket_grant::DockerSocketCapability;
use temps_core::url_validation::{redact_url_password, validate_git_url};
use tracing::{error, info, warn};

use sea_orm::{
    prelude::Uuid, sea_query::LockType, ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait,
    DatabaseTransaction, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    RelationTrait, Set, Statement, TransactionTrait,
};
use temps_core::{
    ForceRouteReloadJob, Job, ProjectCreatedJob, ProjectDeletedJob, ProjectUpdatedJob,
};
use temps_entities::{
    delivery_profiles, deployments, dns_providers, domain_delivery_bindings, env_var_environments,
    env_vars, environments, external_services, git_provider_connections, git_providers,
    project_delivery_settings, project_group_members, project_services, projects, settings,
    types::ProjectType,
};
use temps_git::services::public_repo::{
    PublicRefKind, PublicRepoError, PublicRepoProviderFactory, ResolvedPublicRef,
};

use serde::Serialize;

use super::types::{
    CreateProjectEnvVar, CreateProjectRequest, Project, ProjectError, ProjectRename,
    ProjectSettingsUpdate, ProjectStatistics, ReservedSlugChange, SlugClaimAuthority,
    SlugClaimCaller, UpdateDeploymentSettingsRequest, UpdateProjectSettingsParams,
};
use super::{EnvVarService, EnvVarWithEnvironments};
use crate::handlers::{UpdateDeploymentConfigRequest, UpdateServiceTemplateRuntimeRequest};
use temps_core::docker_socket_grant::DeployCaller;

mod create;
mod deletion;
mod delivery;
mod git_settings;
mod guards;
mod hosting;
mod naming;
mod pipeline;
mod presets;
mod project_settings;
mod queries;
mod source_directory;
mod templates;
mod variables;
mod webhooks;

use super::{compose_security, data_purge, types};
use delivery::{chosen_delivery_provider, delivery_provider_label};
use git_settings::would_desync_git_url;
use naming::{slugify, validate_project_name};
use presets::{
    apply_resolved_preset, compose_public_ports, merge_preset_config,
    project_type_for_resolved_preset, resolve_preset_patch, resolve_preset_selection,
    validate_preset_config,
};
use source_directory::{
    normalize_project_directory, normalized_stored_directory, pull_only_root_directory_value,
    SourceDirectoryTarget,
};
pub use templates::{TemplateGithubResponse, TemplateResponse};
use webhooks::repository_target_changed;

// Placeholder functions - these should be implemented properly or imported from other services

/// Optional group membership filter for [`ProjectService::get_projects_paginated_excluding_search`].
#[derive(Debug, Clone, Copy)]
pub enum GroupFilter {
    /// Return only services that are **not** members of any project group.
    Ungrouped,
    /// Return only services that are members of the given project group.
    InGroup(i32),
}

// Add this new struct to represent an environment variable with its environments
#[derive(Debug, Serialize)]
pub struct EnvVarEnvironment {
    pub id: i32,
    pub name: String,
}

// Constants for default hosted website resource profiles. CPU values are stored
// as microcores (1_000_000 = 1 CPU core); memory values are stored as MB. These
// profiles are intentionally separate from the external database-service
// profiles because app containers are usually burstier and safer to cap by
// default on small single-node installs.
pub const DEFAULT_CPU_REQUEST: i32 = 500_000; // 0.5 cores
pub const DEFAULT_MEMORY_REQUEST: i32 = 128; // 128 MB
pub const DEFAULT_MEMORY_LIMIT: i32 = 512; // 512 MB (small hosted website profile)

// Add these constants at the top of the file proper key management
pub const NONCE_LENGTH: usize = 12;

/// Resolve an API/UI catalog slug to its canonical persisted preset and config.
fn resolve_preset_slug(
    slug: &str,
    config: Option<temps_entities::preset::PresetConfig>,
) -> Result<temps_presets::StoredPreset, ProjectError> {
    temps_presets::resolve_preset_slug(slug, config)
        .map_err(|error| ProjectError::InvalidInput(format!("Invalid preset: {}", error)))
}

#[derive(Clone)]
pub struct ProjectService {
    pub db: Arc<temps_database::DbConnection>,
    pub queue_service: Arc<dyn temps_core::JobQueue>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub external_service_manager: Arc<temps_providers::ExternalServiceManager>,
    pub git_provider_manager: Arc<temps_git::GitProviderManager>,
    env_var_service: Arc<EnvVarService>,
    environment_service: Arc<temps_environments::EnvironmentService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// This control plane's own host Docker socket grant (ADR 045).
    ///
    /// Snapshotted from the process-wide grant at construction rather than
    /// read per call: the whole point of the `OnceLock` is that a later
    /// `set_var` cannot change who is root-equivalent, and a field also lets
    /// tests exercise the claim rule against an explicit grant without
    /// mutating process-global environment state.
    docker_socket_grant: temps_core::docker_socket_grant::DockerSocketGrant,
    /// ADR 068 D5: the reachability warning on the project response
    /// (late-bound, optional: empty without the ingress plugin).
    environment_addresses:
        std::sync::OnceLock<Arc<temps_core::environment_address::EnvironmentAddressSlot>>,
}

impl ProjectService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        config_service: Arc<temps_config::ConfigService>,
        external_service_manager: Arc<temps_providers::ExternalServiceManager>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        environment_service: Arc<temps_environments::EnvironmentService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        let env_var_service = Arc::new(EnvVarService::new(db.clone(), encryption_service.clone()));

        ProjectService {
            db: db.clone(),
            queue_service,
            config_service: config_service.clone(),
            external_service_manager,
            git_provider_manager,
            env_var_service,
            environment_service,
            encryption_service,
            docker_socket_grant: temps_core::docker_socket_grant::process_grant().clone(),
            environment_addresses: std::sync::OnceLock::new(),
        }
    }

    /// Wire the ADR 068 D5 reachability warning.
    pub fn set_environment_addresses(
        &self,
        slot: Arc<temps_core::environment_address::EnvironmentAddressSlot>,
    ) {
        let _ = self.environment_addresses.set(slot);
    }

    /// ADR 068 D5: the warning for the project's production environment
    /// (the one the project header shows), or `None`. Never an error: a
    /// reachability failure is logged and reads as no warning.
    pub async fn reachability_warning(
        &self,
        project_id: i32,
    ) -> Option<temps_core::environment_address::ReachabilityWarning> {
        use temps_entities::environments;
        let addresses = self.environment_addresses.get()?.get()?;
        let candidates = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .filter(environments::Column::IsPreview.eq(false))
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .order_by_asc(environments::Column::Id)
            .all(self.db.as_ref())
            .await;
        let candidates = match candidates {
            Ok(rows) => rows,
            Err(error) => {
                warn!(project_id, %error, "could not read environments for the reachability warning");
                return None;
            }
        };
        let environment = candidates
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("production"))
            .or_else(|| candidates.first())?;
        match addresses.address(project_id, environment.id).await {
            Ok(address) => address.warning,
            Err(error) => {
                warn!(
                    project_id,
                    environment_id = environment.id,
                    %error,
                    "could not tell whether the project's environment is reachable"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
