// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Services linked to a sandbox (ADR-051) and the scoped runtime variables
//! issued for them.

use super::*;

impl SandboxService {
    /// Resolve the runtime variables available to this sandbox's attached
    /// project. This is a sandbox primitive: application workspaces and
    /// ordinary API-created sandboxes use the same path.
    ///
    /// The sandbox row is the authorization boundary. A caller cannot pass a
    /// different project ID and use sandbox ownership to reveal that project's
    /// credentials. Application workspaces keep `project_id` synchronized to
    /// their primary project when they are prepared.
    pub async fn runtime_environment(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<HashMap<String, String>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let project_id = row.project_id.ok_or_else(|| SandboxError::Validation {
            message: format!(
                "sandbox {sandbox_public_id} is not attached to a project; attach a project before requesting database runtime variables"
            ),
        })?;
        // Credentials without connectivity are unusable. Reconcile the generic
        // sandbox data network at issuance time so API-created sandboxes get
        // the same behavior as application workspaces, including services
        // linked after the sandbox itself was created.
        self.synchronize_data_network(user_id, sandbox_public_id, &[project_id])
            .await?;
        let environments = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        let environment = environments
            .iter()
            .find(|environment| environment.name.eq_ignore_ascii_case("production"))
            .or_else(|| environments.first())
            .ok_or_else(|| SandboxError::RuntimeEnvironmentNotFound {
                sandbox_id: sandbox_public_id.to_string(),
                project_id,
            })?;
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .order_by_asc(project_services::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let provider =
            self.runtime_credentials
                .as_ref()
                .ok_or_else(|| SandboxError::Unavailable {
                    reason: "runtime credential provider is not registered".to_string(),
                })?;
        let service_count = links.len();
        let mut variables = HashMap::new();
        for link in links {
            // A sandbox runs code nobody reviewed, so it gets credentials of
            // its own where the service can mint them (ADR-051). Only a
            // service type with no such mechanism yet keeps the credentials
            // a deployment of the environment would receive.
            let scope = temps_core::SandboxScope {
                sandbox_public_id: sandbox_public_id.to_string(),
                project_id: Some(project_id),
            };
            let issued = match provider.issue_for_sandbox(link.service_id, &scope).await {
                Ok(issued) => issued,
                Err(temps_core::SandboxRuntimeCredentialsError::NoScopedCredentials { .. }) => {
                    provider
                        .issue(link.service_id, project_id, environment.id)
                        .await
                        .map_err(|error| SandboxError::RuntimeCredentialsFailed {
                            sandbox_id: sandbox_public_id.to_string(),
                            service_id: link.service_id,
                            reason: error.to_string(),
                        })?
                }
                Err(error) => {
                    return Err(SandboxError::RuntimeCredentialsFailed {
                        sandbox_id: sandbox_public_id.to_string(),
                        service_id: link.service_id,
                        reason: error.to_string(),
                    })
                }
            };
            merge_runtime_variables(&mut variables, issued).map_err(|variable| {
                SandboxError::RuntimeVariableConflict {
                    sandbox_id: sandbox_public_id.to_string(),
                    variable,
                }
            })?;
        }
        let mut variable_names = variables.keys().cloned().collect::<Vec<_>>();
        variable_names.sort();
        self.record_event(
            row.id,
            "runtime_credentials_issued",
            Some(serde_json::json!({
                "project_id": project_id,
                "environment_id": environment.id,
                "service_count": service_count,
                "variable_names": variable_names,
            })),
        )
        .await;
        Ok(variables)
    }

    /// The services `sandbox_public_id` is linked to (ADR-051), oldest first.
    pub async fn linked_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<Vec<LinkedService>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.linked_services_of(row.id).await
    }

