// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service-template provenance, runtime updates and upgrades.

use super::*;

// API Response types
#[derive(Debug, Serialize)]
pub struct TemplateResponse {
    pub name: String,
    pub description: String,
    pub image: String,
    pub github: TemplateGithubResponse,
    pub preset: Option<String>,
    pub project_type: String,
    pub services: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub env: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct TemplateGithubResponse {
    pub owner: String,
    pub repo: String,
    pub path: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

fn apply_new_template_default_when_unmodified<T: Clone + PartialEq>(
    current: T,
    applied_default: T,
    target_default: T,
) -> T {
    if current == applied_default {
        target_default
    } else {
        current
    }
}

fn template_resource_value(
    template: &temps_core::templates::TemplateServiceSpec,
    select: impl FnOnce(&temps_core::templates::TemplateResources) -> Option<i32>,
) -> Option<i32> {
    template.resources.as_ref().and_then(select)
}

fn missing_required_service_types(required: &[String], linked: &HashSet<String>) -> Vec<String> {
    required
        .iter()
        .filter(|service| {
            !linked.iter().any(|linked_service| {
                temps_core::templates::managed_service_types_compatible(service, linked_service)
            })
        })
        .cloned()
        .collect()
}

/// Keep template upgrades reversible with the current deployment model.
/// Deployments intentionally read the project's current managed-service
/// aliases, so an upgrade may add aliases but cannot remove or remap aliases
/// used by an older release. That keeps a rollback's database credentials
/// compatible without guessing from the live catalog.
fn validate_service_binding_compatibility(
    applied: &temps_core::templates::TemplateServiceSpec,
    target: &temps_core::templates::TemplateServiceSpec,
) -> Result<(), ProjectError> {
    for required_service in &applied.databases {
        if !target.databases.iter().any(|target_service| {
            temps_core::templates::managed_service_types_compatible(
                required_service,
                target_service,
            )
        }) {
            return Err(ProjectError::InvalidInput(format!(
                "Template upgrade cannot remove required managed service '{required_service}'; older deployments require it for safe rollback"
            )));
        }
    }
    for (service_type, applied_bindings) in &applied.database_bindings {
        let target_bindings = target
            .database_bindings
            .iter()
            .find(|(target_service_type, _)| {
                temps_core::templates::managed_service_types_compatible(
                    service_type,
                    target_service_type,
                )
            })
            .map(|(_, bindings)| bindings);
        for (application_variable, service_variable) in applied_bindings {
            let preserved = target_bindings
                .and_then(|bindings| bindings.get(application_variable))
                .is_some_and(|target_variable| target_variable == service_variable);
            if !preserved {
                return Err(ProjectError::InvalidInput(format!(
                    "Template upgrade cannot remove or remap managed-service binding \
                     {service_type}.{application_variable} -> {service_variable}; older \
                     deployments require it for safe rollback"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn production_configured_variable_keys(
    variables: &[env_vars::Model],
    links: &[env_var_environments::Model],
    production_environment_id: i32,
) -> HashSet<String> {
    let linked_ids = links
        .iter()
        .map(|link| link.env_var_id)
        .collect::<HashSet<_>>();
    let production_linked_ids = links
        .iter()
        .filter(|link| link.environment_id == production_environment_id)
        .map(|link| link.env_var_id)
        .collect::<HashSet<_>>();

    variables
        .iter()
        .filter(|variable| {
            variable.environment_id == Some(production_environment_id)
                || production_linked_ids.contains(&variable.id)
                // Legacy/global variables have neither the old direct
                // environment id nor a join-table relation and apply to every
                // environment. Keep their values during upgrades.
                || (variable.environment_id.is_none() && !linked_ids.contains(&variable.id))
        })
        .map(|variable| variable.key.clone())
        .collect()
}

pub(super) fn applied_service_template_from_model(
    project: &projects::Model,
) -> Result<temps_core::templates::ServiceTemplateInstance, ProjectError> {
    if project.project_type != ProjectType::Service {
        return Err(ProjectError::InvalidInput(format!(
            "Project {} is not a service project",
            project.id
        )));
    }

    if let Some(snapshot) = &project.service_template {
        let instance = serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(
            snapshot.clone(),
        )
        .map_err(|error| {
            ProjectError::Other(format!(
                "Stored service template for project {} is invalid: {error}",
                project.id
            ))
        })?;
        instance.validate().map_err(|error| {
            ProjectError::Other(format!(
                "Stored service template for project {} is invalid: {error}",
                project.id
            ))
        })?;
        return Ok(instance);
    }

    Err(ProjectError::Other(format!(
        "Service project {} has no applied template release",
        project.id
    )))
}

impl ProjectService {
    pub(super) fn check_service_release(
        request: &CreateProjectRequest,
        service_template: &temps_core::templates::ServiceTemplateInstance,
    ) -> Result<(), ProjectError> {
        if service_template.template.kind != temps_core::templates::TemplateKind::App {
            return Err(ProjectError::InvalidInput(format!(
                "Template '{}' is not a service template",
                service_template.slug
            )));
        }
        service_template.validate().map_err(|error| {
            ProjectError::InvalidInput(format!(
                "Invalid service template {}@{}: {error}",
                service_template.slug, service_template.version,
            ))
        })?;
        if request.template_slug.as_deref() != Some(service_template.slug.as_str()) {
            return Err(ProjectError::InvalidInput(format!(
                "Service project provenance must match template '{}'",
                service_template.slug
            )));
        }
        Ok(())
    }

    /// Load the immutable release currently applied to a service project.
    pub async fn get_applied_service_template(
        &self,
        project_id: i32,
    ) -> Result<temps_core::templates::ServiceTemplateInstance, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("Project with id {project_id} not found"))
            })?;
        applied_service_template_from_model(&project)
    }

    /// Return the managed-service families currently linked to a project.
    pub async fn get_linked_service_types(
        &self,
        project_id: i32,
    ) -> Result<HashSet<String>, ProjectError> {
        let linked = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .find_also_related(external_services::Entity)
            .all(self.db.as_ref())
            .await?;
        Ok(linked
            .into_iter()
            .filter_map(|(_, service)| service)
            .map(|service| service.service_type.to_ascii_lowercase())
            .collect())
    }

    /// Replace the editable runtime snapshot for a single-container image
    /// template and its project-level resources atomically.
    ///
    /// Both JSON columns live on `projects`, so one row update is enough. A
    /// rejected image, command, health path, resource profile, or tenant
    /// ceiling leaves every previous value intact.
    ///
    /// `caller` is required (ADR 045): this is where a project's *persisted*
    /// runtime image and command are written, and on a project this control
    /// plane declares, that command is what the next deployment runs as host
    /// root — including a deployment started by an admin, who would be
    /// executing a payload they did not write. Ordinary `ProjectsWrite` is
    /// therefore not sufficient here for a declared project.
    pub async fn update_service_template_runtime(
        &self,
        project_id: i32,
        runtime: UpdateServiceTemplateRuntimeRequest,
        ceiling_enforcement: temps_core::CeilingEnforcement,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        let txn = self.db.begin().await?;
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .lock(LockType::Update)
            .one(&txn)
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("Project with id {} not found", project_id))
            })?;
        self.guard_granted_project_write(&project.slug, "the runtime image and command", caller)?;

        if project.source_type != temps_entities::source_type::SourceType::DockerImage
            || project.project_type != ProjectType::Service
        {
            return Err(ProjectError::InvalidInput(
                "Service runtime settings are available only for template-backed service projects"
                    .to_string(),
            ));
        }
        if project.preset != temps_entities::preset::Preset::Dockerfile {
            return Err(ProjectError::InvalidInput(
                "Single-container service templates must use the Dockerfile runtime preset"
                    .to_string(),
            ));
        }
        // A service identity is never inferred from its slug or current
        // runtime. New service projects always carry their complete release.
        applied_service_template_from_model(&project)?;

        let mut preset_config = project.preset_config.clone().unwrap_or_else(|| {
            temps_entities::preset::PresetConfig::default_for_preset(
                temps_entities::preset::Preset::Dockerfile,
            )
        });
        let temps_entities::preset::PresetConfig::Dockerfile(config) = &mut preset_config else {
            return Err(ProjectError::InvalidInput(
                "Project preset configuration does not match the Dockerfile runtime".to_string(),
            ));
        };
        config.image_runtime = Some(temps_entities::preset::ImageRuntimeConfig {
            image_ref: runtime.image_ref,
            command: (!runtime.command.is_empty()).then_some(runtime.command),
            health_check_path: Some(runtime.health_check_path),
        });
        temps_presets::validate_preset_config(project.preset, &preset_config).map_err(|error| {
            ProjectError::InvalidInput(format!("Invalid service runtime: {error}"))
        })?;

        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        deployment_config.cpu_request = runtime.cpu_request;
        deployment_config.cpu_limit = runtime.cpu_limit;
        deployment_config.memory_request = runtime.memory_request;
        deployment_config.memory_limit = runtime.memory_limit;
        deployment_config.exposed_port = runtime.exposed_port;
        deployment_config.validate().map_err(|error| {
            ProjectError::InvalidInput(format!("Invalid deployment config: {error}"))
        })?;

        if ceiling_enforcement == temps_core::CeilingEnforcement::Enforce {
            let app_settings = self.config_service.get_settings().await.map_err(|error| {
                ProjectError::Other(format!(
                    "Failed to read instance settings to check resource ceilings for project {project_id}: {error}"
                ))
            })?;
            Self::enforce_tenant_ceilings(&deployment_config, &app_settings)?;
        }

        let mut active_project: projects::ActiveModel = project.into();
        active_project.preset_config = Set(Some(preset_config));
        active_project.deployment_config = Set(Some(deployment_config));
        let updated_project = active_project.update(&txn).await?;
        txn.commit().await?;

        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: updated_project.id,
            project_name: updated_project.name.clone(),
        });
        if let Err(error) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                updated_project.id, error
            );
        }

        Ok(self.map_written_project(updated_project).await)
    }

    /// Apply a newer release of the same service template while preserving
    /// instance overrides. A field receives the new template default only when
    /// its current value still equals the previously applied default.
    pub async fn upgrade_service_template(
        &self,
        project_id: i32,
        target: temps_core::templates::ServiceTemplateInstance,
        new_environment_variables: Vec<CreateProjectEnvVar>,
        ceiling_enforcement: temps_core::CeilingEnforcement,
        caller: DeployCaller,
    ) -> Result<Project, ProjectError> {
        let txn = self.db.begin().await?;
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .lock(LockType::Update)
            .one(&txn)
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("Project with id {} not found", project_id))
            })?;
        // ADR 045: the sibling of `update_service_template_runtime`, and the
        // other way the persisted image and command of a service project
        // change. The target comes from the operator-installed catalog rather
        // than from the request body, so this is the weaker of the two — but
        // it still decides what a declared project runs as host root, and
        // leaving it open would make "downgrade to a release with a different
        // entrypoint" the way around the guard next to it.
        self.guard_granted_project_write(
            &project.slug,
            "the applied service template release",
            caller,
        )?;

        if project.project_type != ProjectType::Service {
            return Err(ProjectError::InvalidInput(
                "Only service projects can apply service-template upgrades".to_string(),
            ));
        }
        if target.template.kind != temps_core::templates::TemplateKind::App {
            return Err(ProjectError::InvalidInput(format!(
                "Template '{}' is not a service release",
                target.slug
            )));
        }
        target.validate().map_err(|error| {
            ProjectError::InvalidInput(format!(
                "Invalid target service template {}@{}: {error}",
                target.slug, target.version,
            ))
        })?;

        // Only one service's runtime is applied here. A release with several
        // services would persist its full snapshot without creating the extra
        // services or routes, so it can only be deployed as a whole app.
        if target.template.is_multi_service() {
            return Err(ProjectError::SingleServiceUpgradeToMultiService {
                project_id,
                template_slug: target.slug.clone(),
                target_version: target.version.clone(),
                service_count: target.template.services.len(),
            });
        }

        let applied = applied_service_template_from_model(&project)?;

        if applied.slug != target.slug {
            return Err(ProjectError::InvalidInput(format!(
                "A '{}' service cannot be changed into '{}'; select a newer release of the same template",
                applied.slug, target.slug
            )));
        }
        let is_newer = target.is_newer_than(&applied).map_err(|error| {
            ProjectError::InvalidInput(format!(
                "Cannot compare service template releases {}@{} and {}@{}: {error}",
                applied.slug, applied.version, target.slug, target.version
            ))
        })?;
        if !is_newer {
            return Err(ProjectError::InvalidInput(format!(
                "Service project {project_id} uses {}@{}; target release {}@{} must be newer",
                applied.slug, applied.version, target.slug, target.version
            )));
        }
        let applied_spec = applied.spec().cloned().ok_or_else(|| {
            ProjectError::Other(format!(
                "Stored service template for project {project_id} names a service it does not define"
            ))
        })?;
        let target_spec = target.spec().cloned().ok_or_else(|| {
            ProjectError::InvalidInput(format!(
                "Template {}@{} has no '{}' service",
                target.slug,
                target.version,
                target.service.as_deref().unwrap_or_default()
            ))
        })?;
        validate_service_binding_compatibility(&applied_spec, &target_spec)?;
        if project.source_type != temps_entities::source_type::SourceType::DockerImage
            || project.preset != temps_entities::preset::Preset::Dockerfile
        {
            return Err(ProjectError::InvalidInput(
                "This release can only upgrade a single-container image service".to_string(),
            ));
        }

        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .find_also_related(external_services::Entity)
            .all(&txn)
            .await?;
        let linked_service_types = linked_services
            .iter()
            .filter_map(|(_, service)| service.as_ref())
            .map(|service| service.service_type.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let missing_services =
            missing_required_service_types(&target_spec.databases, &linked_service_types);
        if !missing_services.is_empty() {
            return Err(ProjectError::InvalidInput(format!(
                "Template {}@{} requires linked managed service(s): {}",
                target.slug,
                target.version,
                missing_services.join(", ")
            )));
        }
        for required in &target_spec.databases {
            let count = linked_services
                .iter()
                .filter_map(|(_, service)| service.as_ref())
                .filter(|service| {
                    temps_core::templates::managed_service_types_compatible(
                        required,
                        &service.service_type,
                    )
                })
                .count();
            if count > 1 {
                return Err(ProjectError::InvalidInput(format!(
                    "Template {}@{} requires exactly one linked {} service, but project {project_id} has {count}",
                    target.slug, target.version, required
                )));
            }
        }

        let mut preset_config = project.preset_config.clone().ok_or_else(|| {
            ProjectError::Other(format!(
                "Service project {project_id} has no persisted runtime configuration"
            ))
        })?;
        let temps_entities::preset::PresetConfig::Dockerfile(config) = &mut preset_config else {
            return Err(ProjectError::Other(format!(
                "Service project {project_id} has a non-Dockerfile runtime configuration"
            )));
        };
        let current_runtime = config.image_runtime.clone().ok_or_else(|| {
            ProjectError::Other(format!(
                "Service project {project_id} has no persisted image runtime"
            ))
        })?;
        let target_image = target_spec.image.clone().ok_or_else(|| {
            ProjectError::InvalidInput(format!(
                "Target service template {}@{} has no image",
                target.slug, target.version
            ))
        })?;

        config.image_runtime = Some(temps_entities::preset::ImageRuntimeConfig {
            image_ref: apply_new_template_default_when_unmodified(
                current_runtime.image_ref,
                applied_spec.image.clone().unwrap_or_default(),
                target_image,
            ),
            command: apply_new_template_default_when_unmodified(
                current_runtime.command,
                applied_spec.command.clone(),
                target_spec.command.clone(),
            ),
            health_check_path: apply_new_template_default_when_unmodified(
                current_runtime.health_check_path,
                applied_spec
                    .health_check_path
                    .clone()
                    .or_else(|| Some("/".to_string())),
                target_spec
                    .health_check_path
                    .clone()
                    .or_else(|| Some("/".to_string())),
            ),
        });
        temps_presets::validate_preset_config(project.preset, &preset_config).map_err(|error| {
            ProjectError::InvalidInput(format!("Invalid target service runtime: {error}"))
        })?;

        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        let applied_resources = &applied_spec;
        let target_resources = &target_spec;
        deployment_config.cpu_request = apply_new_template_default_when_unmodified(
            deployment_config.cpu_request,
            template_resource_value(applied_resources, |resources| resources.cpu_request),
            template_resource_value(target_resources, |resources| resources.cpu_request),
        );
        deployment_config.cpu_limit = apply_new_template_default_when_unmodified(
            deployment_config.cpu_limit,
            template_resource_value(applied_resources, |resources| resources.cpu_limit),
            template_resource_value(target_resources, |resources| resources.cpu_limit),
        );
        deployment_config.memory_request = apply_new_template_default_when_unmodified(
            deployment_config.memory_request,
            template_resource_value(applied_resources, |resources| resources.memory_request),
            template_resource_value(target_resources, |resources| resources.memory_request),
        );
        deployment_config.memory_limit = apply_new_template_default_when_unmodified(
            deployment_config.memory_limit,
            template_resource_value(applied_resources, |resources| resources.memory_limit),
            template_resource_value(target_resources, |resources| resources.memory_limit),
        );
        deployment_config.exposed_port = apply_new_template_default_when_unmodified(
            deployment_config.exposed_port,
            applied_spec.exposed_port,
            target_spec.exposed_port,
        );
        deployment_config.validate().map_err(|error| {
            ProjectError::InvalidInput(format!("Invalid target deployment config: {error}"))
        })?;

        if ceiling_enforcement == temps_core::CeilingEnforcement::Enforce {
            let app_settings = self.config_service.get_settings().await.map_err(|error| {
                ProjectError::Other(format!(
                    "Failed to read instance settings before upgrading project {project_id}: {error}"
                ))
            })?;
            Self::enforce_tenant_ceilings(&deployment_config, &app_settings)?;
        }

        let production_environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::IsPreview.eq(false))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::Id)
            .one(&txn)
            .await?
            .ok_or_else(|| {
                ProjectError::Other(format!(
                    "Service project {project_id} has no active production environment"
                ))
            })?;
        let existing_variables = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .all(&txn)
            .await?;
        let existing_variable_ids = existing_variables
            .iter()
            .map(|variable| variable.id)
            .collect::<Vec<_>>();
        let variable_links = if existing_variable_ids.is_empty() {
            Vec::new()
        } else {
            env_var_environments::Entity::find()
                .filter(env_var_environments::Column::EnvVarId.is_in(existing_variable_ids))
                .all(&txn)
                .await?
        };
        let linked_ids = variable_links
            .iter()
            .map(|link| link.env_var_id)
            .collect::<HashSet<_>>();
        let production_linked_ids = variable_links
            .iter()
            .filter(|link| link.environment_id == production_environment.id)
            .map(|link| link.env_var_id)
            .collect::<HashSet<_>>();
        let applies_to_production = |variable: &env_vars::Model| {
            variable.environment_id == Some(production_environment.id)
                || production_linked_ids.contains(&variable.id)
                || (variable.environment_id.is_none() && !linked_ids.contains(&variable.id))
        };
        let mut production_variables = BTreeMap::<String, Vec<env_vars::Model>>::new();
        for variable in existing_variables
            .iter()
            .filter(|variable| applies_to_production(variable))
        {
            production_variables
                .entry(variable.key.clone())
                .or_default()
                .push(variable.clone());
        }
        let is_production_specific = |variable: &env_vars::Model| {
            variable.environment_id == Some(production_environment.id)
                || production_linked_ids.contains(&variable.id)
        };
        let effective_index = |variables: &[env_vars::Model]| {
            variables
                .iter()
                .enumerate()
                .max_by_key(|(_, variable)| (is_production_specific(variable), variable.id))
                .map(|(index, _)| index)
        };
        let mut configured_keys = HashSet::new();
        for (key, variables) in &production_variables {
            let Some(variable) = effective_index(variables).map(|index| &variables[index]) else {
                continue;
            };
            let cleartext = if variable.is_encrypted {
                self.encryption_service
                    .decrypt_string(&variable.value)
                    .map_err(|error| {
                        ProjectError::Other(format!(
                            "Failed to decrypt production environment variable '{}' while upgrading project {project_id}: {error}",
                            variable.key
                        ))
                    })?
            } else {
                variable.value.clone()
            };
            if !cleartext.is_empty() {
                configured_keys.insert(key.clone());
            }
        }

        let target_variables = target.service_variables();
        let definitions = target_variables
            .iter()
            .map(|definition| (definition.name.as_str(), definition))
            .collect::<BTreeMap<_, _>>();
        let mut pending_keys = HashSet::new();
        let mut pending_non_empty_keys = HashSet::new();
        for variable in &new_environment_variables {
            if !pending_keys.insert(variable.key.clone()) {
                return Err(ProjectError::InvalidInput(format!(
                    "Environment variable '{}' is supplied more than once for template upgrade",
                    variable.key
                )));
            }
            if !definitions.contains_key(variable.key.as_str()) {
                return Err(ProjectError::InvalidInput(format!(
                    "Environment variable '{}' is not declared by template {}@{}",
                    variable.key, target.slug, target.version
                )));
            }
            if !variable.value.is_empty() {
                pending_non_empty_keys.insert(variable.key.clone());
            }
        }
        for definition in target_variables
            .iter()
            .filter(|definition| definition.required)
        {
            if !configured_keys.contains(&definition.name)
                && !pending_non_empty_keys.contains(&definition.name)
            {
                return Err(ProjectError::InvalidInput(format!(
                    "Required environment variable '{}' has no production value for template {}@{}",
                    definition.name, target.slug, target.version
                )));
            }
        }

        // A newer release may classify an existing configuration key as a
        // credential. Sensitivity is project/key-wide, so promote matching
        // rows in every scope before the snapshot changes; never leave a
        // preview/staging duplicate readable or demote an existing secret.
        for definition in target_variables
            .iter()
            .filter(|definition| definition.is_secret())
        {
            for existing in existing_variables
                .iter()
                .filter(|variable| variable.key == definition.name && !variable.is_secret)
            {
                let mut active: env_vars::ActiveModel = existing.clone().into();
                active.is_secret = Set(true);
                active.update(&txn).await?;
            }
        }

        for variable in new_environment_variables {
            // A concurrent settings edit may have created the input after the
            // upgrade preview. Preserve that user value instead of replacing
            // it with a stale submitted/default value.
            if configured_keys.contains(&variable.key) {
                continue;
            }
            let definition = definitions.get(variable.key.as_str()).ok_or_else(|| {
                ProjectError::InvalidInput(format!(
                    "Environment variable '{}' is not declared by template {}@{}",
                    variable.key, target.slug, target.version
                ))
            })?;
            if variable.value.is_empty() && definition.required {
                return Err(ProjectError::InvalidInput(format!(
                    "Required environment variable '{}' cannot be empty",
                    variable.key
                )));
            }
            let encrypted_value = self
                .encryption_service
                .encrypt_string(&variable.value)
                .map_err(|error| {
                    ProjectError::Other(format!(
                        "Failed to encrypt environment variable '{}' while upgrading project {project_id}: {error}",
                        variable.key
                    ))
                })?;
            if let Some(existing_variables) = production_variables.get_mut(&variable.key) {
                let existing_index = effective_index(existing_variables).ok_or_else(|| {
                    ProjectError::Other(format!(
                        "No effective production row found for environment variable '{}' while upgrading project {project_id}",
                        variable.key
                    ))
                })?;
                let existing = &mut existing_variables[existing_index];
                let mut active: env_vars::ActiveModel = existing.clone().into();
                active.value = Set(encrypted_value);
                active.is_encrypted = Set(true);
                active.is_secret =
                    Set(existing.is_secret || variable.is_secret || definition.is_secret());
                *existing = active.update(&txn).await?;
                configured_keys.insert(variable.key);
                continue;
            }
            let inserted = env_vars::ActiveModel {
                project_id: Set(project_id),
                environment_id: Set(None),
                key: Set(variable.key.clone()),
                value: Set(encrypted_value),
                include_in_preview: Set(false),
                is_encrypted: Set(true),
                is_secret: Set(variable.is_secret || definition.is_secret()),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
            env_var_environments::ActiveModel {
                env_var_id: Set(inserted.id),
                environment_id: Set(production_environment.id),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
            configured_keys.insert(variable.key);
        }

        let target_json = serde_json::to_value(&target).map_err(|error| {
            ProjectError::Other(format!(
                "Failed to persist target service template {}@{} for project {project_id}: {error}",
                target.slug, target.version
            ))
        })?;
        let mut active_project: projects::ActiveModel = project.into();
        active_project.project_type = Set(ProjectType::Service);
        active_project.template_slug = Set(Some(target.slug));
        active_project.service_template = Set(Some(target_json));
        active_project.preset_config = Set(Some(preset_config));
        active_project.deployment_config = Set(Some(deployment_config));
        let updated_project = active_project.update(&txn).await?;
        txn.commit().await?;

        if let Err(error) = self
            .queue_service
            .send(Job::ProjectUpdated(ProjectUpdatedJob {
                project_id: updated_project.id,
                project_name: updated_project.name.clone(),
            }))
            .await
        {
            warn!(
                project_id = updated_project.id,
                %error,
                "Failed to emit ProjectUpdated after service-template upgrade"
            );
        }

        Ok(self.map_written_project(updated_project).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_upgrade_replaces_only_an_unmodified_template_default() {
        assert_eq!(
            apply_new_template_default_when_unmodified(
                "image:v1".to_string(),
                "image:v1".to_string(),
                "image:v2".to_string(),
            ),
            "image:v2"
        );
        assert_eq!(
            apply_new_template_default_when_unmodified(
                "registry.example/custom@sha256:123".to_string(),
                "image:v1".to_string(),
                "image:v2".to_string(),
            ),
            "registry.example/custom@sha256:123"
        );
    }

    #[test]
    fn production_template_upgrade_preserves_global_and_production_variables() {
        let now = chrono::Utc::now();
        let variable = |id: i32, key: &str, environment_id: Option<i32>| env_vars::Model {
            id,
            project_id: 7,
            environment_id,
            key: key.to_string(),
            value: "encrypted".to_string(),
            created_at: now,
            updated_at: now,
            include_in_preview: false,
            is_encrypted: true,
            is_secret: false,
        };
        let variables = vec![
            variable(1, "GLOBAL", None),
            variable(2, "PRODUCTION_JOIN", None),
            variable(3, "PREVIEW_JOIN", None),
            variable(4, "PRODUCTION_LEGACY", Some(20)),
        ];
        let links = vec![
            env_var_environments::Model {
                id: 1,
                env_var_id: 2,
                environment_id: 20,
                created_at: now,
            },
            env_var_environments::Model {
                id: 2,
                env_var_id: 3,
                environment_id: 21,
                created_at: now,
            },
        ];

        let keys = production_configured_variable_keys(&variables, &links, 20);

        assert_eq!(
            keys,
            ["GLOBAL", "PRODUCTION_JOIN", "PRODUCTION_LEGACY"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn service_upgrade_may_add_but_not_remap_managed_service_bindings() {
        let mut applied = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("bundled Keycloak release")
            .primary()
            .clone();
        let mut target = applied.clone();
        target
            .database_bindings
            .entry("postgres".to_string())
            .or_default()
            .insert("NEW_ALIAS".to_string(), "POSTGRES_HOST".to_string());
        validate_service_binding_compatibility(&applied, &target)
            .expect("adding a managed-service alias is rollback-safe");

        let existing_alias = applied
            .database_bindings
            .get_mut("postgres")
            .and_then(|bindings| bindings.keys().next().cloned())
            .expect("Keycloak declares a PostgreSQL binding");
        target
            .database_bindings
            .get_mut("postgres")
            .expect("target PostgreSQL bindings")
            .insert(existing_alias, "REMAPPED_SOURCE".to_string());
        assert!(matches!(
            validate_service_binding_compatibility(&applied, &target),
            Err(ProjectError::InvalidInput(_))
        ));
    }

    #[test]
    fn service_upgrade_cannot_remove_required_service_without_custom_bindings() {
        let mut applied = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("bundled Keycloak release")
            .primary()
            .clone();
        applied.database_bindings.clear();
        let mut target = applied.clone();
        target.databases.clear();

        assert!(matches!(
            validate_service_binding_compatibility(&applied, &target),
            Err(ProjectError::InvalidInput(_))
        ));
    }
}
