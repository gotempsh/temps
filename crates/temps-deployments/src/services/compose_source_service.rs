// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use temps_entities::deployments::DeploymentMetadata;
use temps_entities::preset::{
    ComposeServiceSnapshot, ComposeTemplateOrigin, DockerComposeConfig, Preset, PresetConfig,
};
use temps_entities::source_type::SourceType;
use temps_entities::{deployments, environments, projects, source_bundles};
use thiserror::Error;

const COMPOSE_SOURCE_KIND: &str = "compose";
const MAX_COMPOSE_SOURCE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ComposeSourceError {
    #[error("Project {project_id} was not found")]
    ProjectNotFound { project_id: i32 },
    #[error("Environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Project {project_id} uses source '{source_type}' and preset '{preset}'; editable Compose sources require source 'compose' and preset 'docker-compose'")]
    IncompatibleProject {
        project_id: i32,
        source_type: SourceType,
        preset: Preset,
    },
    #[error("Project {project_id} has a non-Compose preset configuration")]
    InvalidPresetConfig { project_id: i32 },
    #[error("Project {project_id} has unsafe Compose path '{path}'")]
    UnsafeComposePath { project_id: i32, path: String },
    #[error("Docker Compose YAML cannot be empty for project {project_id}")]
    EmptySource { project_id: i32 },
    #[error("Docker Compose YAML for project {project_id} exceeds {max_bytes} bytes")]
    SourceTooLarge { project_id: i32, max_bytes: usize },
    #[error("Docker Compose YAML is invalid for project {project_id}: {reason}")]
    InvalidYaml { project_id: i32, reason: String },
    #[error("Docker Compose YAML for project {project_id} must define at least one named service")]
    NoServices { project_id: i32 },
    #[error("Docker Compose YAML for project {project_id} contains a literal credential at {location}; reference an encrypted project environment variable instead")]
    EmbeddedCredential { project_id: i32, location: String },
    #[error("Health-check path '{path}' for Compose project {project_id} is invalid: {reason}")]
    InvalidHealthCheckPath {
        project_id: i32,
        path: String,
        reason: String,
    },
    #[error("Project {project_id} has no saved Compose source{revision_suffix}")]
    SourceNotFound {
        project_id: i32,
        revision_suffix: String,
    },
    #[error("Project {project_id} is at Compose revision {current_revision:?}, but the editor saved from revision {expected_revision:?}; reload before saving")]
    RevisionConflict {
        project_id: i32,
        current_revision: Option<i32>,
        expected_revision: Option<i32>,
    },
    #[error("Database operation '{operation}' failed for Compose project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Storage operation '{operation}' failed for Compose project {project_id} at '{path}': {reason}")]
    Storage {
        project_id: i32,
        operation: &'static str,
        path: String,
        reason: String,
    },
    #[error(
        "Background operation '{operation}' failed for Compose project {project_id}: {reason}"
    )]
    Task {
        project_id: i32,
        operation: &'static str,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct ComposeSourceDocument {
    pub content: String,
    pub bundle: source_bundles::Model,
    pub services: Vec<temps_presets::ComposeServicePreview>,
    pub origin: Option<ComposeTemplateOrigin>,
}

#[derive(Debug, Clone)]
pub struct PreparedComposeDeployment {
    pub deployment: deployments::Model,
    pub environment: environments::Model,
    pub bundle: source_bundles::Model,
}

#[derive(Clone)]
pub struct ComposeSourceService {
    db: Arc<DatabaseConnection>,
    data_dir: PathBuf,
}

impl ComposeSourceService {
    pub fn new(db: Arc<DatabaseConnection>, data_dir: PathBuf) -> Self {
        Self { db, data_dir }
    }