    pub(super) async fn linked_services_of(
        &self,
        sandbox_id: i32,
    ) -> Result<Vec<LinkedService>, SandboxError> {
        Ok(sandbox_services::Entity::find()
            .filter(sandbox_services::Column::SandboxId.eq(sandbox_id))
            .order_by_asc(sandbox_services::Column::Id)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|link| LinkedService {
                service_id: link.service_id,
                project_id: link.project_id,
            })
            .collect())
    }

    /// The services of `project_id` a sandbox can be linked to: those whose
    /// type has a sandbox access engine and that run where a sandbox can
    /// reach them. A service the sandbox cannot use is not offered.
    pub async fn linkable_project_services(
        &self,
        project_id: i32,
    ) -> Result<Vec<i32>, SandboxError> {
        let service_ids = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .order_by_asc(project_services::Column::ServiceId)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|link| link.service_id)
            .collect::<Vec<_>>();
        let mut linkable = Vec::new();
        for service_id in service_ids {
            if self.service_supports_sandbox(service_id).await? {
                linkable.push(service_id);
            }
        }
        Ok(linkable)
    }

    /// Whether a sandbox can be linked to `service_id`: its type has a sandbox
    /// access engine and it runs where a sandbox can reach it.
    pub async fn service_supports_sandbox(&self, service_id: i32) -> Result<bool, SandboxError> {
        let containers = self
            .credentials_provider()?
            .sandbox_access_containers(service_id)
            .await
            .map_err(|error| match error {
                temps_core::SandboxRuntimeCredentialsError::ServiceNotFound { service_id } => {
                    SandboxError::ServiceNotFound { service_id }
                }
                other => SandboxError::RuntimeCredentialsFailed {
                    sandbox_id: String::new(),
                    service_id,
                    reason: other.to_string(),
                },
            })?;
        Ok(!containers.is_empty())
    }

    /// Link `sandbox_public_id` to `links` and give it what it needs to use
    /// them: the services' containers on its private network, and the
    /// variables to run code with. A link that is already there is kept as it
    /// is.
    ///
    /// Nothing is left half done: if the network or a credential fails, or two
    /// services would set the same variable, the links this call added are
    /// removed again and the error returned.
    pub async fn link_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        links: &[LinkedService],
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let existing = self.linked_services_of(row.id).await?;
        let mut added = Vec::new();
        for link in links {
            if existing
                .iter()
                .any(|known| known.service_id == link.service_id)
                || added.contains(&link.service_id)
            {
                continue;
            }
            sandbox_services::ActiveModel {
                sandbox_id: Set(row.id),
                service_id: Set(link.service_id),
                project_id: Set(link.project_id),
                created_by: Set(Some(user_id)),
                ..Default::default()
            }
            .insert(self.db.as_ref())
            .await?;
            added.push(link.service_id);
        }
        match self.rebuild_scoped(&row).await {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                if !added.is_empty() {
                    self.delete_links(row.id, &added).await?;
                    if let Err(restore) = self.rebuild_network(&row).await {
                        tracing::warn!(
                            sandbox_id = %row.public_id,
                            "could not restore the sandbox's connections after a failed link: {restore}"
                        );
                    }
                }
                Err(error)
            }
        }
    }

    /// Unlink `sandbox_public_id` from `service_ids` and take their containers
    /// off its private network, which drops the sandbox's open connections to
    /// them. The data in the sandbox's database or bucket stays.
    pub async fn unlink_services(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        service_ids: &[i32],
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        self.delete_links(row.id, service_ids).await?;
        self.rebuild_scoped(&row).await
    }

    /// Remove every link of `sandbox_public_id` made through `project_id`.
    /// Returns the services that were unlinked.
    pub async fn unlink_project(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        project_id: i32,
    ) -> Result<(Vec<i32>, ScopedRuntime), SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let removed = self
            .linked_services_of(row.id)
            .await?
            .into_iter()
            .filter(|link| link.project_id == Some(project_id))
            .map(|link| link.service_id)
            .collect::<Vec<_>>();
        self.delete_links(row.id, &removed).await?;
        let runtime = self.rebuild_scoped(&row).await?;
        Ok((removed, runtime))
    }

    /// The variables for running a command in `sandbox_public_id`, resolved
    /// fresh from its links and without touching the network.
    pub async fn scoped_runtime_variables(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
    ) -> Result<ScopedRuntime, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.issue_scoped(&row).await
    }

    async fn delete_links(&self, sandbox_id: i32, service_ids: &[i32]) -> Result<(), SandboxError> {
        if service_ids.is_empty() {
            return Ok(());
        }
        sandbox_services::Entity::delete_many()
            .filter(sandbox_services::Column::SandboxId.eq(sandbox_id))
            .filter(sandbox_services::Column::ServiceId.is_in(service_ids.iter().copied()))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Rebuild the private network from the links, then issue the variables.
    async fn rebuild_scoped(&self, row: &sandboxes::Model) -> Result<ScopedRuntime, SandboxError> {
        self.rebuild_network(row).await?;
        self.issue_scoped(row).await
    }

    pub(super) fn credentials_provider(
        &self,
    ) -> Result<&Arc<dyn temps_core::SandboxRuntimeCredentialsProvider>, SandboxError> {
        self.runtime_credentials
            .as_ref()
            .ok_or_else(|| SandboxError::Unavailable {
                reason: "runtime credential provider is not registered".to_string(),
            })
    }

    async fn issue_scoped(&self, row: &sandboxes::Model) -> Result<ScopedRuntime, SandboxError> {
        let mut runtime = ScopedRuntime::default();
        let links = self.linked_services_of(row.id).await?;
        if links.is_empty() {
            return Ok(runtime);
        }
        let provider = self.credentials_provider()?;
        for link in &links {
            let scope = temps_core::SandboxScope {
                sandbox_public_id: row.public_id.clone(),
                project_id: link.project_id,
            };
            let issued = match provider.issue_for_sandbox(link.service_id, &scope).await {
                Ok(issued) => issued,
                Err(temps_core::SandboxRuntimeCredentialsError::NoScopedCredentials { .. }) => {
                    runtime.skipped_service_ids.push(link.service_id);
                    continue;
                }
                Err(error) => {
                    return Err(SandboxError::RuntimeCredentialsFailed {
                        sandbox_id: row.public_id.clone(),
                        service_id: link.service_id,
                        reason: error.to_string(),
                    })
                }
            };
            merge_runtime_variables(&mut runtime.variables, issued).map_err(|variable| {
                SandboxError::RuntimeVariableConflict {
                    sandbox_id: row.public_id.clone(),
                    variable,
                }
            })?;
            runtime.connected_service_ids.push(link.service_id);
        }
        let mut variable_names = runtime.variable_names();
        variable_names.sort();
        self.record_event(
            row.id,
            "scoped_credentials_issued",
            Some(serde_json::json!({
                "service_ids": runtime.connected_service_ids,
                "variable_names": variable_names,
            })),
        )
        .await;
        Ok(runtime)
    }
}

