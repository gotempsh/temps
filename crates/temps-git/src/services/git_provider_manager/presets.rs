// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preset detection on connected repositories: stored and live presets,
//! `.env.example` variables, Compose services and previews, directory checks.

use super::*;

const MAX_DOCKERFILES_TO_SCAN: usize = 16;

impl GitProviderManager {
    /// Calculate and store preset for a repository
    pub async fn calculate_and_store_preset(
        &self,
        repo_id: i32,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Verify repository exists
        let _ = repositories::Entity::find_by_id(repo_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Repository {} not found",
                    repo_id
                ))
            })?;

        // Verify the connection exists (connections are shared across all
        // authenticated users -- see `get_repository_for_user` -- so no
        // owning-user check is needed here beyond confirming it's real).
        let _ = self.get_connection(connection_id).await?;

        // Calculate preset (this will automatically cache it in the database)
        let _ = self
            .calculate_repository_preset_live(repo_id, None) // Use default branch
            .await?;

        Ok(())
    }

    /// Get repository files for preset detection
    async fn get_repository_files(
        &self,
        provider_service: &Arc<dyn GitProviderService>,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<String>, GitProviderError> {
        match provider_service.provider_type() {
            GitProviderType::GitHub | GitProviderType::GitLab => {
                provider_service
                    .list_repository_files(access_token, owner, repo, branch)
                    .await
            }
            _ => {
                // For other providers, return empty list for now
                Ok(Vec::new())
            }
        }
    }

    /// Get repository preset from database by branch
    /// If branch is not specified, uses the repository's default branch
    pub async fn get_repository_preset(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: Option<String>,
    ) -> Result<Option<serde_json::Value>, GitProviderManagerError> {
        // Find repository
        let repository = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(repo))
            .one(self.db.as_ref())
            .await?;

        let repository = match repository {
            Some(r) => r,
            None => return Ok(None),
        };

        // Use provided branch or fall back to default branch
        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        // Get preset cache and extract data for the specific branch
        let preset_data = repository.preset.as_ref().and_then(|json_cache| {
            // Deserialize Json to RepositoryPresetCache
            let cache: repositories::RepositoryPresetCache =
                serde_json::from_value(json_cache.clone()).ok()?;
            cache
                .branches
                .get(&target_branch)
                .map(|branch_data| json!(branch_data))
        });

        Ok(preset_data)
    }

    /// Check that `directory` exists in `owner/repo` at `branch`, through the
    /// connection's provider API.
    ///
    /// Never fails: anything that prevents a definite answer (the connection
    /// or provider cannot be loaded, the provider has no listing API, the API
    /// is unreachable or slow) is
    /// [`RepositoryDirectoryCheck::Unverified`](crate::services::repository_directory::RepositoryDirectoryCheck::Unverified),
    /// so a flaky provider never blocks saving a project. The whole walk is
    /// bounded by `REPOSITORY_DIRECTORY_CHECK_TIMEOUT` and issues one listing
    /// per path component.
    pub async fn check_repository_directory(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
        directory: &str,
    ) -> crate::services::repository_directory::RepositoryDirectoryCheck {
        use crate::services::repository_directory::{
            check_repository_directory, DirectoryListingEntry, RepositoryDirectoryCheck,
            REPOSITORY_DIRECTORY_CHECK_TIMEOUT,
        };

        let walk = async {
            let provider_service = match self.get_connection(connection_id).await {
                Ok(connection) => match self.get_provider_service(connection.provider_id).await {
                    Ok(service) => service,
                    Err(error) => {
                        return RepositoryDirectoryCheck::Unverified {
                            reason: format!(
                                "the Git provider of connection {connection_id} could not be \
                                 loaded: {error}"
                            ),
                        }
                    }
                },
                Err(error) => {
                    return RepositoryDirectoryCheck::Unverified {
                        reason: format!(
                            "Git connection {connection_id} could not be loaded: {error}"
                        ),
                    }
                }
            };
            if matches!(provider_service.provider_type(), GitProviderType::Generic) {
                return RepositoryDirectoryCheck::Unverified {
                    reason: "generic Git providers have no API to list repository directories"
                        .to_string(),
                };
            }
            check_repository_directory(directory, |path| {
                let provider_service = provider_service.clone();
                async move {
                    self.execute_with_refresh(connection_id, |access_token| {
                        let provider_service = provider_service.clone();
                        let path = path.clone();
                        async move {
                            provider_service
                                .list_directory(&access_token, owner, repo, &path, Some(branch))
                                .await
                        }
                    })
                    .await
                    .map(|entries| {
                        entries
                            .into_iter()
                            .map(|entry| DirectoryListingEntry {
                                name: entry.name,
                                is_dir: entry.is_dir,
                            })
                            .collect::<Vec<_>>()
                    })
                }
            })
            .await
        };
        match tokio::time::timeout(REPOSITORY_DIRECTORY_CHECK_TIMEOUT, walk).await {
            Ok(check) => check,
            Err(_) => RepositoryDirectoryCheck::Unverified {
                reason: format!(
                    "listing {owner}/{repo} at '{branch}' did not finish within {}s",
                    REPOSITORY_DIRECTORY_CHECK_TIMEOUT.as_secs()
                ),
            },
        }
    }

    /// Calculate repository preset in real-time without storing it
    pub async fn calculate_repository_preset_live(
        &self,
        repository_id: i32,
        branch: Option<String>,
    ) -> Result<RepositoryPresetDomain, GitProviderManagerError> {
        let repository = self.get_repository_for_user(repository_id).await?;

        // Repository always has a git provider connection (required field)
        let connection_id = repository.git_provider_connection_id;

        let provider_service = {
            let connection = self.get_connection(connection_id).await?;
            self.get_provider_service(connection.provider_id).await?
        };

        // Use provided branch or fall back to repository's default branch
        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        // Get all files in the repository.
        //
        // Routed through `execute_with_refresh` so an expired-but-refreshable
        // access token is refreshed and retried instead of surfacing a 401 to
        // the user. This previously decrypted the token inline and called
        // directly, so preset detection was the one provider path that never
        // got the refresh treatment — and because a provider 401 was also
        // flattened into a generic `ApiError`, the retry could not fire. The
        // provider implementation now preserves typed authentication errors.
        let files = self
            .execute_with_refresh(connection_id, |access_token| {
                let provider_service = provider_service.clone();
                let owner = repository.owner.clone();
                let name = repository.name.clone();
                let target_branch = target_branch.clone();
                async move {
                    self.get_repository_files(
                        &provider_service,
                        &access_token,
                        &owner,
                        &name,
                        &target_branch,
                    )
                    .await
                }
            })
            .await?;

        // Detect presets in root and subdirectories
        let mut presets = self.detect_presets_in_directories(&files).await;
        self.enrich_dockerfile_exposed_ports(
            &mut presets,
            connection_id,
            &provider_service,
            &repository.owner,
            &repository.name,
            &target_branch,
        )
        .await;

        // Cache presets in repositories.preset as HashMap<branch, preset_data>
        let calculated_at = chrono::Utc::now();

        // Get existing preset cache or create new one
        let mut preset_cache: repositories::RepositoryPresetCache = repository
            .preset
            .as_ref()
            .and_then(|json| serde_json::from_value(json.clone()).ok())
            .unwrap_or_else(|| repositories::RepositoryPresetCache {
                branches: std::collections::HashMap::new(),
            });

        // Convert ProjectPresetDomain to PresetInfo
        let preset_infos: Vec<repositories::PresetInfo> = presets
            .iter()
            .map(|p| repositories::PresetInfo {
                path: p.path.clone(),
                preset: p.preset.clone(),
                preset_label: p.preset_label.clone(),
                exposed_port: p.exposed_port,
                icon_url: p.icon_url.clone(),
                project_type: p.project_type.clone(),
                compose_files: p.compose_files.clone(),
                dockerfile_path: p.dockerfile_path.clone(),
            })
            .collect();

        // Update cache for this branch
        preset_cache.branches.insert(
            target_branch.clone(),
            repositories::BranchPresetData {
                presets: preset_infos,
                calculated_at,
            },
        );

        // Serialize cache to Json for database storage
        let preset_json = serde_json::to_value(&preset_cache).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to serialize preset cache: {}",
                e
            ))
        })?;

        // Update repository with new cache
        let mut repo_update: repositories::ActiveModel = repository.clone().into();
        repo_update.preset = Set(Some(preset_json));
        repo_update.update(self.db.as_ref()).await?;

        Ok(RepositoryPresetDomain {
            repository_id,
            owner: repository.owner,
            name: repository.name,
            presets,
            calculated_at,
        })
    }

    /// Detect and parse a `.env.example`-style file for a repository, live
    /// (not cached — this is a one-shot lookup for the New Project wizard,
    /// unlike preset detection which persists to `repositories.preset`).
    pub async fn calculate_repository_env_example_live(
        &self,
        repository_id: i32,
        branch: Option<String>,
        root_directory: &str,
    ) -> Result<RepositoryEnvExampleDomain, GitProviderManagerError> {
        let repository = self.get_repository_for_user(repository_id).await?;

        let connection_id = repository.git_provider_connection_id;
        let provider_service = {
            let connection = self.get_connection(connection_id).await?;
            self.get_provider_service(connection.provider_id).await?
        };

        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        let files = self
            .execute_with_refresh(connection_id, |access_token| {
                let provider_service = provider_service.clone();
                let owner = repository.owner.clone();
                let name = repository.name.clone();
                let target_branch = target_branch.clone();
                async move {
                    self.get_repository_files(
                        &provider_service,
                        &access_token,
                        &owner,
                        &name,
                        &target_branch,
                    )
                    .await
                }
            })
            .await?;

        let Some(env_path) =
            temps_presets::detect_env_example_files_in_directory(&files, root_directory)
                .into_iter()
                .next()
        else {
            return Ok(RepositoryEnvExampleDomain {
                repository_id,
                path: None,
                variables: Vec::new(),
            });
        };

        let file = self
            .execute_with_refresh(connection_id, |access_token| {
                let provider_service = provider_service.clone();
                let owner = repository.owner.clone();
                let name = repository.name.clone();
                let target_branch = target_branch.clone();
                let env_path = env_path.clone();
                async move {
                    provider_service
                        .get_file_content(
                            &access_token,
                            &owner,
                            &name,
                            &env_path,
                            Some(&target_branch),
                        )
                        .await
                }
            })
            .await?;

        let content = decode_file_content(&file.content, &file.encoding);
        let variables = temps_presets::parse_env_example(&content)
            .into_iter()
            .map(|v| EnvExampleVariableDomain {
                key: v.key,
                default_value: v.default_value,
                description: v.description,
            })
            .collect();

        Ok(RepositoryEnvExampleDomain {
            repository_id,
            path: Some(env_path),
            variables,
        })
    }

    /// Parses a compose file's services for the New Project wizard, live (not
    /// cached — same one-shot-lookup model as `calculate_repository_env_example_live`).
    /// Unlike env-example detection, the compose path is already known by the
    /// caller (it comes from the `compose_files` list `/preset/live` already
    /// returned), so this skips straight to fetching that one file's content.
    pub async fn calculate_repository_compose_services_live(
        &self,
        repository_id: i32,
        branch: Option<String>,
        path: String,
    ) -> Result<RepositoryComposeServicesDomain, GitProviderManagerError> {
        let repository = self.get_repository_for_user(repository_id).await?;

        let connection_id = repository.git_provider_connection_id;
        let provider_service = {
            let connection = self.get_connection(connection_id).await?;
            self.get_provider_service(connection.provider_id).await?
        };

        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        let file = self
            .execute_with_refresh(connection_id, |access_token| {
                let provider_service = provider_service.clone();
                let owner = repository.owner.clone();
                let name = repository.name.clone();
                let target_branch = target_branch.clone();
                let path = path.clone();
                async move {
                    provider_service
                        .get_file_content(&access_token, &owner, &name, &path, Some(&target_branch))
                        .await
                }
            })
            .await?;

        let content = decode_file_content(&file.content, &file.encoding);
        let services = temps_presets::list_compose_services(&content)
            .map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Compose file '{}' could not be parsed: {}",
                    path, e
                ))
            })?
            .into_iter()
            .map(|s| ComposeServicePreviewDomain {
                name: s.name,
                image: s.image,
                depends_on: s.depends_on,
                environment_variables: s.environment_variables,
                looks_like_database: s.looks_like_database,
                detected_service_type: s.detected_service_type,
                ports: s.ports,
            })
            .collect();

        Ok(RepositoryComposeServicesDomain {
            repository_id,
            path,
            services,
        })
    }

    /// Read one text file from a connected repository through the provider
    /// API, at a branch name or commit SHA. No checkout is made.
    pub async fn read_repository_text_file(
        &self,
        connection_id: i32,
        owner: &str,
        name: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<String, GitProviderManagerError> {
        let provider_service = {
            let connection = self.get_connection(connection_id).await?;
            self.get_provider_service(connection.provider_id).await?
        };
        let file = self
            .execute_with_refresh(connection_id, |access_token| {
                let provider_service = provider_service.clone();
                let owner = owner.to_string();
                let name = name.to_string();
                let path = path.to_string();
                let git_ref = git_ref.to_string();
                async move {
                    provider_service
                        .get_file_content(&access_token, &owner, &name, &path, Some(&git_ref))
                        .await
                }
            })
            .await?;
        Ok(decode_file_content(&file.content, &file.encoding))
    }

    /// Fetch a Compose file from a connected repository at `branch` (the
    /// repository's default branch when `None`).
    pub async fn fetch_repository_compose_source(
        &self,
        repository_id: i32,
        branch: Option<String>,
        path: String,
    ) -> Result<RepositoryComposeSourceDomain, GitProviderManagerError> {
        let repository = self.get_repository_for_user(repository_id).await?;
        let git_ref = branch.unwrap_or_else(|| repository.default_branch.clone());
        let content = self
            .read_repository_text_file(
                repository.git_provider_connection_id,
                &repository.owner,
                &repository.name,
                &path,
                &git_ref,
            )
            .await?;
        Ok(RepositoryComposeSourceDomain {
            repository_id,
            connection_id: repository.git_provider_connection_id,
            owner: repository.owner,
            name: repository.name,
            git_ref,
            path,
            content,
        })
    }

    /// Render the effective Compose document for the settings mini editor.
    /// The presets layer removes disabled services, applies the inline
    /// override, and redacts sensitive values before this returns.
    pub fn render_repository_compose_preview(
        source: &RepositoryComposeSourceDomain,
        compose_override: Option<&str>,
        excluded_services: &[String],
        preview_policy: &temps_entities::compose_security::ComposeSecurityPolicy,
    ) -> Result<RepositoryComposePreviewDomain, GitProviderManagerError> {
        let preview = temps_presets::render_effective_compose_preview_with_policy(
            &source.content,
            compose_override,
            excluded_services,
            preview_policy,
        )
        .map_err(|error| GitProviderManagerError::ComposePreview {
            path: source.path.clone(),
            source: error,
        })?;
        Ok(RepositoryComposePreviewDomain {
            repository_id: source.repository_id,
            path: source.path.clone(),
            preview,
        })
    }

    /// Fetch and render the effective Compose document for the settings mini
    /// editor. See [`Self::render_repository_compose_preview`].
    pub async fn calculate_repository_compose_preview_live(
        &self,
        repository_id: i32,
        branch: Option<String>,
        path: String,
        compose_override: Option<String>,
        excluded_services: Vec<String>,
        preview_policy: temps_entities::compose_security::ComposeSecurityPolicy,
    ) -> Result<RepositoryComposePreviewDomain, GitProviderManagerError> {
        let source = self
            .fetch_repository_compose_source(repository_id, branch, path)
            .await?;
        Self::render_repository_compose_preview(
            &source,
            compose_override.as_deref(),
            &excluded_services,
            &preview_policy,
        )
    }

    /// Detect presets in directories with proper grouping and filtering
    async fn detect_presets_in_directories(&self, files: &[String]) -> Vec<ProjectPresetDomain> {
        // Use the centralized file tree detection from temps-presets
        let detected_presets = temps_presets::detect_presets_from_file_tree(files);

        // Convert to ProjectPresetDomain
        detected_presets
            .into_iter()
            .map(|preset| {
                let runtime_preset = temps_presets::get_preset_by_slug(&preset.slug);
                let preset_enum = runtime_preset
                    .as_ref()
                    .and_then(|preset| preset.stored_preset());

                // Prefer temps-presets metadata (covers nixpacks-* UI slugs); fall back to entity.
                let exposed_port = preset
                    .exposed_port
                    .or_else(|| preset_enum.as_ref().and_then(|p| p.exposed_port()));

                let icon_url = runtime_preset
                    .as_ref()
                    .map(|preset| preset.icon_url())
                    .or_else(|| {
                        preset_enum
                            .as_ref()
                            .and_then(|p| p.icon_url())
                            .map(|s| s.to_string())
                    });

                let project_type = runtime_preset
                    .as_ref()
                    .map(|preset| preset.project_type().to_string())
                    .or_else(|| preset_enum.as_ref().map(|p| p.project_type().to_string()))
                    .unwrap_or_else(|| "unknown".to_string());

                ProjectPresetDomain {
                    path: preset.path,
                    preset: preset.slug,
                    preset_label: preset.label,
                    exposed_port,
                    icon_url,
                    project_type,
                    compose_files: preset.compose_files,
                    dockerfile_path: preset.dockerfile_path,
                }
            })
            .collect()
    }

    /// Read a bounded number of detected Dockerfiles and attach their final
    /// stage's primary `EXPOSE` port to preset metadata. Detection remains
    /// best-effort: a provider read failure must not hide an otherwise valid
    /// preset or prevent project creation.
    async fn enrich_dockerfile_exposed_ports(
        &self,
        presets: &mut [ProjectPresetDomain],
        connection_id: i32,
        provider_service: &Arc<dyn GitProviderService>,
        owner: &str,
        repository_name: &str,
        target_branch: &str,
    ) {
        let dockerfiles: Vec<(usize, String)> = presets
            .iter()
            .enumerate()
            .filter(|(_, preset)| preset.preset == "dockerfile")
            .take(MAX_DOCKERFILES_TO_SCAN)
            .map(|(index, preset)| {
                let path = if preset.path == "./" || preset.path.is_empty() {
                    "Dockerfile".to_string()
                } else {
                    format!("{}/Dockerfile", preset.path.trim_end_matches('/'))
                };
                (index, path)
            })
            .collect();

        for (preset_index, dockerfile_path) in dockerfiles {
            let file = self
                .execute_with_refresh(connection_id, |access_token| {
                    let provider_service = provider_service.clone();
                    let owner = owner.to_string();
                    let repository_name = repository_name.to_string();
                    let target_branch = target_branch.to_string();
                    let dockerfile_path = dockerfile_path.clone();
                    async move {
                        provider_service
                            .get_file_content(
                                &access_token,
                                &owner,
                                &repository_name,
                                &dockerfile_path,
                                Some(&target_branch),
                            )
                            .await
                    }
                })
                .await;

            match file {
                Ok(file) => {
                    let content = decode_file_content(&file.content, &file.encoding);
                    presets[preset_index].exposed_port =
                        temps_presets::detect_primary_exposed_port(&content);
                }
                Err(error) => warn!(
                    owner,
                    repository = repository_name,
                    branch = target_branch,
                    dockerfile = dockerfile_path,
                    error = %error,
                    "Could not inspect Dockerfile EXPOSE metadata during preset detection"
                ),
            }
        }
    }
}

