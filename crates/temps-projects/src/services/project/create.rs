// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project creation, template-service instantiation and the initial deployment.

use super::*;

pub(super) fn initial_deployment_config(
    request: &CreateProjectRequest,
) -> temps_entities::deployment_config::DeploymentConfig {
    temps_entities::deployment_config::DeploymentConfig {
        cpu_request: request.cpu_request.or(Some(DEFAULT_CPU_REQUEST)),
        cpu_limit: request.cpu_limit,
        memory_request: request.memory_request.or(Some(DEFAULT_MEMORY_REQUEST)),
        memory_limit: request.memory_limit.or(Some(DEFAULT_MEMORY_LIMIT)),
        exposed_port: request.exposed_port,
        automatic_deploy: Some(request.automatic_deploy),
        ..Default::default()
    }
}

impl ProjectService {
    pub async fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<Project, ProjectError> {
        self.create_project_as(request, &SlugClaimCaller::project_writer())
            .await
    }

    /// Create a project on behalf of a caller whose authority is known.
    ///
    /// Only matters for one thing (ADR 045): whether the caller may claim a
    /// slug this host grants host Docker access to. Every other path keeps
    /// [`Self::create_project`], which is fail-closed.
    pub async fn create_project_as(
        &self,
        request: CreateProjectRequest,
        caller: &SlugClaimCaller<'_>,
    ) -> Result<Project, ProjectError> {
        self.create_project_with_identity(request, None, caller, false, None)
            .await
    }

    /// Create a template-backed service project from an immutable resolved
    /// release. Generic project creation cannot supply this identity, so a
    /// caller cannot turn an arbitrary image project into a trusted service.
    pub async fn create_service_project(
        &self,
        request: CreateProjectRequest,
        service_template: temps_core::templates::ServiceTemplateInstance,
    ) -> Result<Project, ProjectError> {
        self.create_service_project_as(
            request,
            service_template,
            &SlugClaimCaller::project_writer(),
        )
        .await
    }

    /// [`Self::create_service_project`] with the caller's slug-claim authority
    /// (ADR 045).
    pub async fn create_service_project_as(
        &self,
        request: CreateProjectRequest,
        service_template: temps_core::templates::ServiceTemplateInstance,
        caller: &SlugClaimCaller<'_>,
    ) -> Result<Project, ProjectError> {
        Self::check_service_release(&request, &service_template)?;
        self.create_project_with_identity(request, Some(service_template), caller, false, None)
            .await
    }

    /// Create one service of a template (ADR-051). An app's services carry
    /// their release; a starter's do not. With `defer_initial_deployment`,
    /// a source-built service does not deploy until
    /// [`Self::queue_initial_deployment`] is called, so a service created
    /// as part of a larger app first gets its project, shared variables and
    /// routes.
    ///
    /// `instantiation_token` is the ownership marker of the instantiation
    /// creating the service (see
    /// [`temps_entities::projects::instantiation_token`]). The project row
    /// is inserted holding its *pending* spelling, so a run that crashes
    /// after the insert can still recognise the project as its own and
    /// discard it; the final token is only written once the project's
    /// environment, variables and service links exist, so it proves a
    /// complete project. `None` for every creation that is not part of an
    /// instantiation.
    pub async fn create_template_service_as(
        &self,
        request: CreateProjectRequest,
        release: Option<temps_core::templates::ServiceTemplateInstance>,
        caller: &SlugClaimCaller<'_>,
        defer_initial_deployment: bool,
        instantiation_token: Option<String>,
    ) -> Result<Project, ProjectError> {
        if let Some(release) = release.as_ref() {
            Self::check_service_release(&request, release)?;
        }
        self.create_project_with_identity(
            request,
            release,
            caller,
            defer_initial_deployment,
            instantiation_token,
        )
        .await
    }