    pub async fn get(&self, project_id: i32) -> Result<ComposeSourceDocument, ComposeSourceError> {
        let project = self.load_project(project_id).await?;
        let (current_compose_path, _, origin) = compose_path_and_origin(&project)?;
        let bundle = find_revision(self.db.as_ref(), project_id, None)
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "load current source revision",
                source,
            })?
            .ok_or_else(|| source_not_found(project_id, None))?;
        let (compose_path, _, archive_entry) = bundle_compose_location(
            project_id,
            &bundle,
            &current_compose_path,
            &project.directory,
        )?;
        let content = read_archive(
            &self.data_dir,
            project_id,
            &bundle,
            &archive_entry,
            Some(&compose_path),
        )
        .await?;
        let services = parse_source(project_id, &content)?;
        Ok(ComposeSourceDocument {
            content,
            bundle,
            services,
            origin,
        })
    }

    pub async fn save(
        &self,
        project_id: i32,
        content: String,
        expected_revision: Option<i32>,
    ) -> Result<ComposeSourceDocument, ComposeSourceError> {
        let parse_content = content.clone();
        let services =
            tokio::task::spawn_blocking(move || parse_source(project_id, &parse_content))
                .await
                .map_err(|error| ComposeSourceError::Task {
                    project_id,
                    operation: "validate Compose YAML",
                    reason: error.to_string(),
                })??;

        let transaction = self
            .db
            .begin()
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "start source transaction",
                source,
            })?;
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .lock_exclusive()
            .one(&transaction)
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "lock project",
                source,
            })?
            .ok_or(ComposeSourceError::ProjectNotFound { project_id })?;
        validate_project(&project)?;
        let (compose_path, mut config, origin) = compose_path_and_origin(&project)?;
        let current = find_revision(&transaction, project_id, None)
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "load current source revision",
                source,
            })?;
        let current_revision = current.as_ref().map(|bundle| bundle.id);
        if expected_revision != current_revision {
            return Err(ComposeSourceError::RevisionConflict {
                project_id,
                current_revision,
                expected_revision,
            });
        }
        let template_slug_update = if current_revision.is_none() {
            project.template_slug.as_deref().and_then(|provenance| {
                match temps_core::templates::resolve_pending_service_catalog_template_provenance(
                    provenance, &content,
                ) {
                    temps_core::templates::PendingServiceCatalogResolution::NotPending => None,
                    temps_core::templates::PendingServiceCatalogResolution::Matched(provenance) => {
                        Some(Some(provenance))
                    }
                    temps_core::templates::PendingServiceCatalogResolution::Mismatched => {
                        Some(None)
                    }
                }
            })
        } else {
            None
        };

        let archive_entry =
            compose_archive_entry_path(project_id, &project.directory, &compose_path)?;
        let archive_content = content.clone();
        let archive_compose_path = archive_entry.clone();
        let archive = tokio::task::spawn_blocking(move || {
            create_archive(&archive_content, &archive_compose_path)
        })
        .await
        .map_err(|error| ComposeSourceError::Task {
            project_id,
            operation: "build Compose archive",
            reason: error.to_string(),
        })?
        .map_err(|error| ComposeSourceError::Storage {
            project_id,
            operation: "build Compose archive",
            path: compose_path.clone(),
            reason: error.to_string(),
        })?;

        let checksum = format!("sha256:{}", hex::encode(Sha256::digest(content.as_bytes())));
        let relative_path = format!("source-bundles/compose-{}.zip", uuid::Uuid::new_v4());
        let absolute_path = self.data_dir.join(&relative_path);
        if let Some(parent) = absolute_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                ComposeSourceError::Storage {
                    project_id,
                    operation: "create source storage",
                    path: parent.display().to_string(),
                    reason: error.to_string(),
                }
            })?;
        }
        tokio::fs::write(&absolute_path, &archive)
            .await
            .map_err(|error| ComposeSourceError::Storage {
                project_id,
                operation: "write source archive",
                path: absolute_path.display().to_string(),
                reason: error.to_string(),
            })?;

        let now = Utc::now();
        let bundle = match (source_bundles::ActiveModel {
            project_id: Set(project_id),
            source_kind: Set(COMPOSE_SOURCE_KIND.to_string()),
            archive_path: Set(relative_path),
            original_filename: Set(Some(compose_path.clone())),
            content_type: Set("application/vnd.temps.compose+zip".to_string()),
            size_bytes: Set(archive.len() as i64),
            checksum: Set(checksum),
            directory: Set(project.directory.clone()),
            preset: Set(project.preset.as_str().to_string()),
            metadata: Set(Some(serde_json::json!({
                "source_kind": COMPOSE_SOURCE_KIND,
                "compose_path": compose_path,
                "directory": project.directory,
                "parent_revision": current_revision,
            }))),
            uploaded_at: Set(now),
            created_at: Set(now),
            ..Default::default()
        })
        .insert(&transaction)
        .await
        {
            Ok(bundle) => bundle,
            Err(source) => {
                let _ = transaction.rollback().await;
                let _ = tokio::fs::remove_file(&absolute_path).await;
                return Err(ComposeSourceError::Database {
                    project_id,
                    operation: "register source revision",
                    source,
                });
            }
        };

        config.compose_services = service_snapshots(&services);
        let mut project_update: projects::ActiveModel = project.into();
        project_update.preset_config = Set(Some(PresetConfig::DockerCompose(config)));
        if let Some(template_slug) = template_slug_update {
            project_update.template_slug = Set(template_slug);
        }
        if let Err(source) = project_update.update(&transaction).await {
            let _ = transaction.rollback().await;
            let _ = tokio::fs::remove_file(&absolute_path).await;
            return Err(ComposeSourceError::Database {
                project_id,
                operation: "update Compose discovery metadata",
                source,
            });
        }
        if let Err(source) = transaction.commit().await {
            let _ = tokio::fs::remove_file(&absolute_path).await;
            return Err(ComposeSourceError::Database {
                project_id,
                operation: "commit source revision",
                source,
            });
        }

        Ok(ComposeSourceDocument {
            content,
            bundle,
            services,
            origin,
        })
    }

    pub async fn prepare_deployment(
        &self,
        project_id: i32,
        environment_id: i32,
        revision: Option<i32>,
    ) -> Result<PreparedComposeDeployment, ComposeSourceError> {
        let project = self.load_project(project_id).await?;
        let (current_compose_path, mut config, _) = compose_path_and_origin(&project)?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "load deployment environment",
                source,
            })?
            .ok_or(ComposeSourceError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        let bundle = find_revision(self.db.as_ref(), project_id, revision)
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "load deployment source revision",
                source,
            })?
            .ok_or_else(|| source_not_found(project_id, revision))?;
        let (compose_path, directory, archive_entry) = bundle_compose_location(
            project_id,
            &bundle,
            &current_compose_path,
            &project.directory,
        )?;
        let content = read_archive(
            &self.data_dir,
            project_id,
            &bundle,
            &archive_entry,
            Some(&compose_path),
        )
        .await?;
        let parse_content = content.clone();
        let selected_services =
            tokio::task::spawn_blocking(move || parse_source(project_id, &parse_content))
                .await
                .map_err(|error| ComposeSourceError::Task {
                    project_id,
                    operation: "validate selected Compose revision",
                    reason: error.to_string(),
                })??;
        config.compose_services = service_snapshots(&selected_services);
        let health_check_path =
            effective_health_check_path(project.id, Some(&PresetConfig::DockerCompose(config)))?;

        let now = Utc::now();
        let deployment = (deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(format!(
                "{}-{}",
                project.slug,
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            )),
            state: Set("pending".to_string()),
            metadata: Set(Some(DeploymentMetadata {
                source_bundle_id: Some(bundle.id),
                source_bundle_path: Some(bundle.archive_path.clone()),
                source_bundle_content_type: Some(bundle.content_type.clone()),
                deployment_source_type: Some(SourceType::Compose),
                health_check_path,
                ..Default::default()
            })),
            context_vars: Set(Some(serde_json::json!({
                "trigger": "compose_source",
                "source": "compose",
                "source_revision": bundle.id,
                "compose_path": compose_path,
                "directory": directory,
            }))),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        })
        .insert(self.db.as_ref())
        .await
        .map_err(|source| ComposeSourceError::Database {
            project_id,
            operation: "create deployment",
            source,
        })?;

        Ok(PreparedComposeDeployment {
            deployment,
            environment,
            bundle,
        })
    }

    pub async fn delete_unqueued_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<(), ComposeSourceError> {
        deployments::Entity::delete_by_id(deployment_id)
            .exec(self.db.as_ref())
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "delete unqueued deployment",
                source,
            })?;
        Ok(())
    }

    async fn load_project(&self, project_id: i32) -> Result<projects::Model, ComposeSourceError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|source| ComposeSourceError::Database {
                project_id,
                operation: "load project",
                source,
            })?
            .ok_or(ComposeSourceError::ProjectNotFound { project_id })?;
        validate_project(&project)?;
        Ok(project)
    }
}

