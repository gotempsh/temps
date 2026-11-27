// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins that run as Temps services, as an administrator manages them:
//! adding a service as a plugin, removing it, and seeing how each is doing.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_entities::{environments, projects};
use temps_plugin_protocol::PluginManifest;
use tracing::{debug, info};

use super::{ExternalPluginsError, ExternalPluginsService};
use crate::manager::ServicePluginState;
use crate::service_runtime::ServiceBinding;

/// One plugin that runs as a service, with the names an administrator knows
/// the service by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePluginView {
    pub name: String,
    /// The service and environment it runs as; `None` once either was
    /// deleted.
    pub binding: Option<ServiceBinding>,
    pub service_name: Option<String>,
    pub service_slug: Option<String>,
    pub environment_name: Option<String>,
    /// Whether the environment stops when idle and starts on the next
    /// request, which is what keeps an unused plugin from holding memory.
    pub stops_when_idle: bool,
    /// The deployment Temps is attached to, while the plugin is running.
    pub deployment_id: Option<i32>,
    pub state: ServicePluginState,
    /// Why it is not running, in words an administrator can be shown.
    pub detail: Option<String>,
}

impl ExternalPluginsService {
    /// Keep routes, event subscriptions and AI tools in step with service
    /// plugins the manager attaches to, detaches from or finds asleep on its
    /// own: after a deployment, an idle stop, or a request that started one.
    pub(crate) fn follow_service_plugins(self: &Arc<Self>) {
        let changed = self.manager.service_plugins_changed();
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                changed.notified().await;
                let Some(service) = service.upgrade() else {
                    return;
                };
                let _lifecycle = service.lifecycle.lock().await;
                if service.closing.load(Ordering::Acquire) {
                    return;
                }
                debug!("A service plugin changed; rebuilding plugin routes");
                service.refresh_runtime_surfaces().await;
            }
        });
    }

    /// Make the service `binding` names a plugin: attach to its running
    /// container, read its manifest and route requests to it.
    ///
    /// The plugin starts with no grants. Nothing about the service changes.
    pub async fn add_service_plugin(
        &self,
        binding: ServiceBinding,
    ) -> Result<PluginManifest, ExternalPluginsError> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        let manifest = self.manager.attach_service(binding, None).await?;
        info!(
            plugin = %manifest.name,
            project_id = binding.project_id,
            environment_id = binding.environment_id,
            "A service was added as a plugin"
        );
        self.refresh_runtime_surfaces().await;
        Ok(manifest)
    }

    /// Stop treating a service as a plugin and revoke everything the plugin
    /// was granted. The service itself is left as it is: stopping or deleting
    /// it is done from the service's own page.
    pub(crate) async fn remove_service_plugin(
        &self,
        name: &str,
    ) -> Result<(), ExternalPluginsError> {
        // Invalidate before the cancellable write: a lost acknowledgement can
        // mean revocation committed. Recovery reloads authoritative consent.
        self.ai_tools
            .publish(self.ai_tools.snapshot().without_plugin(name));
        self.schedule_consent_recovery();
        // Revoke first: if that fails the plugin stays attached and listed,
        // instead of disappearing while its grants survive.
        crate::grants::PluginGrantService::new(self.db.clone())
            .revoke_actor(name)
            .await?;
        self.manager.detach_service(name).await;
        info!(plugin = %name, "A service plugin was removed; its service keeps running");
        self.refresh_runtime_surfaces().await;
        Ok(())
    }

    /// Every plugin that runs as a service, and how it is doing.
    pub async fn service_plugins(&self) -> Result<Vec<ServicePluginView>, ExternalPluginsError> {
        let statuses = self.manager.service_plugin_statuses().await;
        let project_ids: Vec<i32> = statuses
            .iter()
            .filter_map(|status| status.binding.map(|binding| binding.project_id))
            .collect();
        let environment_ids: Vec<i32> = statuses
            .iter()
            .filter_map(|status| status.binding.map(|binding| binding.environment_id))
            .collect();
        let read = |source| ExternalPluginsError::ServiceLookup { source };
        let projects: HashMap<i32, projects::Model> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids))
                .all(self.db.as_ref())
                .await
                .map_err(read)?
                .into_iter()
                .map(|project| (project.id, project))
                .collect()
        };
        let environments: HashMap<i32, environments::Model> = if environment_ids.is_empty() {
            HashMap::new()
        } else {
            environments::Entity::find()
                .filter(environments::Column::Id.is_in(environment_ids))
                .all(self.db.as_ref())
                .await
                .map_err(read)?
                .into_iter()
                .map(|environment| (environment.id, environment))
                .collect()
        };
        Ok(statuses
            .into_iter()
            .map(|status| {
                let project = status
                    .binding
                    .and_then(|binding| projects.get(&binding.project_id));
                let environment = status
                    .binding
                    .and_then(|binding| environments.get(&binding.environment_id));
                ServicePluginView {
                    service_name: project.map(|project| project.name.clone()),
                    service_slug: project.map(|project| project.slug.clone()),
                    environment_name: environment.map(|environment| environment.name.clone()),
                    stops_when_idle: environment
                        .and_then(|environment| environment.deployment_config.as_ref())
                        .is_some_and(|config| config.on_demand),
                    name: status.name,
                    binding: status.binding,
                    deployment_id: status.deployment_id,
                    state: status.state,
                    detail: status.detail,
                }
            })
            .collect())
    }
}
