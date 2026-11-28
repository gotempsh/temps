// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins that run as Temps services, as an administrator manages them:
//! adding a service as a plugin, removing it, and seeing how each is doing.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_entities::{deployments, environments, external_services, project_services, projects};
use temps_plugin_protocol::PluginManifest;
use tracing::{debug, info};

use super::{ExternalPluginsError, ExternalPluginsService};
use crate::manager::ServicePluginState;
use crate::service_runtime::ServiceBinding;
use crate::tier::PluginTier;
use crate::updates::{AccessDifference, PluginBuilds};

/// A build of a plugin's service that waits for an administrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUpdateView {
    /// The deployment that waits.
    pub deployment_id: i32,
    /// What an approval must quote: this build answering this manifest.
    pub review: String,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// The image that waits, when the deployment was made from an image in
    /// a registry.
    pub image: Option<String>,
    /// The content the image is, when Temps recorded it: what a registry
    /// image resolved to, or what an uploaded image was imported as.
    pub image_id: Option<String>,
    /// What it asks for compared with the approved build.
    pub difference: AccessDifference,
}

/// The limits the plugin's container runs with: those of the deployment its
/// service runs, as recorded when it was deployed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginResourceLimits {
    /// The deployment the limits belong to.
    pub deployment_id: i32,
    /// Megabytes of memory; `None` when the service is not capped.
    pub memory_limit_mb: Option<i32>,
    /// Microcores of CPU (1,000,000 is one core); `None` when not capped.
    pub cpu_limit_microcores: Option<i32>,
}