fn effective_health_check_path(
    project_id: i32,
    preset_config: Option<&PresetConfig>,
) -> Result<Option<String>, ComposeSourceError> {
    let Some(PresetConfig::DockerCompose(config)) = preset_config else {
        return Ok(None);
    };
    let Some(primary_route) = config.public_ports.first() else {
        return Ok(None);
    };
    let path = primary_route.health_check_path.clone().or_else(|| {
        config
            .compose_services
            .iter()
            .find(|service| service.name == primary_route.service)
            .and_then(|service| service.health_check_path.clone())
    });
    if let Some(path) = path.as_deref() {
        validate_health_check_path(project_id, path)?;
    }
    Ok(path)
}

fn validate_health_check_path(project_id: i32, path: &str) -> Result<(), ComposeSourceError> {
    let invalid = |reason: &str| ComposeSourceError::InvalidHealthCheckPath {
        project_id,
        path: path.to_string(),
        reason: reason.to_string(),
    };
    if path.len() > 2048 {
        return Err(invalid("path exceeds 2048 bytes"));
    }
    if !path.starts_with('/') {
        return Err(invalid("path must start with '/'"));
    }
    if path.contains('@') || path.contains("://") {
        return Err(invalid("path must not contain a URL authority or scheme"));
    }
    if path.chars().any(char::is_control) {
        return Err(invalid("path must not contain control characters"));
    }
    Ok(())
}

