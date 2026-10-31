// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project, deployment and Compose-security settings updates, and route
//! reload signalling.

use super::*;

impl ProjectService {
    pub async fn compose_security_policy(
        &self,
        project_id: i32,
    ) -> Result<
        temps_entities::compose_security::ComposeSecurityPolicy,
        super::compose_security::ComposeSecurityError,
    > {
        super::compose_security::read(self.db.as_ref(), project_id).await
    }

    pub async fn compose_security_legacy_migration_pending(
        &self,
        project_id: i32,
    ) -> Result<bool, super::compose_security::ComposeSecurityError> {
        super::compose_security::legacy_migration_pending(self.db.as_ref(), project_id).await
    }

    pub async fn update_compose_security_policy(
        &self,
        project_id: i32,
        actor: i32,
        policy: temps_entities::compose_security::ComposeSecurityPolicy,
        acknowledged: bool,
        expected_policy: temps_entities::compose_security::ComposeSecurityPolicy,
        acknowledge_legacy_migration: bool,
    ) -> Result<
        temps_entities::compose_security::ComposeSecurityPolicy,
        super::compose_security::ComposeSecurityError,
    > {
        super::compose_security::update(
            self.db.as_ref(),
            project_id,
            actor,
            policy,
            acknowledged,
            expected_policy,
            acknowledge_legacy_migration,
        )
        .await
    }

