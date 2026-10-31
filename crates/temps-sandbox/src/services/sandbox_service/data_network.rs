// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-sandbox data network that connects a sandbox to its linked
//! services' containers.

use super::*;

impl SandboxService {
    /// Put the workspace and only the databases linked to its application
    /// projects on a private, internal Docker network. This grants direct
    /// data-plane reachability without placing a reusable Temps API token in
    /// the sandbox and without exposing unrelated tenants' services.
    pub async fn synchronize_application_data_network(
        &self,
        user_id: i32,
        _application_public_id: &str,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        self.synchronize_data_network(user_id, sandbox_public_id, project_ids)
            .await
    }

    /// Reconcile the isolated data network for any sandbox from an authorized
    /// list of project IDs. The network identity belongs to the sandbox, not
    /// to whichever product surface created it.
    pub async fn synchronize_data_network(
        &self,
        user_id: i32,
        sandbox_public_id: &str,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        let key = managed_application_id_from_name(&row.name).unwrap_or(sandbox_public_id);
        let _workspace_mutation = self.lock_application_workspace(key).await;
        let row = self.find_by_public_id(sandbox_public_id, user_id).await?;
        self.configure_application_data_network_locked(&row, project_ids)
            .await
    }

    pub(super) async fn configure_application_data_network_locked(
        &self,
        row: &sandboxes::Model,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let containers = self
            .application_data_service_containers(project_ids)
            .await?;
        let network_name = sandbox_data_network_name(&row.public_id)?;
        self.registry
            .configure_application_network(row.id, &row.public_id, &network_name, &containers)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))?;
        Ok(containers)
    }

    /// Return the database containers currently implied by the committed
    /// project topology without changing container or network state.
    pub async fn application_data_service_count(
        &self,
        project_ids: &[i32],
    ) -> Result<usize, SandboxError> {
        Ok(self
            .application_data_service_containers(project_ids)
            .await?
            .len())
    }

    pub(super) async fn rebuild_network(&self, row: &sandboxes::Model) -> Result<(), SandboxError> {
        let containers = self.scoped_service_containers(row).await?;
        let network_name = sandbox_data_network_name(&row.public_id)?;
        self.registry
            .configure_application_network(row.id, &row.public_id, &network_name, &containers)
            .await
            .map_err(|error| from_agent_error(&row.public_id, error))
    }

    /// The containers of the services the sandbox is linked to, as each
    /// service's engine names them. A service no engine covers contributes
    /// none, so it stays off the sandbox's network.
    async fn scoped_service_containers(
        &self,
        row: &sandboxes::Model,
    ) -> Result<Vec<String>, SandboxError> {
        let links = self.linked_services_of(row.id).await?;
        if links.is_empty() {
            return Ok(Vec::new());
        }
        let provider = self.credentials_provider()?;
        let mut containers = Vec::new();
        for link in links {
            let named = provider
                .sandbox_access_containers(link.service_id)
                .await
                .map_err(|error| SandboxError::RuntimeCredentialsFailed {
                    sandbox_id: row.public_id.clone(),
                    service_id: link.service_id,
                    reason: error.to_string(),
                })?;
            containers.extend(named);
        }
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    async fn application_data_service_containers(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<String>, SandboxError> {
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.is_in(project_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let service_ids = links
            .iter()
            .map(|link| link.service_id)
            .collect::<std::collections::HashSet<_>>();
        let services = if service_ids.is_empty() {
            Vec::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids.iter().copied()))
                .all(self.db.as_ref())
                .await?
        };
        let service_ids = services
            .iter()
            .map(|service| service.id)
            .collect::<std::collections::HashSet<_>>();
        let members = if service_ids.is_empty() {
            Vec::new()
        } else {
            service_members::Entity::find()
                .filter(service_members::Column::ServiceId.is_in(service_ids.iter().copied()))
                .filter(service_members::Column::NodeId.is_null())
                .all(self.db.as_ref())
                .await?
        };
        let mut containers = Vec::new();
        for service in services {
            if service.node_id.is_some() {
                continue;
            }
            let service_members = members
                .iter()
                .filter(|member| member.service_id == service.id)
                .map(|member| member.container_name.clone())
                .collect::<Vec<_>>();
            if !service_members.is_empty() {
                containers.extend(service_members);
                continue;
            }
            let derived =
                service
                    .container_name
                    .unwrap_or_else(|| match service.service_type.as_str() {
                        "postgres" => format!("postgres-{}", service.name),
                        "redis" => format!("redis-{}", service.name),
                        "mongodb" => format!("temps-mongodb-{}", service.name),
                        "mariadb" | "mysql" => format!("mariadb-{}", service.name),
                        _ => String::new(),
                    });
            if !derived.is_empty() {
                containers.push(derived);
            }
        }
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    /// Reconcile every running application workspace linked to a project
    /// after that project's service topology changes. Revocations are applied
    /// from the committed database topology; if Docker cannot confirm the
    /// narrower membership, compute is stopped so existing sockets cannot
    /// retain access behind the control plane's back.
    pub async fn reconcile_application_networks_for_project(
        &self,
        project_id: i32,
    ) -> Result<(), SandboxError> {
        let affected_links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        if affected_links.is_empty() {
            return Ok(());
        }
        let application_ids = affected_links
            .iter()
            .map(|link| link.application_id)
            .collect::<Vec<_>>();
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::Id.is_in(application_ids))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;

        let mut first_error = None;
        for application in applications {
            let project_ids = ai_application_projects::Entity::find()
                .filter(ai_application_projects::Column::ApplicationId.eq(application.id))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|link| link.project_id)
                .collect::<Vec<_>>();
            let Some(summary) = self
                .application_workspace_summary(application.created_by, &application.public_id)
                .await?
                .filter(|summary| matches!(summary.status.as_str(), "running" | "recovering"))
            else {
                continue;
            };
            if let Err(error) = self
                .synchronize_application_data_network(
                    application.created_by,
                    &application.public_id,
                    &summary.public_id,
                    &project_ids,
                )
                .await
            {
                let quarantine_error = format!(
                    "Application data-network reconciliation failed; workspace compute was stopped: {error}"
                );
                if let Err(persist_error) = ai_application_workspaces::Entity::update_many()
                    .col_expr(
                        ai_application_workspaces::Column::DesiredState,
                        Expr::value("quarantined"),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::LastError,
                        Expr::value(Some(quarantine_error)),
                    )
                    .col_expr(
                        ai_application_workspaces::Column::UpdatedAt,
                        Expr::value(Utc::now()),
                    )
                    .filter(ai_application_workspaces::Column::ApplicationId.eq(application.id))
                    .exec(self.db.as_ref())
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        error = %persist_error,
                        "Failed to persist fail-closed application workspace quarantine"
                    );
                }
                if let Err(stop_error) = self
                    .pause_sandbox(&summary.public_id, application.created_by)
                    .await
                {
                    tracing::error!(
                        application_id = %application.public_id,
                        sandbox_id = %summary.public_id,
                        error = %stop_error,
                        "Failed to stop workspace after data-network reconciliation failure"
                    );
                }
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl temps_core::ApplicationDataNetworkReconciler for SandboxService {
    async fn project_topology_changed(
        &self,
        project_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reconcile_application_networks_for_project(project_id)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

pub(super) fn sandbox_data_network_name(sandbox_public_id: &str) -> Result<String, SandboxError> {
    let opaque_id = sandbox_public_id
        .strip_prefix("sbx_")
        .unwrap_or(sandbox_public_id);
    if opaque_id.is_empty()
        || opaque_id.len() > 40
        || !opaque_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SandboxError::Validation {
            message: "sandbox data-network identifier is invalid".to_string(),
        });
    }
    Ok(format!("temps-sandbox-data-{opaque_id}"))
}