fn validate_project(project: &projects::Model) -> Result<(), ComposeSourceError> {
    if project.source_type != SourceType::Compose || project.preset != Preset::DockerCompose {
        return Err(ComposeSourceError::IncompatibleProject {
            project_id: project.id,
            source_type: project.source_type,
            preset: project.preset,
        });
    }
    Ok(())
}

fn compose_path_and_origin(
    project: &projects::Model,
) -> Result<(String, DockerComposeConfig, Option<ComposeTemplateOrigin>), ComposeSourceError> {
    let config = match project.preset_config.clone() {
        Some(PresetConfig::DockerCompose(config)) => config,
        Some(_) => {
            return Err(ComposeSourceError::InvalidPresetConfig {
                project_id: project.id,
            });
        }
        None => DockerComposeConfig::default(),
    };
    let compose_path = config
        .compose_path
        .clone()
        .unwrap_or_else(|| "docker-compose.yml".to_string());
    let path = Path::new(&compose_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ComposeSourceError::UnsafeComposePath {
            project_id: project.id,
            path: compose_path,
        });
    }
    let origin = config.template_origin.as_deref().cloned();
    Ok((compose_path, config, origin))
}

fn compose_archive_entry_path(
    project_id: i32,
    directory: &str,
    compose_path: &str,
) -> Result<String, ComposeSourceError> {
    let mut entry = PathBuf::new();
    let directory = directory.trim();
    if !directory.is_empty() && directory != "." && directory != "./" {
        entry.push(directory);
    }
    entry.push(compose_path);
    if entry.is_absolute()
        || entry.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ComposeSourceError::UnsafeComposePath {
            project_id,
            path: entry.display().to_string(),
        });
    }
    entry
        .to_str()
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ComposeSourceError::UnsafeComposePath {
            project_id,
            path: entry.display().to_string(),
        })
}

fn bundle_compose_location(
    project_id: i32,
    bundle: &source_bundles::Model,
    fallback_compose_path: &str,
    fallback_directory: &str,
) -> Result<(String, String, String), ComposeSourceError> {
    let compose_path = bundle
        .original_filename
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(fallback_compose_path)
        .to_string();
    let directory = if bundle.directory.trim().is_empty() {
        fallback_directory.to_string()
    } else {
        bundle.directory.clone()
    };
    let archive_entry = compose_archive_entry_path(project_id, &directory, &compose_path)?;
    Ok((compose_path, directory, archive_entry))
}