/// Decode a provider `FileContent`. GitHub and GitLab both return base64
/// (with embedded newlines in GitHub's case); falls back to the raw string
/// if decoding fails or the encoding isn't base64, so the caller still gets
/// *something* rather than an error over a cosmetic decode issue.
fn decode_file_content(content: &str, encoding: &str) -> String {
    use base64::Engine;
    if encoding.eq_ignore_ascii_case("base64") {
        let stripped: String = content.split_whitespace().collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(stripped) {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;

    async fn mock_manager_with_compose_file(
        content: &str,
        expected_path: &str,
        expected_branch: &str,
    ) -> (
        GitProviderManager,
        mockito::ServerGuard,
        mockito::Mock,
        mockito::Mock,
    ) {
        use crate::services::git_provider::{AuthMethod, GitProviderService};
        use crate::services::github_provider::GitHubProvider;
        use base64::Engine;
        use sea_orm::{DatabaseBackend, MockDatabase};

        let mut server = mockito::Server::new_async().await;
        let validate_token = server
            .mock("GET", "/rate_limit")
            .match_header("authorization", "Bearer test-access-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
            .create_async()
            .await;
        let file_content = server
            .mock(
                "GET",
                format!("/repos/example-owner/example-repository/contents/{expected_path}")
                    .as_str(),
            )
            .match_query(mockito::Matcher::UrlEncoded(
                "ref".to_string(),
                expected_branch.to_string(),
            ))
            .match_header("authorization", "Bearer test-access-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "path": expected_path,
                    "content": base64::engine::general_purpose::STANDARD.encode(content),
                    "encoding": "base64"
                })
                .to_string(),
            )
            .create_async()
            .await;

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key should be valid"),
        );
        let mut connection = connection_fixture(11, Some(5));
        connection.access_token = Some(
            encryption_service
                .encrypt_string("test-access-token")
                .expect("test access token should encrypt"),
        );
        let repository = repository_fixture(connection.id);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[repository]])
                .append_query_results([[connection.clone()]])
                .append_query_results([[connection]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );
        let provider: Arc<dyn GitProviderService> = Arc::new(GitHubProvider::new(
            Some(server.url()),
            AuthMethod::PersonalAccessToken {
                token: "unused-constructor-token".to_string(),
            },
        ));
        manager.providers_cache.write().await.insert(7, provider);

        (manager, server, validate_token, file_content)
    }

    #[tokio::test]
    async fn connected_compose_preview_fetches_custom_ref_and_renders_safe_effective_yaml() {
        let compose = r#"
services:
  web:
    image: example/web:latest
    depends_on:
      - db
    environment:
      API_TOKEN: repository-secret
    ports:
      - "3000:3000"
  db:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD: database-secret
"#;
        let (manager, _server, validate_token, file_content) = mock_manager_with_compose_file(
            compose,
            "deploy/compose.preview.yml",
            "feature/compose",
        )
        .await;

        let result = manager
            .calculate_repository_compose_preview_live(
                42,
                Some("feature/compose".to_string()),
                "deploy/compose.preview.yml".to_string(),
                Some(
                    "services:\n  web:\n    environment:\n      LOG_LEVEL: debug\n    ports:\n      - \"15455:80\"\n"
                        .to_string(),
                ),
                vec!["db".to_string()],
                Default::default(),
            )
            .await
            .expect("connected Compose preview should render");

        assert_eq!(result.repository_id, 42);
        assert_eq!(result.path, "deploy/compose.preview.yml");
        assert_eq!(result.preview.enabled_services, ["web"]);
        assert_eq!(result.preview.disabled_services, ["db"]);
        assert!(result.preview.yaml.contains("15455:80"));
        assert!(!result.preview.yaml.contains("3000:3000"));
        assert!(!result.preview.yaml.contains("repository-secret"));
        assert!(!result.preview.yaml.contains("database-secret"));
        assert!(!result.preview.yaml.contains("POSTGRES_PASSWORD"));
        assert!(result.preview.yaml.contains("API_TOKEN: <redacted>"));
        assert!(result.preview.yaml.contains("LOG_LEVEL: <redacted>"));
        validate_token.assert_async().await;
        file_content.assert_async().await;
    }

    #[tokio::test]
    async fn connected_compose_preview_reports_custom_path_for_invalid_yaml() {
        let (manager, _server, validate_token, file_content) = mock_manager_with_compose_file(
            "services: [not a mapping",
            "ops/custom.compose.yaml",
            "release-candidate",
        )
        .await;

        let error = manager
            .calculate_repository_compose_preview_live(
                42,
                Some("release-candidate".to_string()),
                "ops/custom.compose.yaml".to_string(),
                None,
                Vec::new(),
                Default::default(),
            )
            .await
            .expect_err("invalid repository YAML must fail preview rendering");

        let message = error.to_string();
        assert!(message.contains("ops/custom.compose.yaml"), "{message}");
        assert!(message.contains("could not be rendered"), "{message}");
        validate_token.assert_async().await;
        file_content.assert_async().await;
    }

    #[tokio::test]
    async fn connected_preset_detection_uses_selected_branch_and_preserves_read_failures() {
        use crate::services::git_provider::{AuthMethod, GitProviderService};
        use crate::services::github_provider::GitHubProvider;
        use base64::Engine;
        use sea_orm::{DatabaseBackend, MockDatabase};

        let mut server = mockito::Server::new_async().await;
        let validate_token = server
            .mock("GET", "/rate_limit")
            .match_header("authorization", "Bearer test-access-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
            .expect(3)
            .create_async()
            .await;
        let tree = server
            .mock(
                "GET",
                "/repos/example-owner/example-repository/git/trees/release-port",
            )
            .match_query(mockito::Matcher::UrlEncoded(
                "recursive".to_string(),
                "1".to_string(),
            ))
            .match_header("authorization", "Bearer test-access-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"tree":[{"path":"Dockerfile","type":"blob"},{"path":"apps/api/Dockerfile","type":"blob"}]}"#,
            )
            .create_async()
            .await;
        let missing_nested_dockerfile = server
            .mock(
                "GET",
                "/repos/example-owner/example-repository/contents/apps/api/Dockerfile",
            )
            .match_query(mockito::Matcher::UrlEncoded(
                "ref".to_string(),
                "release-port".to_string(),
            ))
            .match_header("authorization", "Bearer test-access-token")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"simulated missing file"}"#)
            .create_async()
            .await;
        let dockerfile = server
            .mock(
                "GET",
                "/repos/example-owner/example-repository/contents/Dockerfile",
            )
            .match_query(mockito::Matcher::UrlEncoded(
                "ref".to_string(),
                "release-port".to_string(),
            ))
            .match_header("authorization", "Bearer test-access-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "path": "Dockerfile",
                    "content": base64::engine::general_purpose::STANDARD
                        .encode("FROM alpine\nEXPOSE 4321\n"),
                    "encoding": "base64"
                })
                .to_string(),
            )
            .create_async()
            .await;

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key should be valid"),
        );
        let mut connection = connection_fixture(11, Some(5));
        connection.access_token = Some(
            encryption_service
                .encrypt_string("test-access-token")
                .expect("test access token should encrypt"),
        );
        let repository = repository_fixture(connection.id);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[repository.clone()]])
                .append_query_results([[connection.clone()]])
                .append_query_results([[connection.clone()]])
                .append_query_results([[connection.clone()]])
                .append_query_results([[connection]])
                .append_query_results([[repository]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );
        let provider: Arc<dyn GitProviderService> = Arc::new(GitHubProvider::new(
            Some(server.url()),
            AuthMethod::PersonalAccessToken {
                token: "unused-constructor-token".to_string(),
            },
        ));
        manager.providers_cache.write().await.insert(7, provider);

        let result = manager
            .calculate_repository_preset_live(42, Some("release-port".to_string()))
            .await
            .expect("connected preset detection should succeed");

        assert_eq!(result.presets.len(), 2);
        let root = result
            .presets
            .iter()
            .find(|preset| preset.path == "./")
            .expect("root Dockerfile preset should remain available");
        let nested = result
            .presets
            .iter()
            .find(|preset| preset.path == "apps/api")
            .expect("unreadable nested Dockerfile preset should remain available");
        assert_eq!(root.exposed_port, Some(4321));
        assert_eq!(nested.exposed_port, None);
        validate_token.assert_async().await;
        tree.assert_async().await;
        dockerfile.assert_async().await;
        missing_nested_dockerfile.assert_async().await;
    }
}