    pub async fn update_project(
        &self,
        project_id: i32,
        request: CreateProjectRequest,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        // Find the existing project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))?;
        if project.project_type == ProjectType::Service {
            return Err(ProjectError::InvalidInput(
                "Service projects must be configured through the service runtime and template update controls"
                    .to_string(),
            ));
        }

        // ADR 045: this method unconditionally rewrites repo_owner, repo_name,
        // directory, main_branch and preset/preset_config below -- the same
        // source-definition fields `update_project_settings_as` gates -- but
        // took no caller at all until this was found to bypass that guard
        // entirely through a sibling handler. The guard is therefore
        // unconditional here too, since every call rewrites every field.
        self.guard_granted_project_write(
            &project.slug,
            "the source repository, branch, directory or build preset",
            caller,
        )?;

        let normalized_directory = normalize_project_directory(&request.directory)?;

        if project.source_type == temps_entities::source_type::SourceType::Git {
            let target = SourceDirectoryTarget {
                connection_id: project.git_provider_connection_id,
                is_public_repo: project.is_public_repo,
                git_url: project.git_url.as_deref(),
                repo_owner: request.repo_owner.as_deref().unwrap_or("unknown"),
                repo_name: request.repo_name.as_deref().unwrap_or("unknown"),
                branch: &request.main_branch,
                directory: &normalized_directory,
            };
            if target.differs_from(&project) {
                self.verify_root_directory_in_repository(Some(project_id), target)
                    .await?;
            }
        }

        let resolved = resolve_preset_selection(
            request.preset.as_str(),
            request.preset_config.as_ref(),
            project.preset_config.as_ref(),
        )?;

        let name = validate_project_name(&request.name)?;
        if name.to_lowercase() != project.name.to_lowercase() {
            self.ensure_project_name_available(&name, Some(project_id))
                .await?;
        }

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.name = Set(name);
        active_project.repo_name = Set(request.repo_name.unwrap_or_else(|| "unknown".to_string()));
        active_project.repo_owner =
            Set(request.repo_owner.unwrap_or_else(|| "unknown".to_string()));
        active_project.directory = Set(normalized_directory);
        active_project.main_branch = Set(request.main_branch);
        apply_resolved_preset(&mut active_project, resolved)?;
        active_project.updated_at = Set(chrono::Utc::now());

        let project_found = self.persist_named_project(active_project, false).await?;

        // Emit ProjectUpdated before reading anything else. Everything after
        // the commit is a fresh await point, and a cancelled request (client
        // disconnect, timeout) drops the task there — so the notification goes
        // out first, and only the response can be lost.
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: project_found.id,
            project_name: project_found.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                project_found.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {}",
                project_found.id
            );
        }

        Ok(self.map_written_project(project_found).await)
    }

    pub async fn update_project_settings(
        &self,
        project_id: i32,
        params: UpdateProjectSettingsParams,
    ) -> Result<ProjectSettingsUpdate, ProjectError> {
        self.update_project_settings_as(project_id, params, &SlugClaimCaller::project_writer())
            .await
    }

    /// [`Self::update_project_settings`] for a caller whose authority is known.
    ///
    /// Only the *claim* is gated (ADR 045): a project whose slug already
    /// matches the host grant keeps working through every update that does not
    /// change it, whoever sends them.
    pub async fn update_project_settings_as(
        &self,
        project_id: i32,
        params: UpdateProjectSettingsParams,
        caller: &SlugClaimCaller<'_>,
    ) -> Result<ProjectSettingsUpdate, ProjectError> {
        let UpdateProjectSettingsParams {
            name: new_name,
            slug: new_slug,
            git_provider_connection_id,
            main_branch,
            repo_owner,
            repo_name,
            preset,
            directory,
            attack_mode,
            shield_mode,
            enable_preview_environments,
            preview_envs_on_demand,
            preview_envs_idle_timeout_seconds,
            preview_envs_wake_timeout_seconds,
            preset_config,
            ai_alert_summaries_enabled,
            cross_project_trace_sharing,
            serves_own_temps_api,
            error_source_context_enabled,
            vulnerability_scanning_enabled,
            error_source_root,
            ai_api_traffic_summary_enabled,
            image_retention_hours,
            log_ingest_lines_per_sec,
            log_ingest_kib_per_sec,
        } = params;

        // Validate preview env on-demand timeouts before touching the DB.
        // Mirrors DeploymentConfig::validate so the project-level defaults are
        // never out of range.
        if let Some(idle) = preview_envs_idle_timeout_seconds {
            if !(60..=86400).contains(&idle) {
                return Err(ProjectError::InvalidInput(format!(
                    "preview_envs_idle_timeout_seconds {} is not in valid range (60-86400)",
                    idle
                )));
            }
        }
        if let Some(wake) = preview_envs_wake_timeout_seconds {
            if !(5..=120).contains(&wake) {
                return Err(ProjectError::InvalidInput(format!(
                    "preview_envs_wake_timeout_seconds {} is not in valid range (5-120)",
                    wake
                )));
            }
        }
        if let Some(Some(hours)) = image_retention_hours {
            if !(1..=8760).contains(&hours) {
                return Err(ProjectError::InvalidInput(format!(
                    "image_retention_hours {} is not in valid range (1-8760)",
                    hours
                )));
            }
        }
        temps_core::ContainerLogSettings::validate_ingest_override(
            log_ingest_lines_per_sec.flatten(),
            log_ingest_kib_per_sec.flatten(),
        )
        .map_err(|reason| {
            ProjectError::InvalidInput(format!("Project {}: {}", project_id, reason))
        })?;
        // Normalize the display name up-front with the other validations so an
        // unusable name is rejected before anything is written, then persist it
        // only at the end (see below) — a rename must not survive a request that
        // goes on to fail on a conflicting slug or a git desync.
        let new_name = match new_name {
            Some(raw) => Some(validate_project_name(&raw)?),
            None => None,
        };

        // Get the current project
        let mut project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;
        if project.project_type == ProjectType::Service
            && (git_provider_connection_id.is_some()
                || main_branch.is_some()
                || repo_owner.is_some()
                || repo_name.is_some()
                || preset.is_some()
                || directory.is_some()
                || preset_config.is_some())
        {
            return Err(ProjectError::InvalidInput(
                "Service source and runtime fields must be changed through the service controls"
                    .to_string(),
            ));
        }
        // ADR 045, before any of this request's several independent writes
        // commit: on a project this control plane declares, the source
        // definition *is* what runs as host root. Repointing the repository,
        // branch, subdirectory or build preset and then pushing produces a
        // deployment from a git webhook — which carries no authenticated
        // principal, so there is nothing for the deploy guard to check and no
        // HTTP deploy request is ever made. Gating the source here is what
        // closes that two-step path, at the same choke point the slug claim
        // and release are already gated. `git_provider_connection_id` belongs
        // in this list too: `DownloadRepoJob` resolves the clone host from the
        // connection, not from a stored URL, so repointing the connection
        // alone -- without touching owner/name/branch -- is the same
        // escalation through a field this list previously missed.
        // `enable_preview_environments` belongs here for the same reason:
        // once on, any push to a branch no environment already tracks gets a
        // preview environment auto-created and deployed
        // (`find_environments_for_branch`) as `DeployCaller::Platform`, which
        // neither the deploy gate nor the exec gate refuses -- so flipping
        // this one boolean turns "push a branch" into the same host-root
        // execution path a repository repoint gives, without needing the
        // project's primary source touched at all.
        if git_provider_connection_id.is_some()
            || main_branch.is_some()
            || repo_owner.is_some()
            || repo_name.is_some()
            || directory.is_some()
            || preset.is_some()
            || preset_config.is_some()
            || enable_preview_environments.is_some()
        {
            self.guard_granted_project_write(
                &project.slug,
                "the source repository, connection, branch, directory, build preset or preview \
                 environments",
                DeployCaller::from_instance_admin(caller.authority.may_claim_reserved_slug()),
            )?;
        }
        // Refuse a rename onto another project's name before any of this
        // request's independent writes commit.
        if let Some(name_value) = new_name.as_deref() {
            if name_value.to_lowercase() != project.name.to_lowercase() {
                self.ensure_project_name_available(name_value, Some(project_id))
                    .await?;
            }
        }
        // Same for a root directory the repository does not have (issue
        // #1350): checked against the source this request would leave the
        // project with, before any write, and outside every transaction so a
        // provider round-trip never holds a row lock.
        if project.source_type == temps_entities::source_type::SourceType::Git
            && (git_provider_connection_id.is_some()
                || main_branch.is_some()
                || repo_owner.is_some()
                || repo_name.is_some()
                || directory.is_some())
        {
            let normalized_directory = match directory.as_deref() {
                Some(requested) => normalize_project_directory(requested)?,
                None => normalized_stored_directory(&project.directory),
            };
            let target = SourceDirectoryTarget {
                connection_id: match git_provider_connection_id {
                    Some(id) if id > 0 => Some(id),
                    Some(_) => None,
                    None => project.git_provider_connection_id,
                },
                is_public_repo: project.is_public_repo,
                git_url: project.git_url.as_deref(),
                repo_owner: repo_owner.as_deref().unwrap_or(&project.repo_owner),
                repo_name: repo_name.as_deref().unwrap_or(&project.repo_name),
                branch: main_branch.as_deref().unwrap_or(&project.main_branch),
                directory: &normalized_directory,
            };
            if target.differs_from(&project) {
                self.verify_root_directory_in_repository(Some(project_id), target)
                    .await?;
            }
        }
        // Update the slug if provided
        if let Some(slug_value) = new_slug {
            let slug_value = slugify(&slug_value);
            if slug_value.is_empty() || slug_value.len() > 63 {
                return Err(ProjectError::InvalidInput(
                    "Project slug must contain 1-63 lowercase DNS-safe characters".to_string(),
                ));
            }
            // ADR 045: a slug change that touches a granted slug in either
            // direction is admin-only. Renaming *onto* one is a claim on host
            // root; renaming *away* from one revokes an infrastructure
            // service's access and frees the slug for the next project — so
            // guarding only the claim would leave "rename it away, then create
            // your own" open. Re-sending the current slug unchanged is neither,
            // and an ungranted slug is untouched by both: an existing project
            // must keep working through ordinary settings saves.
            if slug_value != project.slug {
                self.guard_reserved_slug(
                    &slug_value,
                    caller,
                    Some(project_id),
                    ReservedSlugChange::Claim,
                )
                .await?;
                self.guard_reserved_slug(
                    &project.slug,
                    caller,
                    Some(project_id),
                    ReservedSlugChange::Release,
                )
                .await?;
            }
            let txn = self.db.begin().await?;
            project = projects::Entity::find_by_id(project_id)
                .lock_exclusive()
                .one(&txn)
                .await?
                .ok_or_else(|| ProjectError::NotFound(format!("Project {project_id} not found")))?;
            // An administrator may have moved this project onto a granted slug
            // since the initial read. Authorize the actual transition under lock.
            if slug_value != project.slug {
                self.guard_reserved_slug(
                    &slug_value,
                    caller,
                    Some(project_id),
                    ReservedSlugChange::Claim,
                )
                .await?;
                self.guard_reserved_slug(
                    &project.slug,
                    caller,
                    Some(project_id),
                    ReservedSlugChange::Release,
                )
                .await?;
            }
            if git_provider_connection_id.is_some()
                || main_branch.is_some()
                || repo_owner.is_some()
                || repo_name.is_some()
                || directory.is_some()
                || preset.is_some()
                || preset_config.is_some()
                || enable_preview_environments.is_some()
            {
                self.guard_granted_project_write(
                    &project.slug,
                    "the source repository, connection, branch, directory, build preset or preview environments",
                    DeployCaller::from_instance_admin(caller.authority.may_claim_reserved_slug()),
                )?;
            }
            txn.execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtext('project-slug:' || $1))",
                [slug_value.clone().into()],
            ))
            .await?;
            // Check if the slug is already taken by another project
            let existing = projects::Entity::find()
                .filter(projects::Column::Slug.eq(&slug_value))
                .filter(projects::Column::Id.ne(project_id))
                .one(&txn)
                .await?;

            if existing.is_some() {
                return Err(ProjectError::SlugAlreadyExists(format!(
                    "Slug '{}' is already taken",
                    slug_value
                )));
            }

            let old_slug = project.slug.clone();
            if old_slug != slug_value {
                let envs = temps_entities::environments::Entity::find()
                    .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                    .all(&txn)
                    .await?;
                let project_environment_ids = envs.iter().map(|env| env.id).collect::<Vec<_>>();
                let mut target_subdomains = HashSet::new();

                // Acquire claims in deterministic order to avoid deadlocks when
                // concurrent project renames touch multiple hostnames.
                let mut active_claims = envs
                    .iter()
                    .filter(|env| env.deleted_at.is_none())
                    .map(|env| {
                        (
                            format!("{}-{}", slug_value, env.slug).to_ascii_lowercase(),
                            env.id,
                        )
                    })
                    .collect::<Vec<_>>();
                active_claims.sort_unstable();
                for (new_subdomain, _) in &active_claims {
                    if !target_subdomains.insert(new_subdomain.clone()) {
                        return Err(ProjectError::InvalidInput(format!(
                            "Project slug '{}' would create duplicate environment subdomain '{}'",
                            slug_value, new_subdomain
                        )));
                    }
                    if temps_entities::environments::claim_subdomain(
                        &txn,
                        new_subdomain,
                        &project_environment_ids,
                    )
                    .await?
                    .is_some()
                    {
                        return Err(ProjectError::InvalidInput(format!(
                            "Project slug '{}' would use an environment subdomain that is already in use",
                            slug_value
                        )));
                    }
                }

                project.slug = slug_value.clone();
                let mut active_project: projects::ActiveModel = project.clone().into();
                active_project.slug = Set(slug_value.clone());
                active_project.update(&txn).await?;

                for env in envs {
                    let previous_subdomain = env.subdomain.clone();
                    let new_subdomain = format!("{}-{}", slug_value, env.slug).to_ascii_lowercase();

                    // Keep the environment and its auto-managed domain row in
                    // the same transaction as the project rename.
                    let mut active_env: temps_entities::environments::ActiveModel = env.into();
                    active_env.subdomain = Set(new_subdomain.clone());
                    let updated_env = active_env.update(&txn).await?;

                    self.environment_service
                        .reconcile_managed_domain(
                            &txn,
                            &updated_env,
                            project.source_type,
                            Some(&previous_subdomain),
                        )
                        .await
                        .map_err(|error| ProjectError::DatabaseError {
                            reason: format!("Could not reconcile managed domain for environment {} while renaming project {project_id}: {error}", updated_env.id),
                        })?;
                }

                txn.commit().await?;
            } else {
                txn.rollback().await?;
            }
        }

        // Update git_provider_connection_id if provided
        if let Some(connection_id) = git_provider_connection_id {
            let txn = self.db.begin().await?;
            let project = self
                .lock_project_for_source_write(
                    &txn,
                    project_id,
                    "the Git provider connection",
                    DeployCaller::from_instance_admin(caller.authority.may_claim_reserved_slug()),
                )
                .await?;

            // Verify connection exists and is active if non-zero
            if connection_id > 0 {
                use temps_entities::git_provider_connections;
                let connection = git_provider_connections::Entity::find_by_id(connection_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ProjectError::GitProviderConnectionNotFound { connection_id })?;

                if !connection.is_active {
                    return Err(ProjectError::Other(format!(
                        "Git provider connection {} is not active",
                        connection_id
                    )));
                }

                // Update the project with the new connection ID
                let mut active_project: projects::ActiveModel = project.into();
                active_project.git_provider_connection_id = Set(Some(connection_id));
                active_project.update(&txn).await?;
            } else {
                // Setting to 0 or negative means remove the connection
                let mut active_project: projects::ActiveModel = project.into();
                active_project.git_provider_connection_id = Set(None);
                active_project.update(&txn).await?;
            }
            txn.commit().await?;
        }

        // ADR 066: the Shield mode and the attack-mode column it replaces
        // are written together for one release (`project_shield_write`).
        if shield_mode.is_some() || attack_mode.is_some() {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let current = temps_shield::ShieldMode::effective(
                None,
                &project.shield_mode,
                None,
                project.attack_mode,
            );
            if let Some((mode, attack)) =
                temps_environments::project_shield_write(shield_mode, attack_mode, current)
            {
                let mut active_project: projects::ActiveModel = project.into();
                active_project.shield_mode = Set(mode.as_str().to_string());
                active_project.attack_mode = Set(attack);
                active_project.update(self.db.as_ref()).await?;
            }
        }

        // Update AI feature toggles if provided (ADR-021 / ADR-023). These are
        // tri-state opt-ins (Some(true) = on), stored as nullable columns.
        if ai_alert_summaries_enabled.is_some()
            || error_source_context_enabled.is_some()
            || vulnerability_scanning_enabled.is_some()
            || error_source_root.is_some()
            || ai_api_traffic_summary_enabled.is_some()
        {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let mut active_project: projects::ActiveModel = project.into();
            if let Some(v) = ai_alert_summaries_enabled {
                active_project.ai_alert_summaries_enabled = Set(Some(v));
            }
            if let Some(v) = ai_api_traffic_summary_enabled {
                active_project.ai_api_traffic_summary_enabled = Set(Some(v));
            }
            // Opt-in for native error-tracking source context (non-null bool).
            if let Some(v) = error_source_context_enabled {
                active_project.error_source_context_enabled = Set(v);
            }
            // Opt-in for Trivy vulnerability scanning (non-null bool, default false).
            if let Some(v) = vulnerability_scanning_enabled {
                active_project.vulnerability_scanning_enabled = Set(v);
            }
            // Auto-capture source root (nullable). Empty string clears it back
            // to the build-context default.
            if let Some(v) = error_source_root {
                active_project.error_source_root =
                    Set(if v.trim().is_empty() { None } else { Some(v) });
            }
            active_project.update(self.db.as_ref()).await?;
        }

        // Update cross_project_trace_sharing if provided (ADR-027 Phase 3 opt-out).
        if let Some(sharing) = cross_project_trace_sharing {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let mut active_project: projects::ActiveModel = project.into();
            active_project.cross_project_trace_sharing = Set(sharing);
            active_project.update(self.db.as_ref()).await?;
        }

        // Whether the project's own app answers `/api/_temps` and `/api/otel`.
        // The proxy reads it from the route table, which the projects route
        // trigger reloads when this column changes.
        if let Some(serves) = serves_own_temps_api {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let mut active_project: projects::ActiveModel = project.into();
            active_project.serves_own_temps_api = Set(serves);
            active_project.update(self.db.as_ref()).await?;
        }

        // Update preview environment settings and image retention if any are provided
        let needs_project_row_update = enable_preview_environments.is_some()
            || preview_envs_on_demand.is_some()
            || preview_envs_idle_timeout_seconds.is_some()
            || preview_envs_wake_timeout_seconds.is_some()
            || image_retention_hours.is_some()
            || log_ingest_lines_per_sec.is_some()
            || log_ingest_kib_per_sec.is_some();

        if needs_project_row_update {
            let txn = self.db.begin().await?;
            let project = projects::Entity::find_by_id(project_id)
                .lock_exclusive()
                .one(&txn)
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            if enable_preview_environments.is_some() {
                self.guard_granted_project_write(
                    &project.slug,
                    "preview environments",
                    DeployCaller::from_instance_admin(caller.authority.may_claim_reserved_slug()),
                )?;
            }

            let mut active_project: projects::ActiveModel = project.into();

            if let Some(enable_preview) = enable_preview_environments {
                active_project.enable_preview_environments = Set(enable_preview);
            }
            if let Some(on_demand) = preview_envs_on_demand {
                active_project.preview_envs_on_demand = Set(on_demand);
            }
            if let Some(idle) = preview_envs_idle_timeout_seconds {
                active_project.preview_envs_idle_timeout_seconds = Set(idle);
            }
            if let Some(wake) = preview_envs_wake_timeout_seconds {
                active_project.preview_envs_wake_timeout_seconds = Set(wake);
            }
            if let Some(hours) = image_retention_hours {
                active_project.image_retention_hours = Set(hours);
            }
            if let Some(lines) = log_ingest_lines_per_sec {
                active_project.log_ingest_lines_per_sec = Set(lines);
            }
            if let Some(kib) = log_ingest_kib_per_sec {
                active_project.log_ingest_kib_per_sec = Set(kib);
            }

            active_project.update(&txn).await?;
            txn.commit().await?;
        }

        // Update git-related fields and preset configuration atomically so a
        // config submitted with a new preset is parsed against that new preset.
        let needs_git_update = main_branch.is_some()
            || repo_owner.is_some()
            || repo_name.is_some()
            || preset.is_some()
            || preset_config.is_some()
            || directory.is_some();

        if needs_git_update {
            let txn = self.db.begin().await?;
            let project = self
                .lock_project_for_source_write(
                    &txn,
                    project_id,
                    "the source repository, branch, directory or build preset",
                    DeployCaller::from_instance_admin(caller.authority.may_claim_reserved_slug()),
                )
                .await?;
            let initial_public_ports = compose_public_ports(project.preset_config.as_ref());

            // `repo_owner`/`repo_name` and `git_url` are read by different
            // code paths — branch resolution uses the former, the clone uses
            // the latter — and this endpoint only writes the former. Changing
            // the repo identity here therefore used to leave a stale clone
            // URL behind, and the next deploy resolved a commit from one repo
            // and cloned another:
            //
            //   Starting repository download for owner/new-repo
            //   Checking out ref: <commit that only exists in new-repo>
            //   Cloning public repository from: .../old-repo.git
            //
            // The project could not be recovered through the API. Reject the
            // change when it would actually desync — the git URL is owned by
            // `POST /projects/{id}/git`, which validates it.
            let desync = would_desync_git_url(
                &project.git_url,
                (&project.repo_owner, &project.repo_name),
                (repo_owner.as_deref(), repo_name.as_deref()),
            );
            if let Some((old, new)) = desync {
                return Err(ProjectError::InvalidInput(format!(
                    "Changing the repository to '{new}' would leave the clone URL pointing at \
                     '{old}'. Update both together with POST /projects/{project_id}/git, which \
                     sets git_url alongside the owner and name."
                )));
            }

            let existing_preset_config = project.preset_config.clone();
            let mut active_project: projects::ActiveModel = project.into();

            if let Some(branch) = main_branch {
                active_project.main_branch = Set(branch);
            }
            if let Some(owner) = repo_owner {
                active_project.repo_owner = Set(owner);
            }
            if let Some(name) = repo_name {
                active_project.repo_name = Set(name);
            }
            if let Some(preset_value) = preset {
                let resolved = resolve_preset_selection(
                    preset_value.as_str(),
                    preset_config.as_ref(),
                    existing_preset_config.as_ref(),
                )?;
                apply_resolved_preset(&mut active_project, resolved)?;
            } else if let Some(config_value) = preset_config.as_ref() {
                let parsed = temps_entities::preset::PresetConfig::parse_for_preset(
                    active_project.preset.as_ref(),
                    config_value,
                )
                .map_err(|error| {
                    ProjectError::InvalidInput(format!("Invalid preset config: {}", error))
                })?;
                let merged = merge_preset_config(
                    existing_preset_config.as_ref(),
                    parsed,
                    config_value,
                    true,
                );
                let merged = validate_preset_config(
                    *active_project.preset.as_ref(),
                    merged,
                    Some(config_value),
                )?;
                let resolved = temps_presets::StoredPreset {
                    preset: *active_project.preset.as_ref(),
                    config: Some(merged.clone()),
                };
                if active_project.project_type.as_ref() != &ProjectType::Service {
                    active_project.project_type = Set(project_type_for_resolved_preset(&resolved)?);
                }
                active_project.preset_config = Set(Some(merged));
            }
            if let Some(dir) = directory {
                active_project.directory = Set(normalize_project_directory(&dir)?);
            }
            // Carry the write and the route-reload signal on one transaction.
            // If the signal cannot be published, returning an error while the
            // rename and the preset/directory changes stayed persisted would
            // leave the caller unable to tell what actually happened — and on a
            // port removal, leave a withdrawn route reachable. Dropping the
            // transaction on the error path rolls all of it back together.
            // Fold the rename into this same write so a name change submitted
            // alongside git settings commits atomically with them. The write
            // itself rides `active_project` below rather than a separate update.
            let mut rename = None;
            if let Some(ref name_value) = new_name {
                rename = match self.locked_rename(&txn, project_id, name_value).await {
                    Ok(rename) => rename,
                    Err(error) => {
                        txn.rollback().await?;
                        return Err(error);
                    }
                };
                active_project.name = Set(name_value.clone());
            }

            let updated_project = active_project.update(&txn).await?;
            let ports_changed = initial_public_ports
                != compose_public_ports(updated_project.preset_config.as_ref());
            if ports_changed {
                // Only this transaction rolls back. Earlier steps of this same
                // request (slug, attack mode, AI toggles, preview envs) were
                // committed independently and survive, so do not tell the
                // operator the whole request was discarded.
                self.publish_route_reload(
                    &txn,
                    project_id,
                    "The git settings, preset, directory and name portion of this update was \
                     rolled back; other settings sent in the same request (slug, attack mode, \
                     AI toggles, preview environments) commit independently and may already be \
                     saved.",
                )
                .await?;
            }
            txn.commit().await?;
            if ports_changed {
                self.enqueue_route_reload(project_id).await;
            }
            // Notify before the provider lookup: past the commit, every await
            // is a point the task can be cancelled at, and losing the response
            // is recoverable where losing the notification is not.
            let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
                project_id: updated_project.id,
                project_name: updated_project.name.clone(),
            });

            if let Err(e) = self.queue_service.send(project_updated_job).await {
                warn!(
                    "Failed to emit ProjectUpdated job for project {}: {}",
                    updated_project.id, e
                );
            }

            return Ok(ProjectSettingsUpdate {
                project: self.map_written_project(updated_project).await,
                rename,
            });
        }

        // Persist the rename last, once every other fallible step has
        // succeeded, so a failed request never leaves a renamed project behind.
        //
        // Note the guarantee is one-directional and this endpoint is *not*
        // all-or-nothing: everything above committed independently, so a failure
        // in this block still leaves those earlier steps persisted. Ordering the
        // rename last only ensures it is never the survivor of someone else's
        // failure. Making the whole endpoint atomic belongs with the parked
        // optimistic-concurrency work, not here.
        // The locked read, the write, and the read-back of the final state all
        // ride one transaction: committing before the read-back would let a
        // failure there report an error over an already-persisted rename, and
        // reading back outside the lock would let a concurrent rename supply
        // the "after" half of a transition whose "before" half came from here.
        let mut rename = None;
        let final_project = if let Some(name_value) = new_name {
            let txn = self.db.begin().await?;
            rename = match self.locked_rename(&txn, project_id, &name_value).await {
                Ok(rename) => rename,
                Err(error) => {
                    txn.rollback().await?;
                    return Err(error);
                }
            };
            if rename.is_some() {
                let mut active_project = projects::ActiveModel {
                    id: Set(project_id),
                    ..Default::default()
                };
                active_project.name = Set(name_value);
                active_project.update(&txn).await?;
            }

            let final_project = projects::Entity::find_by_id(project_id)
                .one(&txn)
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            txn.commit().await?;
            final_project
        } else {
            // No mutation to lose here, so a plain read is enough.
            projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?
        };

        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: final_project.id,
            project_name: final_project.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                final_project.id, e
            );
        }

        Ok(ProjectSettingsUpdate {
            project: self.map_written_project(final_project).await,
            rename,
        })
    }

    pub async fn update_automatic_deploy(
        &self,
        project_id: i32,
        automatic_deploy: bool,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        // Get the current project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;
        // ADR 045: arming automatic_deploy is what turns a plain git push
        // into a deployment (`DeployCaller::Platform`, which the deploy gate
        // does not refuse) -- the same mechanism the preview-environment and
        // environment-settings guards close for their own trigger fields.
        self.guard_granted_project_write(
            &project.slug,
            "whether pushes deploy automatically",
            caller,
        )?;
        // Update automatic_deploy setting in deployment_config
        let mut active_project: projects::ActiveModel = project.clone().into();

        // Update deployment config with new automatic_deploy value
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        deployment_config.automatic_deploy = Some(automatic_deploy);
        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        Ok(self.map_written_project(updated_project).await)
    }

    /// Public Compose ports are read directly from `projects.preset_config`
    /// when the proxy builds its route table. Updating the JSON alone leaves
    /// the in-memory table stale, because the project DB trigger deliberately
    /// ignores generic preset-config changes.
    ///
    /// Publishes on the caller's connection rather than reaching for `self.db`,
    /// so a caller writing inside a transaction issues the NOTIFY on that same
    /// transaction. PostgreSQL queues notifications and delivers them at commit,
    /// so a rolled-back update signals nothing and no listener can observe a
    /// reload for a write that never landed.
    ///
    /// The in-process queue job is deliberately *not* sent here, and there is
    /// deliberately no eager fallback to it when the NOTIFY fails. Its
    /// subscriber reloads the route table over its own database connection, so
    /// enqueuing before the transaction commits races that commit: the
    /// subscriber can read the pre-update ports and latch them in with nothing
    /// left to trigger a re-read. On a change that *removes* a public port
    /// that fails open — the withdrawn route stays reachable indefinitely — so
    /// the safe response to an unavailable NOTIFY is to roll the write back and
    /// let the caller retry, not to persist it behind a signal that may never
    /// land. The caller sends the job after committing; see
    /// [`Self::enqueue_route_reload`].
    pub(super) async fn publish_route_reload<C: ConnectionTrait>(
        &self,
        conn: &C,
        project_id: i32,
        rolled_back_scope: &str,
    ) -> Result<(), ProjectError> {
        let payload = serde_json::json!({
            "action": "UPDATE",
            "project_id": project_id,
            "field": "preset_config.public_ports",
        })
        .to_string();
        conn.execute(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_notify('project_route_change', $1)",
            [payload.into()],
        ))
        .await
        .map_err(|database_error| {
            // The driver text can name the connection target or a constraint, so
            // it stays in the operator's logs rather than the HTTP response.
            error!(
                project_id,
                error = %database_error,
                "Failed to publish project route-reload NOTIFY"
            );
            ProjectError::RouteReloadFailed {
                project_id,
                rolled_back_scope: rolled_back_scope.to_string(),
            }
        })?;

        Ok(())
    }

    /// Send the in-process route-reload job. Call only *after* the transaction
    /// carrying the port change has committed, so the subscriber cannot read
    /// pre-commit state.
    ///
    /// Best-effort. NOTIFY is not durable — it reaches only sessions holding a
    /// `LISTEN` at commit time — so a queue failure here is not automatically
    /// harmless. What makes it recoverable is the listener itself, which
    /// reloads the full route table on two independent triggers: after a
    /// reconnect (catching signals missed while it was down) and after an idle
    /// window elapses with no notification (catching a connection that died
    /// without ever reporting an error). The second is what bounds the damage
    /// here — without it, losing both the NOTIFY and this queue send would
    /// leave a withdrawn public port reachable indefinitely. Do not weaken
    /// either trigger while trusting this comment; see
    /// `temps_routes::project_change_listener::IDLE_RECONCILE_INTERVAL`.
    pub(super) async fn enqueue_route_reload(&self, project_id: i32) {
        if let Err(e) = self
            .queue_service
            .send(Job::ForceRouteReload(ForceRouteReloadJob {
                environment_id: None,
                deployment_id: None,
            }))
            .await
        {
            warn!(
                project_id,
                error = %e,
                "Route reload job could not be enqueued after committing a public-port change; \
                 this process will rely on the delivered NOTIFY"
            );
        }
    }

    /// See [`Self::update_project_deployment_config`] for what
    /// `ceiling_enforcement` means.
    ///
    /// No handler in this crate calls this method today — `ProjectsWrite`
    /// callers reach deployment-config updates through
    /// [`Self::update_project_deployment_config`] instead. It is kept public
    /// and ceiling-enforced because [`ProjectService`] is registered with the
    /// plugin DI system (see `crates/temps-projects/src/plugin.rs`), so an
    /// out-of-tree plugin may call it directly; if you're removing the last
    /// in-tree reference to this method, check for that before deleting it.
    pub async fn update_deployment_settings(
        &self,
        project_id_or_slug: &str,
        settings: UpdateDeploymentSettingsRequest,
        ceiling_enforcement: temps_core::CeilingEnforcement,
    ) -> Result<Project, ProjectError> {
        // Find project by ID or slug
        let project = if let Ok(project_id_int) = project_id_or_slug.parse::<i32>() {
            projects::Entity::find_by_id(project_id_int)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    ProjectError::NotFound(format!("Project with id {} not found", project_id_int))
                })?
        } else {
            projects::Entity::find()
                .filter(projects::Column::Slug.eq(project_id_or_slug))
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    ProjectError::NotFound(format!(
                        "Project with slug {} not found",
                        project_id_or_slug
                    ))
                })?
        };

        // Update the project with new settings
        let mut active_project: projects::ActiveModel = project.clone().into();

        // Update deployment config with new resource settings
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        deployment_config.cpu_request = settings.cpu_request;
        deployment_config.cpu_limit = settings.cpu_limit;
        deployment_config.memory_request = settings.memory_request;
        deployment_config.memory_limit = settings.memory_limit;

        if ceiling_enforcement == temps_core::CeilingEnforcement::Enforce {
            let app_settings = self.config_service.get_settings().await.map_err(|e| {
                ProjectError::Other(format!(
                    "Failed to read instance settings to check resource ceilings for project {project_id_or_slug}: {e}"
                ))
            })?;
            Self::enforce_tenant_ceilings(&deployment_config, &app_settings)?;
        }

        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: updated_project.id,
            project_name: updated_project.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                updated_project.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {} (settings update)",
                updated_project.id
            );
        }

        Ok(self.map_written_project(updated_project).await)
    }

    /// Update a project's deployment config.
    ///
    /// `ceiling_enforcement` reflects the caller's `SettingsWrite` permission:
    /// whoever can raise the instance-wide ceilings is by definition allowed to
    /// exceed them, so the check would be theatre for them. Everyone else is
    /// held to `AppSettings.tenant_resource_ceilings`, which is unenforced
    /// until an operator configures it.
    pub async fn update_project_deployment_config(
        &self,
        project_id: i32,
        config: UpdateDeploymentConfigRequest,
        ceiling_enforcement: temps_core::CeilingEnforcement,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        // Find project by ID or slug
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("Project with id {} not found", project_id))
            })?;

        // ADR 045: `automatic_deploy` arms the same push-deploy trigger
        // `ProjectService::update_automatic_deploy` guards, and `exposed_port`
        // republishes a *different* port of a host-root-equivalent container
        // on the node's private address -- both change the attack surface of
        // a granted project without going through a deploy at all. A health
        // command is another executable input to the next granted deployment.
        if config.exposed_port.is_some()
            || config.automatic_deploy.is_some()
            || config.health.is_some()
        {
            self.guard_granted_project_write(
                &project.slug,
                "the exposed port, health command or whether pushes deploy automatically",
                caller,
            )?;
        }

        // Get existing deployment config or create default
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();

        // Update only the fields that are provided
        if let Some(cpu_request) = config.cpu_request {
            deployment_config.cpu_request = Some(cpu_request);
        }
        if let Some(cpu_limit) = config.cpu_limit {
            deployment_config.cpu_limit = Some(cpu_limit);
        }
        if let Some(memory_request) = config.memory_request {
            deployment_config.memory_request = Some(memory_request);
        }
        if let Some(memory_limit) = config.memory_limit {
            deployment_config.memory_limit = Some(memory_limit);
        }
        if let Some(exposed_port) = config.exposed_port {
            deployment_config.exposed_port = exposed_port;
        }
        if let Some(automatic_deploy) = config.automatic_deploy {
            deployment_config.automatic_deploy = Some(automatic_deploy);
        }
        if let Some(performance_metrics_enabled) = config.performance_metrics_enabled {
            deployment_config.performance_metrics_enabled = performance_metrics_enabled;
        }
        if let Some(session_recording_enabled) = config.session_recording_enabled {
            deployment_config.session_recording_enabled = session_recording_enabled;
        }
        if let Some(replicas) = config.replicas {
            deployment_config.replicas = replicas;
        }
        if let Some(security) = config.security {
            deployment_config.security = Some(security);
        }
        // Absent leaves it unset (disabled, and inheritable); an explicit value
        // — including `false` — pins it for every environment that doesn't
        // override it.
        if let Some(cross_architecture_builds) = config.cross_architecture_builds {
            deployment_config.cross_architecture_builds = Some(cross_architecture_builds);
        }
        if let Some(build_location) = config.build_location {
            deployment_config.build_location = Some(build_location);
        }
        if let Some(request_timeout_seconds) = config.request_timeout_seconds {
            deployment_config.request_timeout_seconds = Some(request_timeout_seconds);
        }
        if let Some(sse_idle_timeout_seconds) = config.sse_idle_timeout_seconds {
            deployment_config.sse_idle_timeout_seconds = Some(sse_idle_timeout_seconds);
        }
        if let Some(websocket_idle_timeout_seconds) = config.websocket_idle_timeout_seconds {
            deployment_config.websocket_idle_timeout_seconds = Some(websocket_idle_timeout_seconds);
        }
        if let Some(max_concurrent_connections) = config.max_concurrent_connections {
            deployment_config.max_concurrent_connections = Some(max_concurrent_connections);
        }
        if let Some(health_check_timeout_seconds) = config.health_check_timeout_seconds {
            deployment_config.health_check_timeout_seconds = Some(health_check_timeout_seconds);
        }

        if let Some(health) = config.health {
            deployment_config.health = Some(health);
        }

        // Validate the deployment config
        deployment_config
            .validate()
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid deployment config: {}", e)))?;

        // Checked on the merged config, not on the request, so clearing an
        // override back to "inherit" is judged by what the project will
        // actually run with.
        if ceiling_enforcement == temps_core::CeilingEnforcement::Enforce {
            let app_settings = self.config_service.get_settings().await.map_err(|e| {
                ProjectError::Other(format!(
                    "Failed to read instance settings to check resource ceilings for project {project_id}: {e}"
                ))
            })?;
            Self::enforce_tenant_ceilings(&deployment_config, &app_settings)?;
        }

        // Update the project
        let mut active_project: projects::ActiveModel = project.clone().into();
        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: updated_project.id,
            project_name: updated_project.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                updated_project.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {} (deployment config update)",
                updated_project.id
            );
        }

        Ok(self.map_written_project(updated_project).await)
    }
}