fn parse_source(
    project_id: i32,
    content: &str,
) -> Result<Vec<temps_presets::ComposeServicePreview>, ComposeSourceError> {
    if content.trim().is_empty() {
        return Err(ComposeSourceError::EmptySource { project_id });
    }
    if content.len() > MAX_COMPOSE_SOURCE_BYTES {
        return Err(ComposeSourceError::SourceTooLarge {
            project_id,
            max_bytes: MAX_COMPOSE_SOURCE_BYTES,
        });
    }
    let services = temps_presets::list_compose_services(content).map_err(|error| {
        ComposeSourceError::InvalidYaml {
            project_id,
            reason: error.to_string(),
        }
    })?;
    if services.is_empty() {
        return Err(ComposeSourceError::NoServices { project_id });
    }
    reject_embedded_credentials(project_id, content)?;
    Ok(services)
}

fn reject_embedded_credentials(project_id: i32, content: &str) -> Result<(), ComposeSourceError> {
    match temps_presets::validate_compose_credentials(content) {
        Ok(()) => Ok(()),
        Err(temps_presets::ComposeCredentialValidationError::InvalidCompose(error)) => {
            Err(ComposeSourceError::InvalidYaml {
                project_id,
                reason: error.to_string(),
            })
        }
        Err(temps_presets::ComposeCredentialValidationError::EmbeddedCredential { location }) => {
            Err(ComposeSourceError::EmbeddedCredential {
                project_id,
                location,
            })
        }
    }
}

fn create_archive(content: &str, compose_path: &str) -> Result<Vec<u8>, std::io::Error> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    archive
        .start_file(compose_path, options)
        .map_err(std::io::Error::other)?;
    archive.write_all(content.as_bytes())?;
    archive
        .finish()
        .map(std::io::Cursor::into_inner)
        .map_err(std::io::Error::other)
}

async fn read_archive(
    data_dir: &Path,
    project_id: i32,
    bundle: &source_bundles::Model,
    archive_entry: &str,
    legacy_entry: Option<&str>,
) -> Result<String, ComposeSourceError> {
    let archive_path = data_dir.join(&bundle.archive_path);
    let display_path = archive_path.display().to_string();
    let mut entry_paths = vec![archive_entry.to_string()];
    if let Some(legacy_entry) = legacy_entry.filter(|entry| *entry != archive_entry) {
        entry_paths.push(legacy_entry.to_string());
    }
    tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        let file = std::fs::File::open(&archive_path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
        for entry_path in &entry_paths {
            let Ok(mut entry) = archive.by_name(entry_path) else {
                continue;
            };
            if entry.size() > MAX_COMPOSE_SOURCE_BYTES as u64 {
                return Err(std::io::Error::other("Compose source exceeds read limit"));
            }
            let mut content = String::with_capacity(entry.size() as usize);
            entry.read_to_string(&mut content)?;
            return Ok(content);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Compose entry not found (tried {})", entry_paths.join(", ")),
        ))
    })
    .await
    .map_err(|error| ComposeSourceError::Task {
        project_id,
        operation: "read Compose archive",
        reason: error.to_string(),
    })?
    .map_err(|error| ComposeSourceError::Storage {
        project_id,
        operation: "read Compose archive",
        path: display_path,
        reason: error.to_string(),
    })
}

async fn find_revision<C>(
    db: &C,
    project_id: i32,
    revision: Option<i32>,
) -> Result<Option<source_bundles::Model>, sea_orm::DbErr>
where
    C: sea_orm::ConnectionTrait,
{
    let mut query = source_bundles::Entity::find()
        .filter(source_bundles::Column::ProjectId.eq(project_id))
        .filter(source_bundles::Column::SourceKind.eq(COMPOSE_SOURCE_KIND));
    if let Some(revision) = revision {
        query = query.filter(source_bundles::Column::Id.eq(revision));
    }
    query
        .order_by_desc(source_bundles::Column::Id)
        .one(db)
        .await
}

fn source_not_found(project_id: i32, revision: Option<i32>) -> ComposeSourceError {
    ComposeSourceError::SourceNotFound {
        project_id,
        revision_suffix: revision
            .map(|revision| format!(" at revision {revision}"))
            .unwrap_or_default(),
    }
}