/// A service a sandbox is linked to (ADR-051).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedService {
    pub service_id: i32,
    /// The project the link was made through, which names the sandbox's
    /// database and bucket; `None` for a link made to the service directly.
    pub project_id: Option<i32>,
}

/// What a sandbox was connected to, and the variables to run code with.
#[derive(Default)]
pub struct ScopedRuntime {
    /// Secret values: never log or persist them.
    pub variables: HashMap<String, String>,
    /// Services the variables reach.
    pub connected_service_ids: Vec<i32>,
    /// Linked services left out because their type has no scoped credentials
    /// yet.
    pub skipped_service_ids: Vec<i32>,
}

impl ScopedRuntime {
    pub fn variable_names(&self) -> Vec<String> {
        self.variables.keys().cloned().collect()
    }
}

impl std::fmt::Debug for ScopedRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names = self.variable_names();
        names.sort();
        formatter
            .debug_struct("ScopedRuntime")
            .field("variable_names", &names)
            .field("connected_service_ids", &self.connected_service_ids)
            .field("skipped_service_ids", &self.skipped_service_ids)
            .finish()
    }
}

pub(super) fn merge_runtime_variables(
    destination: &mut HashMap<String, String>,
    issued: HashMap<String, String>,
) -> Result<(), String> {
    for (name, value) in issued {
        if destination
            .get(&name)
            .is_some_and(|existing| existing != &value)
        {
            return Err(name);
        }
        destination.insert(name, value);
    }
    Ok(())
}