/// An update an administrator approved.
#[derive(Debug, Clone)]
pub struct ApprovedUpdate {
    pub manifest: PluginManifest,
    /// The deployment the plugin was pinned to before.
    pub previous_deployment_id: Option<i32>,
    /// The deployment it is pinned to now.
    pub deployment_id: i32,
    pub difference: AccessDifference,
}

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
    /// Who stands behind the plugin, which bounds what it may ask for.
    pub tier: PluginTier,
    /// The managed databases and stores linked to the plugin's service: the
    /// plugin's own data. Empty when it has none.
    pub own_storage: Vec<String>,
    /// The deployment whose build an administrator approved. Temps attaches
    /// to no other.
    pub pinned_deployment_id: Option<i32>,
    /// A newer build that waits for approval, if any.
    pub pending_update: Option<PendingUpdateView>,
    /// Since when the plugin is switched off, while it is.
    pub disabled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The user who switched it off, while that user exists.
    pub disabled_by: Option<i32>,
    /// The memory and CPU the plugin's container is limited to. `None`
    /// before the plugin has a deployment Temps knows.
    pub resource_limits: Option<PluginResourceLimits>,
    /// The image the plugin runs, when its service was deployed from an
    /// image instead of built from a repository: the reference that was
    /// approved.
    pub image: Option<String>,
    /// The Docker image ID the plugin's image is: what a registry image
    /// resolved to when it was deployed, or what an image uploaded from a
    /// client was imported as (which has no `image`). The content that was
    /// approved, which a moved tag does not change.
    pub image_id: Option<String>,
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

    /// Cut `name` off from Temps at once, and keep it that way across
    /// restarts. Its grants, approved build and service are left alone.
    ///
    /// The plugin is disconnected before anything is saved: a switch that
    /// could not be recorded still holds until Temps restarts, and says so.
    pub async fn disable_plugin(&self, name: &str, by: i32) -> Result<(), ExternalPluginsError> {
        if !crate::naming::valid_plugin_name(name) {
            return Err(ExternalPluginsError::InvalidPluginName { name: name.into() });
        }
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        if !self.manager.disable_service(name).await {
            return Err(ExternalPluginsError::NotInstalled { name: name.into() });
        }
        // Its AI tools go with it, before the cancellable write.
        self.ai_tools
            .publish(self.ai_tools.snapshot().without_plugin(name));
        let recorded = crate::grants::PluginGrantService::new(self.db.clone())
            .set_disabled(name, Some(by))
            .await;
        self.refresh_runtime_surfaces().await;
        recorded.map_err(|source| ExternalPluginsError::DisableNotRecorded {
            name: name.into(),
            source,
        })?;
        info!(plugin = %name, user_id = by, "A plugin was disabled; its service keeps running");
        Ok(())
    }

    /// Let `name` connect again, with the grants it had, and connect to it
    /// when its approved build runs.
    pub async fn enable_plugin(&self, name: &str) -> Result<(), ExternalPluginsError> {
        if !crate::naming::valid_plugin_name(name) {
            return Err(ExternalPluginsError::InvalidPluginName { name: name.into() });
        }
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        if !self.manager.is_service_plugin(name).await {
            return Err(ExternalPluginsError::NotInstalled { name: name.into() });
        }
        // Saved first: a plugin is never connected while its row says off.
        crate::grants::PluginGrantService::new(self.db.clone())
            .set_disabled(name, None)
            .await
            .map_err(|source| ExternalPluginsError::EnableNotRecorded {
                name: name.into(),
                source,
            })?;
        self.manager.enable_service(name).await;
        info!(plugin = %name, "A plugin was enabled");
        self.refresh_runtime_surfaces().await;
        Ok(())
    }

    /// Approve the update of `name` that waits and attach to it.
    ///
    /// `review` is what the administrator was shown: one build answering one
    /// manifest. If the service runs anything else by now, nothing is
    /// attached and the new build waits for a review of its own. Approving
    /// changes which build runs as the plugin, never what the plugin is
    /// allowed: permissions it newly asks for stay off until they are chosen.
    pub async fn approve_plugin_update(
        &self,
        name: &str,
        review: &str,
    ) -> Result<ApprovedUpdate, ExternalPluginsError> {
        if !crate::naming::valid_plugin_name(name) {
            return Err(ExternalPluginsError::InvalidPluginName { name: name.into() });
        }
        let _lifecycle = self.lifecycle.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(ExternalPluginsError::ShuttingDown);
        }
        let pin = PluginBuilds::new(self.db.clone())
            .pin_of(name)
            .await?
            .ok_or_else(|| ExternalPluginsError::NotInstalled { name: name.into() })?;
        let Some(pending) = pin.pending.as_ref() else {
            return Err(ExternalPluginsError::NoUpdateWaiting { name: name.into() });
        };
        if pending.review != review {
            return Err(ExternalPluginsError::UpdateChanged {
                name: name.into(),
                deployment_id: pending.deployment_id,
            });
        }
        let difference = AccessDifference::between(pin.approved.as_ref(), &pending.manifest);
        let manifest = match self.manager.approve_service_update(name, review).await {
            Ok(manifest) => manifest,
            // The service was redeployed, or answers another manifest, since
            // the review was shown.
            Err(crate::service_runtime::ServiceAttachError::UpdateWaiting {
                deployment_id,
                ..
            }) => {
                self.refresh_runtime_surfaces().await;
                return Err(ExternalPluginsError::UpdateChanged {
                    name: name.into(),
                    deployment_id,
                });
            }
            Err(error) => {
                self.refresh_runtime_surfaces().await;
                return Err(error.into());
            }
        };
        info!(
            plugin = %name,
            previous_deployment_id = ?pin.deployment_id,
            deployment_id = pending.deployment_id,
            asks_for_more = difference.asks_for_more,
            "A plugin update was approved"
        );
        self.refresh_runtime_surfaces().await;
        Ok(ApprovedUpdate {
            manifest,
            previous_deployment_id: pin.deployment_id,
            deployment_id: pending.deployment_id,
            difference,
        })
    }

    /// Every plugin that runs as a service, and how it is doing.
    pub async fn service_plugins(&self) -> Result<Vec<ServicePluginView>, ExternalPluginsError> {
        let statuses = self.manager.service_plugin_statuses().await;
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
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
        // What is linked to each plugin's service, in one query for all of
        // them: a plugin keeps its own data there, never in Temps's database.
        let mut own_storage: HashMap<i32, Vec<String>> = HashMap::new();
        if !projects.is_empty() {
            let linked = project_services::Entity::find()
                .filter(project_services::Column::ProjectId.is_in(projects.keys().copied()))
                .find_also_related(external_services::Entity)
                .all(self.db.as_ref())
                .await
                .map_err(read)?;
            for (link, store) in linked {
                if let Some(store) = store {
                    own_storage
                        .entry(link.project_id)
                        .or_default()
                        .push(store.name);
                }
            }
            for names in own_storage.values_mut() {
                names.sort();
            }
        }
        let stored: HashMap<String, crate::grants::StoredServicePlugin> =
            crate::grants::PluginGrantService::new(self.db.clone())
                .service_plugins()
                .await?
                .into_iter()
                .map(|plugin| (plugin.name.clone(), plugin))
                .collect();
        // The limits each plugin's container runs with, from the deployment
        // it is attached or pinned to, in one query. Only the limits are
        // read: the same record holds the deployment's environment.
        let deployment_of = |status: &crate::manager::services::ServicePluginStatus| {
            status.deployment_id.or_else(|| {
                stored
                    .get(&status.name)
                    .and_then(|plugin| plugin.pin.deployment_id)
            })
        };
        // The deployment that waits is read too: its image is what an
        // administrator is asked to approve.
        let deployment_ids: Vec<i32> = statuses
            .iter()
            .filter_map(deployment_of)
            .chain(stored.values().filter_map(|plugin| {
                plugin
                    .pin
                    .pending
                    .as_ref()
                    .map(|pending| pending.deployment_id)
            }))
            .collect();
        let mut images: HashMap<i32, String> = HashMap::new();
        let mut image_ids: HashMap<i32, String> = HashMap::new();
        let limits: HashMap<i32, PluginResourceLimits> = if deployment_ids.is_empty() {
            HashMap::new()
        } else {
            use sea_orm::QuerySelect;
            deployments::Entity::find()
                .select_only()
                .column(deployments::Column::Id)
                .column(deployments::Column::DeploymentConfig)
                .column(deployments::Column::Metadata)
                .filter(deployments::Column::Id.is_in(deployment_ids))
                .into_tuple::<(
                    i32,
                    Option<temps_entities::deployment_config::DeploymentConfigSnapshot>,
                    Option<deployments::DeploymentMetadata>,
                )>()
                .all(self.db.as_ref())
                .await
                .map_err(read)?
                .into_iter()
                .map(|(deployment_id, config, metadata)| {
                    if let Some(id) = crate::service_runtime::image_content_id(metadata.as_ref()) {
                        image_ids.insert(deployment_id, id.to_string());
                    }
                    // An image uploaded from a client is kept under a name Temps
                    // made up; only a reference somebody named is shown.
                    if let Some(image) = metadata
                        .filter(|metadata| !metadata.image_uploaded_locally)
                        .and_then(|metadata| metadata.external_image_ref)
                    {
                        images.insert(deployment_id, image);
                    }
                    (
                        deployment_id,
                        PluginResourceLimits {
                            deployment_id,
                            // Zero is the explicit "no limit".
                            memory_limit_mb: config
                                .as_ref()
                                .and_then(|config| config.memory_limit)
                                .filter(|megabytes| *megabytes > 0),
                            cpu_limit_microcores: config
                                .as_ref()
                                .and_then(|config| config.cpu_limit)
                                .filter(|microcores| *microcores > 0),
                        },
                    )
                })
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
                let resource_limits = deployment_of(&status)
                    .and_then(|deployment_id| limits.get(&deployment_id).copied());
                let stored = stored.get(&status.name);
                let pin = stored.map(|plugin| &plugin.pin);
                ServicePluginView {
                    tier: stored.map(|plugin| plugin.tier).unwrap_or_default(),
                    disabled_at: stored
                        .filter(|plugin| plugin.disabled)
                        .and_then(|plugin| plugin.disabled_at),
                    disabled_by: stored
                        .filter(|plugin| plugin.disabled)
                        .and_then(|plugin| plugin.disabled_by),
                    resource_limits,
                    pinned_deployment_id: pin.and_then(|pin| pin.deployment_id),
                    pending_update: pin.and_then(|pin| {
                        let pending = pin.pending.as_ref()?;
                        Some(PendingUpdateView {
                            deployment_id: pending.deployment_id,
                            review: pending.review.clone(),
                            since: pending.since,
                            image: images.get(&pending.deployment_id).cloned(),
                            image_id: image_ids.get(&pending.deployment_id).cloned(),
                            difference: pin.pending_difference()?,
                        })
                    }),
                    image: deployment_of(&status)
                        .and_then(|deployment_id| images.get(&deployment_id).cloned()),
                    image_id: deployment_of(&status)
                        .and_then(|deployment_id| image_ids.get(&deployment_id).cloned()),
                    own_storage: status
                        .binding
                        .and_then(|binding| own_storage.get(&binding.project_id))
                        .cloned()
                        .unwrap_or_default(),
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