    /// Queue the first deployment of a source-built project whose creation
    /// deferred it. Image projects deploy through their image job instead.
    pub async fn queue_initial_deployment(&self, project_id: i32) -> Result<(), ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("project {project_id} not found")))?;
        if !project.source_type.requires_git_info()
            || project.repo_owner.is_empty()
            || project.repo_name.is_empty()
        {
            return Err(ProjectError::InvalidInput(format!(
                "Project {} does not build from a repository",
                project.slug
            )));
        }
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::Id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("project {} has no environment", project.slug))
            })?;
        self.queue_initial_deployment_job(&project, &environment)
            .await
    }

    async fn create_project_with_identity(
        &self,
        request: CreateProjectRequest,
        service_template: Option<temps_core::templates::ServiceTemplateInstance>,
        caller: &SlugClaimCaller<'_>,
        defer_initial_deployment: bool,
        instantiation_token: Option<String>,
    ) -> Result<Project, ProjectError> {
        let mut request = request;
        if request.source_type == temps_entities::source_type::SourceType::External {
            if request.repo_name.is_some()
                || request.repo_owner.is_some()
                || request.git_provider_connection_id.is_some()
                || request.git_url.is_some()
                || !request.storage_service_ids.is_empty()
                || service_template.is_some()
            {
                return Err(ProjectError::InvalidInput(format!(
                    "Project '{}' is for monitoring only; configure hosting before attaching a repository or runtime services",
                    request.name
                )));
            }
            // The persisted preset is an inert compatibility placeholder. It is
            // never exposed or executed until hosting is explicitly enabled.
            request.preset = "dockerfile".to_string();
            request.preset_config = None;
            request.directory = "/".to_string();
            request.main_branch = "main".to_string();
            request.automatic_deploy = false;
        }
        if request.template_slug.as_deref().is_some_and(|slug| {
            slug.chars().count() > temps_core::templates::MAX_TEMPLATE_SLUG_CHARS
        }) {
            return Err(ProjectError::InvalidInput(format!(
                "Template slug cannot exceed {} characters",
                temps_core::templates::MAX_TEMPLATE_SLUG_CHARS
            )));
        }

        // Reject unusable env vars before the project row exists. Catching this
        // here keeps it a 400 on the request that caused it, instead of a 500
        // from the post-insert finalize step that then rolls the project back.
        if let Some(env_vars) = request.environment_variables.as_ref() {
            for env_var in env_vars {
                if env_var.key.trim().is_empty() {
                    return Err(ProjectError::InvalidInput(
                        "Environment variable names cannot be empty".to_string(),
                    ));
                }
                if env_var.is_secret && env_var.value.is_empty() {
                    return Err(ProjectError::InvalidInput(format!(
                        "Environment variable '{}' is marked as a secret but has no value. \
                         Provide a value or clear the secret flag.",
                        env_var.key
                    )));
                }
            }
        }

        // Verify storage service IDs exist if provided
        if !request.storage_service_ids.is_empty() {
            use temps_entities::external_services;

            // Get count of matching services using SeaORM
            let found_count = external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(request.storage_service_ids.clone()))
                .count(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

            // Verify all IDs were found
            if found_count != request.storage_service_ids.len() as u64 {
                return Err(ProjectError::InvalidInput(
                    "One or more storage service IDs not found".to_string(),
                ));
            }
        }

        // The instance checkbox is read only at creation time. An explicit
        // project choice takes precedence, and neither path modifies older projects.
        let delivery_defaults = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .map(|row| temps_core::AppSettings::from_json(row.data))
            .unwrap_or_default();
        let delivery_choice = chosen_delivery_provider(
            request.delivery_provider.as_deref(),
            request.cloudflare_enabled,
            delivery_defaults.cloudflare_new_projects,
            delivery_defaults.bunny_new_projects,
        )?;
        let delivery_profile_id = match delivery_choice.provider {
            Some(provider) => match self.usable_delivery_profile(provider).await? {
                Ok(profile_id) => Some(profile_id),
                Err(missing) if delivery_choice.explicit => {
                    return Err(ProjectError::InvalidInput(format!(
                        "{missing} before enabling {} for project '{}'",
                        delivery_provider_label(provider),
                        request.name
                    )));
                }
                Err(missing) => {
                    // An inherited default must never make project creation
                    // itself fail: every importer and the starter project rely
                    // on it. The project is created without CDN delivery and
                    // the operator can enable it once the provider is set up.
                    warn!(
                        project_name = %request.name,
                        provider,
                        reason = %missing,
                        "Instance default enables {} delivery for new projects, but it is not usable; creating project '{}' without CDN delivery",
                        delivery_provider_label(provider),
                        request.name
                    );
                    None
                }
            },
            None => None,
        };

        let normalized_directory = normalize_project_directory(&request.directory)?;

        let validated_name = validate_project_name(&request.name)?;
        self.ensure_project_name_available(&validated_name, None)
            .await?;
        let project_slug = if let Some(expected_slug) = request.expected_slug.as_deref() {
            self.validate_expected_project_slug(&validated_name, expected_slug)
                .await?;
            expected_slug.to_string()
        } else {
            self.generate_unique_project_slug(&validated_name).await?
        };
        // ADR 045: claiming a slug this host grants the Docker socket to is
        // claiming host root on every machine that grants it, so it is an
        // admin-only act. Checked here rather than in the handler because the
        // slug can also be *derived* from the display name a line above —
        // a caller never has to name it to claim it.
        self.guard_reserved_slug(&project_slug, caller, None, ReservedSlugChange::Claim)
            .await?;
        let resolved = resolve_preset_selection(
            request.preset.as_str(),
            request.preset_config.as_ref(),
            None,
        )?;
        let preset = resolved.preset;
        let preset_config = resolved.config.clone();
        // An app's image services are service projects: their runtime is the
        // pinned release. Services an app builds from source keep the type
        // their preset gives them (a static frontend stays static); their
        // snapshot still records the release and its database bindings, which
        // `projects_service_template_identity` allows on a server/static row
        // that names its template (checked by `check_service_release`).
        let project_type = if service_template
            .as_ref()
            .and_then(|release| release.spec())
            .is_some_and(|spec| spec.is_image())
        {
            ProjectType::Service
        } else {
            project_type_for_resolved_preset(&resolved)?
        };
        let service_template_json = service_template
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| {
                ProjectError::InvalidInput(format!(
                    "Could not serialize the resolved service template: {error}"
                ))
            })?;

        // Create deployment config with resource and deployment settings.
        // New hosted websites get the conservative "small" profile by default:
        // a scheduling request plus a hard memory limit so a runaway app cannot
        // OOM a small single-node host. Operators can still choose standard,
        // dedicated, or explicit uncapped limits later via deployment settings.
        let deployment_config = Some(initial_deployment_config(&request));
        if let Some(config) = deployment_config.as_ref() {
            config.validate().map_err(|reason| {
                ProjectError::InvalidInput(format!(
                    "Invalid initial deployment configuration for project '{}': {reason}",
                    request.name
                ))
            })?;
        }
        if request.cpu_request.is_some()
            || request.cpu_limit.is_some()
            || request.memory_request.is_some()
            || request.memory_limit.is_some()
        {
            let app_settings = self.config_service.get_settings().await.map_err(|error| {
                ProjectError::Other(format!(
                    "Failed to read instance settings before applying template resources: {error}"
                ))
            })?;
            if let Some(config) = deployment_config.as_ref() {
                Self::enforce_tenant_ceilings(config, &app_settings)?;
            }
        }

        // SSRF guard: validate git_url before persisting (Fix #12).
        if let Some(ref git_url) = request.git_url {
            validate_git_url(git_url).map_err(|e| ProjectError::InvalidGitUrl {
                url: redact_url_password(git_url),
                reason: e.to_string(),
            })?;
        }

        // The row is inserted holding the *pending* spelling of the token and
        // only switched to the final one after it is fully initialized (see
        // the crash-window note below), so the final token never marks a
        // half-initialized project.
        let token_for_diagnosis = instantiation_token.clone();
        let token_for_seal = instantiation_token.clone();
        let pending_instantiation_token = instantiation_token
            .as_deref()
            .map(temps_entities::projects::pending_instantiation_token);

        // Explain a root directory the repository does not have now, rather
        // than as a bare ENOENT from the first build (issue #1350).
        if request.source_type == temps_entities::source_type::SourceType::Git {
            self.verify_root_directory_in_repository(
                None,
                SourceDirectoryTarget {
                    connection_id: request.git_provider_connection_id,
                    is_public_repo: request.is_public_repo.unwrap_or(false),
                    git_url: request.git_url.as_deref(),
                    repo_owner: request.repo_owner.as_deref().unwrap_or_default(),
                    repo_name: request.repo_name.as_deref().unwrap_or_default(),
                    branch: &request.main_branch,
                    directory: &normalized_directory,
                },
            )
            .await?;
        }

        // A token has at most one project. Say so up front, before the
        // project, its environment and its links are created and rolled
        // back; the unique index stays the backstop for a concurrent holder.
        if let Some(token) = token_for_diagnosis.as_deref() {
            if let Some(holder) = self.instantiation_token_holder(token).await {
                return Err(ProjectError::DatabaseError {
                    reason: format!(
                        "Project {} ('{}') already holds instantiation token '{token}'; \
                         not creating a second project for '{}'",
                        holder.id, holder.slug, project_slug
                    ),
                });
            }
        }

        let project = projects::ActiveModel {
            name: Set(validated_name),
            repo_name: Set(request.repo_name.unwrap_or_default()),
            repo_owner: Set(request.repo_owner.unwrap_or_default()),
            directory: Set(normalized_directory),
            main_branch: Set(request.main_branch),
            preset: Set(preset), // Now required, not Option
            preset_config: Set(preset_config),
            deployment_config: Set(deployment_config),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            slug: Set(project_slug.clone()),
            is_public_repo: Set(request.is_public_repo.unwrap_or(false)),
            git_url: Set(request.git_url),
            git_provider_connection_id: Set(request.git_provider_connection_id),
            deleted_at: Set(None),
            last_deployment: Set(None),
            source_type: Set(request.source_type),
            project_type: Set(project_type),
            template_slug: Set(request.template_slug),
            service_template: Set(service_template_json),
            hosting: Set(request.hosting),
            instantiation_token: Set(pending_instantiation_token.clone()),
            ..Default::default()
        };

        // Insert the project. The slug column has a UNIQUE index — if a
        // concurrent request raced us to the same slug, surface a typed
        // SlugConflict (HTTP 409) instead of a generic 500.
        let project_found_db = match self.persist_named_project(project, true).await {
            Ok(model) => model,
            Err(ProjectError::SlugConflict { slug }) => {
                // The token has its own unique index: say so when it is the
                // token that collided, not the slug.
                if let Some(token) = token_for_diagnosis.as_deref() {
                    if let Some(holder) = self.instantiation_token_holder(token).await {
                        return Err(ProjectError::DatabaseError {
                            reason: format!(
                                "Project {} ('{}') already holds instantiation token '{token}'; \
                                 not creating a second project for '{}'",
                                holder.id, holder.slug, slug
                            ),
                        });
                    }
                }
                return Err(ProjectError::SlugConflict { slug });
            }
            Err(e) => return Err(e),
        };

        info!(
            "Created project id={} slug={} preset={}",
            project_found_db.id, project_found_db.slug, project_found_db.preset
        );

        // From here on, the project row exists. If any downstream step
        // fails, hard-delete it (CASCADE cleans up environments, env vars,
        // service links, etc.) before returning so the caller never sees
        // a half-initialized project. This is the manual rollback recommended
        // in CLAUDE.md "Resource Cleanup" — a real txn would require pushing
        // a `&impl ConnectionTrait` through every dependent service, which
        // is a much larger refactor.
        //
        // Crash window. The environment, variables and service links are
        // separate writes, so a process that stops between the INSERT and
        // the end of `finalize_project_creation` leaves a project row that
        // this rollback never saw. The ownership token is therefore written
        // in two steps: the INSERT carries the *pending* token, and only
        // once initialization succeeded is it switched to the final one
        // (`seal_instantiation_token`, a single conditional UPDATE). Whoever
        // finds a project holding the final token may rely on it being
        // complete; one holding the pending token is provably the run's own
        // but unfinished, and is discarded and recreated rather than
        // adopted (`discard_unfinished_instantiation_project`). A crash
        // after the seal and before this call returns loses only the
        // best-effort `ProjectCreated` job below, which was never part of
        // what the token vouches for. The unique index covers both
        // spellings, so at most one project per (instantiation, service)
        // can ever hold either, and a second creation that raced past the
        // lease fails the seal and is rolled back here.
        let project_id = project_found_db.id;
        let mut project_found_db = project_found_db;
        let initialized = match self
            .finalize_project_creation(
                &project_found_db,
                request.environment_variables,
                request.storage_service_ids,
                request.storage_service_claim_ids,
                request.storage_service_claim_user_id,
                delivery_profile_id,
            )
            .await
        {
            Ok(env) => match (
                pending_instantiation_token.as_deref(),
                token_for_seal.as_deref(),
            ) {
                (Some(pending), Some(token)) => self
                    .seal_instantiation_token(project_id, pending, token)
                    .await
                    .map(|()| env),
                _ => Ok(env),
            },
            Err(err) => Err(err),
        };
        let default_environment = match initialized {
            Ok(env) => {
                if let Some(token) = token_for_seal {
                    project_found_db.instantiation_token = Some(token);
                }
                env
            }
            Err(err) => {
                tracing::error!(
                    "Project {} creation failed after insert, rolling back: {}",
                    project_id,
                    err
                );
                if let Err(cleanup_err) = temps_entities::projects::Entity::delete_by_id(project_id)
                    .exec(self.db.as_ref())
                    .await
                {
                    tracing::error!(
                        "Failed to roll back project {} after creation error: {}",
                        project_id,
                        cleanup_err
                    );
                }
                return Err(err);
            }
        };

        // Emit ProjectCreated job
        let project_created_job = Job::ProjectCreated(ProjectCreatedJob {
            project_id: project_found_db.id,
            project_name: project_found_db.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_created_job).await {
            warn!(
                "Failed to emit ProjectCreated job for project {}: {}",
                project_found_db.id, e
            );
        } else {
            info!(
                "Emitted ProjectCreated job for project {}",
                project_found_db.id
            );
        }
        // Queue initial deployment/pipeline job only for Git-based projects with repository information
        // For docker_image and static_files source types, deployments are triggered via API
        if defer_initial_deployment {
            info!(
                "Deferring initial deployment for project {} until its template is assembled",
                project_found_db.id
            );
        } else if project_found_db.hosting == temps_entities::types::Hosting::External {
            info!(
                "Project {} runs outside Temps; it is never deployed",
                project_found_db.id
            );
        } else if project_found_db.source_type.requires_git_info()
            && !project_found_db.repo_owner.is_empty()
            && !project_found_db.repo_name.is_empty()
        {
            info!(
                "Queueing initial deployment job for Git project: {}",
                project_found_db.id
            );

            match self
                .queue_initial_deployment_job(&project_found_db, &default_environment)
                .await
            {
                Ok(()) => {
                    info!(
                        "Successfully queued deployment job for project {}",
                        project_found_db.id
                    );
                }
                Err(e) => {
                    // Log error but don't fail project creation
                    tracing::error!(
                        "Failed to queue deployment job for project {}: {}",
                        project_found_db.id,
                        e
                    );
                }
            }
        } else {
            info!(
                "Skipping initial deployment for project {} (source_type: {})",
                project_found_db.id, project_found_db.source_type
            );
        }

        // Auto-install GitLab webhook if applicable (best-effort, non-fatal).
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_gitlab_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_id, encrypted_token)) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.gitlab_webhook_id = Set(Some(hook_id as i32));
                        active.gitlab_webhook_signing_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist GitLab webhook fields on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install GitLab webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Bitbucket Cloud webhook if applicable (best-effort, non-fatal).
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_bitbucket_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_uuid, encrypted_token)) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.bitbucket_webhook_hook_id = Set(Some(hook_uuid));
                        active.bitbucket_webhook_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist Bitbucket webhook fields on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install Bitbucket webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install a GitHub repository webhook for non-App GitHub
        // connections (best-effort, non-fatal). Without it a PAT/OAuth
        // project never hears about a push (#1596).
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_github_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                        None,
                    )
                    .await
                {
                    Ok(Some((hook_id, encrypted_secret))) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.github_webhook_id = Set(Some(hook_id));
                        active.github_webhook_signing_token = Set(Some(encrypted_secret));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist GitHub webhook fields on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Ok(None) => project_found_db,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install GitHub webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Gitea webhook if applicable (best-effort, non-fatal).
        // BLOCKER 1 fix: gitea_webhook_signing_token was never written.
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_gitea_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok(encrypted_token) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.gitea_webhook_signing_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist Gitea webhook token on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install Gitea webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Generic webhook token if applicable (best-effort, non-fatal).
        // BLOCKER 2 fix: generic_webhook_token was never written.
        // Generic has no remote API — generate the token and store it; operators
        // configure the webhook URL manually.
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            match self
                .install_generic_webhook_token(project_found_db.id, conn_id)
                .await
            {
                Ok(Some(encrypted_token)) => {
                    let mut active = projects::ActiveModel::from(project_found_db.clone());
                    active.generic_webhook_token = Set(Some(encrypted_token));
                    active.updated_at = Set(chrono::Utc::now());
                    match active.update(self.db.as_ref()).await {
                        Ok(updated) => updated,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to persist Generic webhook token on new project {}: {}",
                                project_found_db.id,
                                e
                            );
                            project_found_db
                        }
                    }
                }
                Ok(None) => {
                    // Not a Generic connection — nothing to do.
                    project_found_db
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to install Generic webhook token for new project {}: {}",
                        project_found_db.id,
                        e
                    );
                    project_found_db
                }
            }
        } else {
            project_found_db
        };

        Ok(self.map_written_project(project_found_db).await)
    }

    /// The project holding instantiation token `token`, under either its
    /// final or its pending spelling (soft-deleted ones included: they keep
    /// the token). Best effort, for diagnostics and early refusal; the unique
    /// index is what enforces "at most one".
    async fn instantiation_token_holder(&self, token: &str) -> Option<projects::Model> {
        let spellings = [
            token.to_string(),
            temps_entities::projects::pending_instantiation_token(token),
        ];
        projects::Entity::find()
            .filter(projects::Column::InstantiationToken.is_in(spellings))
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
    }

    /// Switch a project from its pending instantiation token to the final
    /// one, declaring its initialization complete.
    ///
    /// Conditional on the row still holding `pending`, so it can never
    /// re-label a project it did not just create. A unique violation means
    /// another project already holds `token`: this run lost a race it should
    /// not have been in, and the caller rolls its project back.
    async fn seal_instantiation_token(
        &self,
        project_id: i32,
        pending: &str,
        token: &str,
    ) -> Result<(), ProjectError> {
        use sea_orm::sea_query::Expr;
        let result = projects::Entity::update_many()
            .col_expr(projects::Column::InstantiationToken, Expr::value(token))
            .filter(projects::Column::Id.eq(project_id))
            .filter(projects::Column::InstantiationToken.eq(pending))
            .exec(self.db.as_ref())
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) if super::types::is_unique_violation(&error) => {
                let holder = match self.instantiation_token_holder(token).await {
                    Some(holder) => format!("project {} ('{}')", holder.id, holder.slug),
                    None => "another project".to_string(),
                };
                return Err(ProjectError::DatabaseError {
                    reason: format!(
                        "Project {project_id} cannot take instantiation token '{token}': \
                         {holder} already holds it"
                    ),
                });
            }
            Err(error) => {
                return Err(ProjectError::DatabaseError {
                    reason: format!(
                        "Failed to record that project {project_id} finished initializing \
                         (instantiation token '{token}'): {error}"
                    ),
                });
            }
        };
        if result.rows_affected != 1 {
            return Err(ProjectError::DatabaseError {
                reason: format!(
                    "Project {project_id} no longer holds pending instantiation token \
                     '{pending}'; it was not marked as initialized"
                ),
            });
        }
        Ok(())
    }

    /// Hard-delete a project an instantiation began creating and never
    /// finished: its row still holds the *pending* token `pending_token`, so
    /// it has no (or only some of its) environment, variables and service
    /// links, and was never exposed or deployed.
    ///
    /// Conditional on the row still holding that token, so a project that
    /// has since been sealed (or anything else) is never touched. Returns
    /// whether a row was removed. Dependent rows go with it through the same
    /// cascade the in-process rollback of `create_project` relies on.
    pub async fn discard_unfinished_instantiation_project(
        &self,
        project_id: i32,
        pending_token: &str,
    ) -> Result<bool, ProjectError> {
        if !temps_entities::projects::is_pending_instantiation_token(pending_token) {
            return Err(ProjectError::InvalidInput(format!(
                "Refusing to discard project {project_id}: '{pending_token}' is not a pending \
                 instantiation token"
            )));
        }
        let result = projects::Entity::delete_many()
            .filter(projects::Column::Id.eq(project_id))
            .filter(projects::Column::InstantiationToken.eq(pending_token))
            .exec(self.db.as_ref())
            .await
            .map_err(|error| ProjectError::DatabaseError {
                reason: format!(
                    "Failed to discard unfinished project {project_id} \
                     (instantiation token '{pending_token}'): {error}"
                ),
            })?;
        Ok(result.rows_affected > 0)
    }

    /// Post-insert steps for `create_project`. Returns the default environment
    /// on success. On any error, the caller is responsible for rolling back
    /// the project row.
    async fn finalize_project_creation(
        &self,
        project: &projects::Model,
        environment_variables: Option<Vec<CreateProjectEnvVar>>,
        storage_service_ids: Vec<i32>,
        storage_service_claim_ids: Vec<i32>,
        storage_service_claim_user_id: Option<i32>,
        delivery_profile_id: Option<i32>,
    ) -> Result<temps_entities::environments::Model, ProjectError> {
        let project_config = project.deployment_config.as_ref();
        let default_environment = self
            .environment_service
            .create_environment(
                project.id,
                "production".to_string(),
                project_config
                    .and_then(|config| config.cpu_request)
                    .or(Some(DEFAULT_CPU_REQUEST)),
                // The production environment starts with the project's
                // effective profile so its normal precedence does not shadow
                // curated template requirements with platform defaults.
                project_config.and_then(|config| config.cpu_limit),
                project_config
                    .and_then(|config| config.memory_request)
                    .or(Some(DEFAULT_MEMORY_REQUEST)),
                project_config
                    .and_then(|config| config.memory_limit)
                    .or(Some(DEFAULT_MEMORY_LIMIT)),
                project.main_branch.clone(),
            )
            .await
            .map_err(|e| ProjectError::EnvironmentCreationFailed {
                project_id: project.id,
                reason: e.to_string(),
            })?;

        info!(
            "Created default environment for project: {}",
            default_environment.id
        );

        if let Some(env_vars) = environment_variables {
            for env_var in env_vars {
                let CreateProjectEnvVar {
                    key,
                    value,
                    is_secret,
                } = env_var;
                self.env_var_service
                    .create_environment_variable(
                        project.id,
                        vec![default_environment.id],
                        key.clone(),
                        value,
                        is_secret,
                    )
                    .await
                    .map_err(|e| ProjectError::EnvVarCreationFailed {
                        project_id: project.id,
                        key,
                        reason: e.to_string(),
                    })?;
            }
        }

        if !storage_service_ids.is_empty() {
            info!(
                "Linking {} storage services to project {}",
                storage_service_ids.len(),
                project.id
            );
            let claims = storage_service_claim_user_id
                .map(|user_id| {
                    storage_service_claim_ids
                        .into_iter()
                        .map(|service_id| (service_id, user_id))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            self.external_service_manager
                .link_services_to_project_with_claims(&storage_service_ids, project.id, &claims)
                .await
                .map_err(|e| ProjectError::StorageLinksFailed {
                    project_id: project.id,
                    service_ids: storage_service_ids,
                    reason: e.to_string(),
                })?;
        }

        if let Some(profile_id) = delivery_profile_id {
            project_delivery_settings::ActiveModel {
                project_id: Set(project.id),
                default_profile_id: Set(Some(profile_id)),
                updated_at: Set(chrono::Utc::now()),
            }
            .insert(self.db.as_ref())
            .await
            .map_err(|error| ProjectError::DatabaseError {
                reason: format!("Project {} delivery setup failed: {error}", project.id),
            })?;
        }

        Ok(default_environment)
    }

    /// Queue an initial deployment job for a newly created project
    async fn queue_initial_deployment_job(
        &self,
        project: &temps_entities::projects::Model,
        _environment: &temps_entities::environments::Model,
    ) -> Result<(), ProjectError> {
        // Fetch the latest commit from the git provider if connection exists
        let commit_sha = if let Some(connection_id) = project.git_provider_connection_id {
            match self
                .git_provider_manager
                .get_branch_latest_commit(
                    connection_id,
                    &project.repo_owner,
                    &project.repo_name,
                    &project.main_branch,
                )
                .await
            {
                Ok(commit) => {
                    info!(
                        "Fetched latest commit for project {}: {} - {}",
                        project.id, commit.sha, commit.message
                    );
                    commit.sha
                }
                Err(e) => {
                    // Don't fail: deploy the branch tip without a known commit.
                    tracing::warn!(
                        "Failed to fetch latest commit for project {}: {}. Deploying the tip \
                         of branch {} instead.",
                        project.id,
                        e,
                        project.main_branch
                    );
                    String::new()
                }
            }
        } else {
            // No provider API to ask (e.g. a public repository). An empty
            // commit deploys the branch tip, and the download job records the
            // commit it checked out. Never store the symbolic "HEAD": the
            // console would show it as the commit, and checkout would treat
            // it as a commit and clone the full history to resolve it.
            String::new()
        };

        // Create a GitPushEvent job to trigger the initial deployment
        // The deployment service's job processor will handle creating the pipeline and deployment
        let git_push_job = temps_core::GitPushEventJob {
            owner: project.repo_owner.clone(),
            repo: project.repo_name.clone(),
            branch: Some(project.main_branch.clone()),
            tag: None, // No tag for initial deployment
            commit: commit_sha.clone(),
            project_id: project.id, // Include project_id
            // Initial deployment is a user-initiated event (project creation),
            // not a git webhook — bypass automatic_deploy.
            manual_trigger: true,
            rollback_from_deployment_id: None,
            // Infer the target from the branch at creation time (the default
            // environment tracks main_branch).
            target_environment_id: None,
            recovery_of_deployment_id: None,
        };

        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| ProjectError::Other(format!("Failed to queue deployment job: {}", e)))?;

        info!(
            "Queued GitPushEvent job for initial deployment of project {} (owner: {}, repo: {}, branch: {}, commit: {})",
            project.id,
            &project.repo_owner,
            &project.repo_name,
            project.main_branch,
            commit_sha
        );

        Ok(())
    }
}