fn service_snapshots(
    services: &[temps_presets::ComposeServicePreview],
) -> Vec<ComposeServiceSnapshot> {
    services
        .iter()
        .map(|service| ComposeServiceSnapshot {
            name: service.name.clone(),
            image: service.image.clone(),
            looks_like_database: service.looks_like_database,
            detected_service_type: service.detected_service_type,
            ports: service.ports.clone(),
            health_check_path: service.health_check_path.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::upstream_config::UpstreamList;

    async fn compose_project_fixture(
        db: &DatabaseConnection,
        slug: &str,
    ) -> (projects::Model, environments::Model) {
        let config = DockerComposeConfig {
            compose_path: Some("stack/compose.yml".to_string()),
            public_ports: vec![temps_entities::preset::ComposePublicPort {
                service: "app".to_string(),
                port: 8080,
                ..Default::default()
            }],
            ..Default::default()
        };
        let project = projects::ActiveModel {
            name: Set(format!("Compose {slug}")),
            repo_name: Set(String::new()),
            repo_owner: Set(String::new()),
            slug: Set(slug.to_string()),
            preset: Set(Preset::DockerCompose),
            preset_config: Set(Some(PresetConfig::DockerCompose(config))),
            source_type: Set(SourceType::Compose),
            directory: Set("saved-root".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(format!("{slug}-production")),
            host: Set(format!("{slug}.test.local")),
            upstreams: Set(UpstreamList::default()),
            branch: Set(Some("main".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        (project, environment)
    }

    #[tokio::test]
    async fn saved_revision_is_immutable_and_redeploys_from_its_snapshot() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let storage = tempfile::tempdir().unwrap();
        let service = ComposeSourceService::new(db.clone(), storage.path().to_path_buf());
        let (project, environment) = compose_project_fixture(db.as_ref(), "compose-snapshot").await;
        let yaml = r#"services:
  app:
    image: example/app:1
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:8080/ready"]
"#;

        let saved = service
            .save(project.id, yaml.to_string(), None)
            .await
            .unwrap();
        assert_eq!(service.get(project.id).await.unwrap().content, yaml);
        assert!(matches!(
            service.save(project.id, yaml.to_string(), None).await,
            Err(ComposeSourceError::RevisionConflict { .. })
        ));

        let changed_config = DockerComposeConfig {
            compose_path: Some("current/compose.yaml".to_string()),
            public_ports: vec![temps_entities::preset::ComposePublicPort {
                service: "app".to_string(),
                port: 8080,
                ..Default::default()
            }],
            compose_services: vec![ComposeServiceSnapshot {
                name: "app".to_string(),
                health_check_path: Some("/current".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut changed_project: projects::ActiveModel = project.clone().into();
        changed_project.directory = Set("current-root".to_string());
        changed_project.preset_config = Set(Some(PresetConfig::DockerCompose(changed_config)));
        changed_project.update(db.as_ref()).await.unwrap();

        let prepared = service
            .prepare_deployment(project.id, environment.id, Some(saved.bundle.id))
            .await
            .unwrap();
        let context = prepared.deployment.context_vars.as_ref().unwrap();
        assert_eq!(
            context.get("compose_path").and_then(|v| v.as_str()),
            Some("stack/compose.yml")
        );
        assert_eq!(
            context.get("directory").and_then(|v| v.as_str()),
            Some("saved-root")
        );
        assert_eq!(
            prepared
                .deployment
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.health_check_path.as_deref()),
            Some("/ready")
        );

        service
            .delete_unqueued_deployment(project.id, prepared.deployment.id)
            .await
            .unwrap();
        assert!(deployments::Entity::find_by_id(prepared.deployment.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn first_compose_source_promotes_only_matching_catalog_provenance() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let storage = tempfile::tempdir().unwrap();
        let service = ComposeSourceService::new(db.clone(), storage.path().to_path_buf());
        let catalog_compose =
            "services:\n  keycloak:\n    image: quay.io/keycloak/keycloak:26.3.2\n";
        let pending = temps_core::templates::pending_service_catalog_template_provenance(
            "keycloak",
            catalog_compose,
        )
        .unwrap();

        let (matching_project, _) =
            compose_project_fixture(db.as_ref(), "compose-catalog-match").await;
        let mut matching_update: projects::ActiveModel = matching_project.clone().into();
        matching_update.template_slug = Set(Some(pending.clone()));
        matching_update.update(db.as_ref()).await.unwrap();
        service
            .save(matching_project.id, catalog_compose.to_string(), None)
            .await
            .unwrap();
        let matching = projects::Entity::find_by_id(matching_project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            matching.template_slug.as_deref(),
            Some("service_catalog:keycloak")
        );

        let (mismatched_project, _) =
            compose_project_fixture(db.as_ref(), "compose-catalog-mismatch").await;
        let mut mismatched_update: projects::ActiveModel = mismatched_project.clone().into();
        mismatched_update.template_slug = Set(Some(pending));
        mismatched_update.update(db.as_ref()).await.unwrap();
        service
            .save(
                mismatched_project.id,
                "services:\n  unrelated:\n    image: example/unrelated:latest\n".to_string(),
                None,
            )
            .await
            .unwrap();
        let mismatched = projects::Entity::find_by_id(mismatched_project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mismatched.template_slug, None);
    }

    #[tokio::test]
    async fn compose_deploy_rejects_an_environment_from_another_project() {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let storage = tempfile::tempdir().unwrap();
        let service = ComposeSourceService::new(db.clone(), storage.path().to_path_buf());
        let (project, _) = compose_project_fixture(db.as_ref(), "compose-owner").await;
        let (_, foreign_environment) =
            compose_project_fixture(db.as_ref(), "compose-foreign").await;
        service
            .save(
                project.id,
                "services:\n  app:\n    image: example/app:1\n".to_string(),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(
            service
                .prepare_deployment(project.id, foreign_environment.id, None)
                .await,
            Err(ComposeSourceError::EnvironmentNotFound { .. })
        ));
    }

    #[test]
    fn validation_discovers_editable_image_versions() {
        let content = r#"
services:
  keycloak:
    image: quay.io/keycloak/keycloak:26.3.2
    ports:
      - "8080:8080"
"#;
        let services = parse_source(42, content).expect("valid Compose source");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "keycloak");
        assert_eq!(
            services[0].image.as_deref(),
            Some("quay.io/keycloak/keycloak:26.3.2")
        );
        assert_eq!(services[0].ports[0].target, 8080);
    }

    #[test]
    fn validation_rejects_empty_service_maps() {
        let error = parse_source(42, "services: {}\n").expect_err("must reject no services");
        assert!(matches!(
            error,
            ComposeSourceError::NoServices { project_id: 42 }
        ));
    }

    #[test]
    fn validation_rejects_literal_environment_credentials_without_echoing_them() {
        let error = parse_source(
            42,
            r#"
services:
  app:
    image: example/app:1
    environment:
      ADMIN_PASSWORD: super-secret-value
"#,
        )
        .expect_err("literal credentials must not enter source storage");

        assert!(matches!(
            &error,
            ComposeSourceError::EmbeddedCredential { project_id: 42, .. }
        ));
        assert!(!error.to_string().contains("super-secret-value"));
    }

    #[test]
    fn validation_rejects_credentials_in_merged_environment_without_echoing_them() {
        let error = parse_source(
            42,
            r#"
x-environment: &shared-environment
  ADMIN_PASSWORD: merged-secret-value
services:
  app:
    image: example/app:1
    environment:
      <<: *shared-environment
"#,
        )
        .expect_err("merged literal credentials must not enter source storage");

        assert!(matches!(
            &error,
            ComposeSourceError::EmbeddedCredential { project_id: 42, .. }
        ));
        assert!(!error.to_string().contains("merged-secret-value"));
    }

    #[test]
    fn validation_accepts_secret_references_and_composed_database_urls() {
        let services = parse_source(
            42,
            r#"
services:
  app:
    image: example/app:1
    environment:
      ADMIN_PASSWORD: ${ADMIN_PASSWORD:?configure ADMIN_PASSWORD in project secrets}
      DATABASE_URL: postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@postgres/${POSTGRES_DB}
"#,
        )
        .expect("secret references should remain editable");

        assert_eq!(services.len(), 1);
    }

    #[test]
    fn validation_rejects_authenticated_literal_urls_and_private_keys() {
        for content in [
            r#"
services:
  app:
    image: example/app:1
    environment:
      UPSTREAM_URL: https://user:literal-password@example.com/api
"#,
            r#"
services:
  app:
    image: example/app:1
    environment:
      CONFIG: |
        -----BEGIN PRIVATE KEY-----
        sensitive-key-material
        -----END PRIVATE KEY-----
"#,
        ] {
            assert!(matches!(
                parse_source(42, content),
                Err(ComposeSourceError::EmbeddedCredential { project_id: 42, .. })
            ));
        }
    }

    #[test]
    fn validation_rejects_literal_url_userinfo_and_sensitive_query_parameters() {
        for content in [
            r#"
services:
  app:
    image: example/app:1
    environment:
      DATABASE_URL: postgres://literal-user:${DATABASE_PASSWORD}@database/app
"#,
            r#"
services:
  app:
    image: example/app:1
    environment:
      DATABASE_URL: postgres://database/app?pass%77ord=literal-query-secret
"#,
            r#"
services:
  app:
    image: example/app:1
    environment:
      UPSTREAM_URL: https://example.test/api?api-key=literal-query-secret
"#,
        ] {
            let error = parse_source(42, content)
                .expect_err("literal URL credentials must not enter source storage");
            assert!(matches!(
                error,
                ComposeSourceError::EmbeddedCredential { project_id: 42, .. }
            ));
        }
    }

    #[test]
    fn validation_accepts_variable_references_in_url_userinfo_and_sensitive_query_parameters() {
        let services = parse_source(
            42,
            r#"
services:
  app:
    image: example/app:1
    environment:
      DATABASE_URL: postgres://${DATABASE_USER}:${DATABASE_PASSWORD}@database/app?password=${DATABASE_PASSWORD}
"#,
        )
        .expect("URL credential references should remain editable");

        assert_eq!(services.len(), 1);
    }

    #[test]
    fn archive_round_trips_without_rewriting_yaml() {
        let content = "services:\n  keycloak:\n    image: quay.io/keycloak/keycloak:26.3.2\n";
        let bytes = create_archive(content, "docker-compose.yml")
            .expect("Compose archive should be created");
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("Compose archive should open");
        let mut entry = archive
            .by_name("docker-compose.yml")
            .expect("Compose entry should exist");
        let mut restored = String::new();
        entry
            .read_to_string(&mut restored)
            .expect("Compose entry should be readable");
        assert_eq!(restored, content);
    }

    #[test]
    fn primary_route_uses_discovered_compose_health_path() {
        let config = PresetConfig::DockerCompose(DockerComposeConfig {
            public_ports: vec![temps_entities::preset::ComposePublicPort {
                service: "browserless".to_string(),
                port: 3000,
                ..Default::default()
            }],
            compose_services: vec![ComposeServiceSnapshot {
                name: "browserless".to_string(),
                health_check_path: Some("/docs".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        assert_eq!(
            effective_health_check_path(42, Some(&config)).expect("valid path"),
            Some("/docs".to_string())
        );
    }

    #[test]
    fn explicit_route_health_path_overrides_discovery_and_is_validated() {
        let config = PresetConfig::DockerCompose(DockerComposeConfig {
            public_ports: vec![temps_entities::preset::ComposePublicPort {
                service: "browserless".to_string(),
                port: 3000,
                health_check_path: Some("/ready".to_string()),
                ..Default::default()
            }],
            compose_services: vec![ComposeServiceSnapshot {
                name: "browserless".to_string(),
                health_check_path: Some("/docs".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(
            effective_health_check_path(42, Some(&config)).expect("valid override"),
            Some("/ready".to_string())
        );

        let invalid = PresetConfig::DockerCompose(DockerComposeConfig {
            public_ports: vec![temps_entities::preset::ComposePublicPort {
                service: "browserless".to_string(),
                port: 3000,
                health_check_path: Some("https://example.com/ready".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(matches!(
            effective_health_check_path(42, Some(&invalid)),
            Err(ComposeSourceError::InvalidHealthCheckPath { project_id: 42, .. })
        ));
    }
}
