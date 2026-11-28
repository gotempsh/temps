// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugins that run as Temps services: attaching to one, staying attached
//! across deployments, and starting an idle one again.
//!
//! The deployment owns the container. This module only connects to it, so
//! detaching never stops anything, and a plugin whose service is stopped for
//! being idle stays listed and is attached to again by the next request.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{OnceLock, Weak};
use std::time::Instant;

use sea_orm::EntityTrait;
use serde::Serialize;
use tokio::sync::Notify;
use tracing::error;
use utoipa::ToSchema;

use super::*;
use crate::grants::PluginGrantService;
use crate::proxy::{ServiceRoute, ServiceTarget};
use crate::service_runtime::{
    self, service_auth_secret, ServiceAttachError, ServiceBinding, ServiceRuntime,
};
use crate::updates::{AccessDifference, PendingUpdate, PluginBuilds};

/// How long after a failed attempt a request may make Temps try again, so a
/// broken plugin costs one attempt per interval and not one per request.
const RETRY_COOLDOWN: Duration = Duration::from_secs(10);

/// Waits between attempts to attach again after a plugin's channel closes: a
/// new deployment replaced the container, or the container restarted.
const REATTACH_DELAYS: [Duration; 5] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// How long starting an idle environment may take when it configures no
/// wake timeout of its own.
const DEFAULT_WAKE_TIMEOUT_SECONDS: i32 = 30;

/// Who decides that Temps may attach to the build a plugin's service runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BuildApproval<'a> {
    /// Nobody is asking: only the approved build is attached to, and any
    /// other is held for an administrator.
    Pinned,
    /// An administrator approved the update with this review. A build or
    /// manifest that is not the one reviewed is held like any other.
    Approved(&'a str),
}

/// What a running service plugin is attached to.
pub(super) struct AttachedService {
    pub(super) binding: ServiceBinding,
    pub(super) deployment_id: i32,
    /// Distinguishes this attachment from an earlier one to the same
    /// plugin, so a channel that closes late detaches nothing current.
    generation: u64,
}

/// Whether a plugin that runs as a service can serve a request right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServicePluginState {
    /// Temps is attached to the service's running container.
    Running,
    /// The service is stopped because it was idle. The next request to the
    /// plugin starts it again.
    Asleep,
    /// The service runs a build no administrator approved. Temps is not
    /// attached to it and attaches only once the update is approved, or the
    /// approved build runs again.
    UpdateWaiting,
    /// An administrator switched the plugin off. Temps is cut off from it
    /// until it is enabled again.
    Disabled,
    /// Temps is not attached, for the reason given with it.
    Unavailable,
}

/// One plugin that runs as a service, and how it is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePluginStatus {
    pub name: String,
    /// The service and environment it runs as; `None` once either was
    /// deleted.
    pub binding: Option<ServiceBinding>,
    /// The deployment Temps is attached to, while it is running.
    pub deployment_id: Option<i32>,
    pub state: ServicePluginState,
    /// Why it is not running, in words an administrator can be shown.
    pub detail: Option<String>,
}

/// A service plugin Temps is not attached to.
struct DormantService {
    binding: Option<ServiceBinding>,
    /// The manifest it last answered with, which keeps its routes, navigation
    /// and event subscriptions in place while it is stopped.
    manifest: Option<PluginManifest>,
    asleep: bool,
    /// The service runs a build that waits for an administrator.
    update_waiting: bool,
    /// An administrator switched the plugin off: nothing attaches to it.
    disabled: bool,
    reason: String,
    /// Until when requests fail, with what they are told, instead of trying
    /// again.
    retry_after: Option<(Instant, String)>,
}

/// The state of every plugin that runs as a service.
#[derive(Default)]
pub(super) struct ServicePlugins {
    runtime: RwLock<Option<ServiceRuntime>>,
    dormant: RwLock<HashMap<String, DormantService>>,
    /// Serialises attaching, so one request starts an idle plugin and the
    /// others find it running.
    attach: tokio::sync::Mutex<()>,
    generation: AtomicU64,
    /// The manager itself, for the tasks that outlive a call: watching a
    /// channel, and resolving a plugin's address per request.
    this: OnceLock<Weak<ExternalPluginManager>>,
    closing: AtomicBool,
    /// Signalled when a plugin was attached, detached or put to sleep without
    /// an administrator asking, so routes and subscriptions can be rebuilt.
    changed: Arc<Notify>,
}

impl ServicePlugins {
    pub(super) async fn dormant_manifests(&self) -> Vec<PluginManifest> {
        self.dormant
            .read()
            .await
            .values()
            .filter_map(|dormant| dormant.manifest.clone())
            .collect()
    }

    pub(super) async fn forget(&self, plugin_name: &str) {
        self.dormant.write().await.remove(plugin_name);
    }

    pub(super) async fn forget_all(&self) {
        self.dormant.write().await.clear();
    }
}

impl ExternalPluginManager {
    /// Share the manager, which lets it stay attached to service plugins on
    /// its own: a manager that is not shared attaches when asked and never
    /// reconnects.
    pub fn shared(self) -> Arc<Self> {
        let shared = Arc::new(self);
        let _ = shared.services.this.set(Arc::downgrade(&shared));
        shared
    }

    /// Give the manager what attaching to services needs. Until this is
    /// called, no plugin can run as a service.
    pub async fn set_service_runtime(&self, runtime: ServiceRuntime) {
        *self.services.runtime.write().await = Some(runtime);
    }

    /// Whether this process can run plugins as services at all.
    pub async fn can_run_services(&self) -> bool {
        self.services.runtime.read().await.is_some()
    }

    /// Signalled whenever a service plugin was attached, detached or put to
    /// sleep by the manager itself.
    pub fn service_plugins_changed(&self) -> Arc<Notify> {
        self.services.changed.clone()
    }

    /// Stop reconnecting: the process is shutting down.
    pub fn stop_supervising_services(&self) {
        self.services.closing.store(true, Ordering::SeqCst);
    }

    fn closing(&self) -> bool {
        self.services.closing.load(Ordering::SeqCst)
    }

    /// Attach to the service `binding` names and make it a running plugin.
    ///
    /// `expected_name` is the plugin's name when it is already known: a
    /// service that starts declaring another plugin is refused, because its
    /// grants were given to the name an administrator approved.
    pub async fn attach_service(
        &self,
        binding: ServiceBinding,
        expected_name: Option<&str>,
    ) -> Result<PluginManifest, ServiceAttachError> {
        let _attaching = self.services.attach.lock().await;
        self.attach_service_locked(binding, expected_name, BuildApproval::Pinned)
            .await
    }

    /// Attach to the update of `plugin_name` an administrator reviewed and
    /// approved. `review` names the build and manifest they saw; anything
    /// else the service runs by now is held for another review.
    pub async fn approve_service_update(
        &self,
        plugin_name: &str,
        review: &str,
    ) -> Result<PluginManifest, ServiceAttachError> {
        let _attaching = self.services.attach.lock().await;
        let binding = match self.plugins.read().await.get(plugin_name) {
            Some(process) => Some(process.service.binding),
            None => self
                .services
                .dormant
                .read()
                .await
                .get(plugin_name)
                .and_then(|dormant| dormant.binding),
        };
        let Some(binding) = binding else {
            return Err(ServiceAttachError::Record {
                plugin: plugin_name.to_string(),
                reason: "it names no service any more; remove the plugin and add it again"
                    .to_string(),
            });
        };
        match self
            .attach_service_locked(binding, Some(plugin_name), BuildApproval::Approved(review))
            .await
        {
            Ok(manifest) => {
                self.services.changed.notify_one();
                Ok(manifest)
            }
            Err(error) => {
                // A plugin that still runs its approved build keeps running.
                if !self.plugins.read().await.contains_key(plugin_name) {
                    self.mark_dormant(plugin_name, binding, &error, None).await;
                    self.services.changed.notify_one();
                }
                Err(error)
            }
        }
    }

    /// Hold `update` unless it is the build this plugin is pinned to, asking
    /// for nothing more, or the one an administrator just approved. Returns
    /// whether the plugin is being added: it has no record yet.
    async fn hold_unapproved(
        &self,
        service: &str,
        update: PendingUpdate,
        approval: BuildApproval<'_>,
    ) -> Result<bool, ServiceAttachError> {
        let plugin = update.manifest.name.clone();
        let record = |error: crate::updates::UpdateError| {
            error!(plugin = %plugin, %error, "Could not check a plugin's approved build");
            ServiceAttachError::Record {
                plugin: plugin.clone(),
                reason: "its approved build could not be read or recorded; see the server log"
                    .to_string(),
            }
        };
        let builds = PluginBuilds::new(self.db.clone());
        // A plugin that is being added has no row yet, and so no pin.
        let Some(pin) = builds.pin_of(&plugin).await.map_err(record)? else {
            return Ok(true);
        };
        if pin.allows(&update.build, &update.manifest)
            || approval == BuildApproval::Approved(update.review.as_str())
        {
            return Ok(false);
        }
        let deployment_id = update.deployment_id;
        let difference = AccessDifference::between(pin.approved.as_ref(), &update.manifest);
        if builds.hold(&plugin, &update).await.map_err(record)? {
            warn!(
                plugin = %plugin,
                approved_deployment_id = ?pin.deployment_id,
                deployment_id,
                asks_for_more = difference.asks_for_more,
                "A plugin's service runs a build that was not approved; holding the update"
            );
            self.record_held_update(&plugin, pin.deployment_id, deployment_id, difference)
                .await;
        }
        Err(ServiceAttachError::UpdateWaiting {
            plugin,
            service: service.to_string(),
            deployment_id,
        })
    }

    /// Put on record that Temps held an update without anyone asking.
    async fn record_held_update(
        &self,
        plugin: &str,
        approved_deployment_id: Option<i32>,
        deployment_id: i32,
        difference: AccessDifference,
    ) {
        let Some(audit) = self.audit_service.read().await.clone() else {
            return;
        };
        let entry = HeldUpdateAudit {
            plugin: plugin.to_string(),
            approved_deployment_id,
            deployment_id,
            difference,
        };
        // The update is held whether or not the entry is written.
        if let Err(error) = audit.create_audit_log(&entry).await {
            warn!(plugin = %plugin, %error, "Could not record that a plugin update is held");
        }
    }

    async fn attach_service_locked(
        &self,
        binding: ServiceBinding,
        expected_name: Option<&str>,
        approval: BuildApproval<'_>,
    ) -> Result<PluginManifest, ServiceAttachError> {
        let runtime = self
            .services
            .runtime
            .read()
            .await
            .clone()
            .ok_or(ServiceAttachError::Unavailable)?;
        let target = service_runtime::resolve_target(&self.db, &runtime.runtime, binding).await?;
        let token = runtime
            .tokens
            .deployment_token(binding.project_id, binding.environment_id)
            .await
            .map_err(|reason| ServiceAttachError::Token {
                service: target.service.clone(),
                reason,
            })?;
        let auth_secret = service_auth_secret(&token);
        let hello = service_runtime::hello(&target.service, &target.endpoint, &auth_secret).await?;
        let manifest = *hello.manifest;
        if let Some(expected) = expected_name.filter(|expected| *expected != manifest.name) {
            return Err(ServiceAttachError::NameChanged {
                service: target.service,
                expected: expected.to_string(),
                found: manifest.name,
            });
        }
        self.ensure_name_is_free(&manifest.name, binding).await?;
        if self.is_disabled(&manifest.name).await {
            return Err(ServiceAttachError::Disabled {
                plugin: manifest.name,
            });
        }
        // Before the plugin is given a channel: a build nobody approved gets
        // none.
        let review = crate::updates::review_id(&target.build, &manifest).map_err(|error| {
            ServiceAttachError::InvalidHello {
                service: target.service.clone(),
                reason: format!("its manifest cannot be stored for review: {error}"),
            }
        })?;
        let being_added = self
            .hold_unapproved(
                &target.service,
                PendingUpdate {
                    build: target.build.clone(),
                    deployment_id: target.deployment_id,
                    manifest: manifest.clone(),
                    review,
                    since: None,
                },
                approval,
            )
            .await?;

        let openapi_schema = hello.openapi.and_then(|json| {
            serde_json::from_value::<OpenApi>(json)
                .map_err(|error| {
                    warn!(plugin = %manifest.name, "Failed to parse OpenAPI schema: {error}");
                })
                .ok()
        });
        let channel = PluginChannel::connect(
            &target.endpoint,
            manifest.name.clone(),
            self.db.clone(),
            self.host_api.clone(),
            manifest.capabilities.clone(),
            manifest.host_permissions.clone(),
            manifest.published_events.clone(),
            self.watch_ready.clone(),
            target.build.clone(),
            binding.source_identity(),
            false,
            self.ai_service.clone(),
            self.audit_service.clone(),
            &auth_secret,
        )
        .await;
        let Some(channel) = channel else {
            // Opening the channel recorded the plugin's actor. A plugin that
            // was being added and never connected must not keep its name:
            // nothing lists it, so nothing could remove it.
            if being_added {
                if let Err(error) = PluginGrantService::new(self.db.clone())
                    .revoke_actor(&manifest.name)
                    .await
                {
                    error!(plugin = %manifest.name, %error, "Could not release the name of a plugin that failed to connect");
                }
            }
            return Err(ServiceAttachError::ChannelRefused {
                plugin: manifest.name.clone(),
            });
        };
        PluginGrantService::new(self.db.clone())
            .bind_service(
                &manifest.name,
                binding,
                &manifest,
                &target.build,
                target.deployment_id,
            )
            .await
            .map_err(|error| {
                error!(plugin = %manifest.name, %error, "Could not record a plugin's service");
                ServiceAttachError::Record {
                    plugin: manifest.name.clone(),
                    reason: "its service could not be saved; see the server log".to_string(),
                }
            })?;

        let generation = self.services.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let closed = channel.closed();
        let name = manifest.name.clone();
        let process = ExternalPluginProcess {
            manifest: manifest.clone(),
            build: target.build,
            endpoint: target.endpoint,
            auth_secret,
            service: AttachedService {
                binding,
                deployment_id: target.deployment_id,
                generation,
            },
            has_ui: hello.has_ui,
            channel: Some(channel),
            openapi_schema,
        };
        // Replacing an earlier attachment drops its channel; its watcher then
        // finds a newer generation and does nothing.
        let previous = self.plugins.write().await.insert(name.clone(), process);
        drop(previous);
        self.services.dormant.write().await.remove(&name);
        self.start_failures.write().await.remove(&name);
        info!(
            plugin = %name,
            version = %manifest.version,
            project_id = binding.project_id,
            environment_id = binding.environment_id,
            deployment_id = target.deployment_id,
            "Attached to the service a plugin runs as"
        );
        self.supervise(name, generation, closed);
        Ok(manifest)
    }

    /// A plugin name belongs to whatever was installed under it first. A
    /// service never takes over the name, and with it the grants, of a plugin
    /// installed from somewhere else.
    async fn ensure_name_is_free(
        &self,
        name: &str,
        binding: ServiceBinding,
    ) -> Result<(), ServiceAttachError> {
        let taken = || ServiceAttachError::NameTaken {
            name: name.to_string(),
        };
        match self.plugins.read().await.get(name).map(|p| &p.service) {
            None => {}
            Some(attached) if attached.binding == binding => {}
            Some(_) => return Err(taken()),
        }
        let source = PluginGrantService::new(self.db.clone())
            .active_source(name)
            .await
            .map_err(|error| {
                error!(plugin = %name, %error, "Could not read who owns a plugin name");
                ServiceAttachError::Record {
                    plugin: name.to_string(),
                    reason: "its current installation could not be read; see the server log"
                        .to_string(),
                }
            })?;
        match source {
            Some(source) if source != binding.source_identity() => Err(taken()),
            _ => Ok(()),
        }
    }

    /// Watch a plugin's channel and attach again when it closes.
    fn supervise(
        &self,
        name: String,
        generation: u64,
        mut closed: tokio::sync::watch::Receiver<bool>,
    ) {
        let Some(this) = self.services.this.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            // An error means the channel is gone without saying so.
            let _ = closed.wait_for(|closed| *closed).await;
            if let Some(manager) = this.upgrade() {
                manager.channel_lost(&name, generation).await;
            }
        });
    }

    /// The channel of attachment `generation` closed: the container stopped,
    /// because a deployment replaced it, it restarted, or its environment
    /// went idle. Attach to whatever runs now.
    async fn channel_lost(&self, name: &str, generation: u64) {
        if self.closing() {
            return;
        }
        let binding = {
            let _attaching = self.services.attach.lock().await;
            let mut plugins = self.plugins.write().await;
            let binding = match plugins.get(name).map(|process| &process.service) {
                Some(attached) if attached.generation == generation => attached.binding,
                _ => return,
            };
            let manifest = plugins.remove(name).map(|process| process.manifest.clone());
            drop(plugins);
            self.services.dormant.write().await.insert(
                name.to_string(),
                DormantService {
                    binding: Some(binding),
                    manifest,
                    asleep: false,
                    update_waiting: false,
                    disabled: false,
                    reason: format!(
                        "Plugin '{name}' lost its connection to Temps and is being reconnected."
                    ),
                    retry_after: None,
                },
            );
            binding
        };
        info!(plugin = %name, "A plugin's channel closed; attaching to its service again");

        for delay in REATTACH_DELAYS {
            tokio::time::sleep(delay).await;
            if self.closing() {
                return;
            }
            let _attaching = self.services.attach.lock().await;
            // A request may have attached it meanwhile, or an administrator
            // removed the plugin.
            if self.plugins.read().await.contains_key(name)
                || !self.services.dormant.read().await.contains_key(name)
            {
                return;
            }
            match self
                .attach_service_locked(binding, Some(name), BuildApproval::Pinned)
                .await
            {
                Ok(_) => {
                    self.services.changed.notify_one();
                    return;
                }
                Err(error) => {
                    let asleep = error.is_asleep();
                    let update_waiting = error.is_update_waiting();
                    self.mark_dormant(name, binding, &error, None).await;
                    if asleep {
                        info!(plugin = %name, "The plugin's service is stopped for being idle");
                        self.services.changed.notify_one();
                        return;
                    }
                    if update_waiting {
                        // Trying again changes nothing: an administrator
                        // approves it, or the approved build runs again and
                        // the next request attaches to it.
                        self.services.changed.notify_one();
                        return;
                    }
                    debug!(plugin = %name, %error, "Could not attach to a plugin's service yet");
                }
            }
        }
        warn!(
            plugin = %name,
            "Gave up reconnecting to a plugin's service; the next request to it tries again"
        );
        self.services.changed.notify_one();
    }

    /// Record why `name` is not attached, keeping the manifest it had.
    async fn mark_dormant(
        &self,
        name: &str,
        binding: ServiceBinding,
        error: &ServiceAttachError,
        retry_after: Option<Instant>,
    ) {
        let asleep = error.is_asleep();
        let reason = error.public_reason();
        let mut dormant = self.services.dormant.write().await;
        // A plugin that was switched off stays off, with that reason.
        if dormant.get(name).is_some_and(|previous| previous.disabled) {
            return;
        }
        let manifest = dormant.remove(name).and_then(|previous| previous.manifest);
        dormant.insert(
            name.to_string(),
            DormantService {
                binding: Some(binding),
                manifest,
                asleep,
                update_waiting: error.is_update_waiting(),
                disabled: false,
                reason: reason.clone(),
                retry_after: retry_after.map(|until| (until, reason.clone())),
            },
        );
        drop(dormant);
        let mut failures = self.start_failures.write().await;
        if asleep {
            failures.remove(name);
        } else {
            failures.insert(name.to_string(), reason);
        }
    }

    /// Attach to every plugin that is recorded as running as a service.
    pub(super) async fn start_service_plugins(&self) -> PluginReloadResult {
        let mut report = PluginReloadResult {
            manifests: Vec::new(),
            failures: Vec::new(),
        };
        // A process that deploys nothing has no service to attach to, and
        // none of its plugins can have been added from one.
        if !self.can_run_services().await {
            return report;
        }
        let stored = match PluginGrantService::new(self.db.clone())
            .service_plugins()
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                error!(%error, "Could not read the plugins that run as services");
                report.failures.push(PluginLoadFailure {
                    plugin: None,
                    reason: "The plugins that run as services could not be read from the database"
                        .to_string(),
                });
                return report;
            }
        };
        for plugin in stored {
            let Some(binding) = plugin.binding else {
                let reason = format!(
                    "The service plugin '{}' ran as was deleted. Remove the plugin, or add it again from another service.",
                    plugin.name
                );
                self.services.dormant.write().await.insert(
                    plugin.name.clone(),
                    DormantService {
                        binding: None,
                        manifest: None,
                        asleep: false,
                        update_waiting: false,
                        disabled: false,
                        reason: reason.clone(),
                        retry_after: None,
                    },
                );
                self.start_failures
                    .write()
                    .await
                    .insert(plugin.name.clone(), reason.clone());
                report.failures.push(PluginLoadFailure {
                    plugin: Some(plugin.name),
                    reason,
                });
                continue;
            };
            if plugin.disabled {
                // Listed, with its routes, and never attached to.
                self.services.dormant.write().await.insert(
                    plugin.name.clone(),
                    DormantService {
                        binding: Some(binding),
                        manifest: plugin.manifest.clone(),
                        asleep: false,
                        update_waiting: false,
                        disabled: true,
                        reason: disabled_reason(&plugin.name),
                        retry_after: None,
                    },
                );
                report.manifests.extend(plugin.manifest);
                continue;
            }
            let _attaching = self.services.attach.lock().await;
            match self
                .attach_service_locked(binding, Some(&plugin.name), BuildApproval::Pinned)
                .await
            {
                Ok(manifest) => report.manifests.push(manifest),
                Err(error) => {
                    // Seed the manifest it last answered with, so a plugin
                    // that is merely stopped keeps its routes.
                    self.services.dormant.write().await.insert(
                        plugin.name.clone(),
                        DormantService {
                            binding: Some(binding),
                            manifest: plugin.manifest.clone(),
                            asleep: false,
                            update_waiting: false,
                            disabled: false,
                            reason: String::new(),
                            retry_after: None,
                        },
                    );
                    self.mark_dormant(&plugin.name, binding, &error, None).await;
                    // A plugin that is stopped, or whose update waits, keeps
                    // the routes and navigation of its approved manifest.
                    match plugin
                        .manifest
                        .filter(|_| error.is_asleep() || error.is_update_waiting())
                    {
                        Some(manifest) => report.manifests.push(manifest),
                        None => {
                            warn!(plugin = %plugin.name, %error, "A plugin's service is not available");
                            report.failures.push(PluginLoadFailure {
                                plugin: Some(plugin.name),
                                reason: error.public_reason(),
                            });
                        }
                    }
                }
            }
        }
        report
    }

    /// The service a plugin runs as, whether or not Temps is attached to it.
    pub async fn service_binding_of(&self, plugin_name: &str) -> Option<ServiceBinding> {
        if let Some(process) = self.plugins.read().await.get(plugin_name) {
            return Some(process.service.binding);
        }
        self.services
            .dormant
            .read()
            .await
            .get(plugin_name)
            .and_then(|dormant| dormant.binding)
    }

    /// Whether `plugin_name` runs as a service, attached or not.
    pub async fn is_service_plugin(&self, plugin_name: &str) -> bool {
        self.plugins.read().await.contains_key(plugin_name)
            || self.services.dormant.read().await.contains_key(plugin_name)
    }

    /// Every plugin that runs as a service and how it is doing, by name.
    pub async fn service_plugin_statuses(&self) -> Vec<ServicePluginStatus> {
        let mut statuses: Vec<ServicePluginStatus> = self
            .plugins
            .read()
            .await
            .iter()
            .map(|(name, process)| ServicePluginStatus {
                name: name.clone(),
                binding: Some(process.service.binding),
                deployment_id: Some(process.service.deployment_id),
                state: ServicePluginState::Running,
                detail: None,
            })
            .collect();
        statuses.extend(
            self.services
                .dormant
                .read()
                .await
                .iter()
                .map(|(name, dormant)| ServicePluginStatus {
                    name: name.clone(),
                    binding: dormant.binding,
                    deployment_id: None,
                    state: if dormant.disabled {
                        ServicePluginState::Disabled
                    } else if dormant.asleep {
                        ServicePluginState::Asleep
                    } else if dormant.update_waiting {
                        ServicePluginState::UpdateWaiting
                    } else {
                        ServicePluginState::Unavailable
                    },
                    detail: Some(dormant.reason.clone()),
                }),
        );
        statuses.sort_by(|a, b| a.name.cmp(&b.name));
        statuses
    }

    /// Stop treating `plugin_name` as a plugin. Its service keeps running:
    /// the deployment owns it.
    #[cfg(test)]
    pub(crate) async fn detach_service(&self, plugin_name: &str) -> bool {
        let _attaching = self.services.attach.lock().await;
        self.forget_service(plugin_name).await
    }

    /// Run `revoke`, then stop treating `plugin_name` as a plugin, with no
    /// attachment in between. A request that was starting the plugin again
    /// has finished by the time `revoke` runs, and none can start until the
    /// plugin is forgotten, so a revoked actor is never created again behind
    /// a removal. When `revoke` fails the plugin stays attached and listed.
    pub(crate) async fn detach_service_after<E>(
        &self,
        plugin_name: &str,
        revoke: impl std::future::Future<Output = Result<(), E>>,
    ) -> Result<bool, E> {
        let _attaching = self.services.attach.lock().await;
        revoke.await?;
        Ok(self.forget_service(plugin_name).await)
    }

    async fn forget_service(&self, plugin_name: &str) -> bool {
        let was_dormant = self
            .services
            .dormant
            .write()
            .await
            .remove(plugin_name)
            .is_some();
        let attached = self.plugins.write().await.remove(plugin_name).is_some();
        self.start_failures.write().await.remove(plugin_name);
        attached || was_dormant
    }

    /// Whether an administrator switched `plugin_name` off.
    pub async fn is_disabled(&self, plugin_name: &str) -> bool {
        self.services
            .dormant
            .read()
            .await
            .get(plugin_name)
            .is_some_and(|dormant| dormant.disabled)
    }

    /// Cut `plugin_name` off from Temps at once: close its channel and
    /// refuse every later attempt to attach, until it is enabled. It stays
    /// listed, with its routes, so that what was switched off says so. The
    /// service keeps running: the deployment owns it. Returns whether such a
    /// plugin exists.
    pub(crate) async fn disable_service(&self, plugin_name: &str) -> bool {
        let _attaching = self.services.attach.lock().await;
        // Dropping the process closes its channel; its watcher finds the
        // plugin gone and does nothing.
        let attached = self.plugins.write().await.remove(plugin_name);
        let mut dormant = self.services.dormant.write().await;
        let (binding, manifest) = match attached {
            Some(process) => (
                Some(process.service.binding),
                Some(process.manifest.clone()),
            ),
            None => match dormant.remove(plugin_name) {
                Some(previous) => (previous.binding, previous.manifest),
                None => return false,
            },
        };
        dormant.insert(
            plugin_name.to_string(),
            DormantService {
                binding,
                manifest,
                asleep: false,
                update_waiting: false,
                disabled: true,
                reason: disabled_reason(plugin_name),
                retry_after: None,
            },
        );
        drop(dormant);
        self.start_failures.write().await.remove(plugin_name);
        true
    }

    /// Let `plugin_name` connect again, and connect to it if its approved
    /// build runs. A service that is stopped for being idle is not started:
    /// the next request to the plugin does that. Returns whether such a
    /// plugin exists.
    pub(crate) async fn enable_service(&self, plugin_name: &str) -> bool {
        let _attaching = self.services.attach.lock().await;
        let binding = {
            let mut dormant = self.services.dormant.write().await;
            let Some(entry) = dormant.get_mut(plugin_name) else {
                return self.plugins.read().await.contains_key(plugin_name);
            };
            entry.disabled = false;
            entry.retry_after = None;
            entry.reason = format!("Plugin '{plugin_name}' was enabled and is being connected.");
            entry.binding
        };
        let Some(binding) = binding else {
            return true;
        };
        if let Err(error) = self
            .attach_service_locked(binding, Some(plugin_name), BuildApproval::Pinned)
            .await
        {
            self.mark_dormant(plugin_name, binding, &error, None).await;
        }
        true
    }

    /// The proxy of a plugin that runs as a service: it looks the address up
    /// for every request, so it stays valid across deployments and idle
    /// stops. `None` for other plugins, and for a manager that is not shared.
    pub(super) async fn service_proxy_for(&self, plugin_name: &str) -> Option<PluginProxy> {
        let this = self.services.this.get()?.clone();
        let manifest = self
            .plugins
            .read()
            .await
            .get(plugin_name)
            .map(|process| process.manifest.clone());
        let manifest = match manifest {
            Some(manifest) => manifest,
            None => self
                .services
                .dormant
                .read()
                .await
                .get(plugin_name)?
                .manifest
                .clone()?,
        };
        Some(
            PluginProxy::for_service(ServiceRoute::new(this), manifest.name.clone())
                .with_public_paths(manifest.public_paths.clone())
                .with_actor_minting(self.actor_crypto.clone(), !manifest.capabilities.is_empty()),
        )
    }

    /// Where to send one request for `plugin_name`, starting its service
    /// again when it was stopped for being idle. The error is what the
    /// caller is told.
    pub(crate) async fn service_request_target(
        &self,
        plugin_name: &str,
    ) -> Result<ServiceTarget, String> {
        if let Some(target) = self.attached_target(plugin_name).await {
            return Ok(target);
        }
        self.revive_service(plugin_name).await?;
        self.attached_target(plugin_name)
            .await
            .ok_or_else(|| format!("Plugin '{plugin_name}' stopped while it was being started."))
    }

    async fn attached_target(&self, plugin_name: &str) -> Option<ServiceTarget> {
        let (binding, target) = {
            let plugins = self.plugins.read().await;
            let process = plugins.get(plugin_name)?;
            (
                process.service.binding,
                ServiceTarget {
                    endpoint: process.endpoint.clone(),
                    auth_secret: process.auth_secret.clone(),
                    public_paths: process.manifest.public_paths.clone(),
                },
            )
        };
        // A request that reaches the plugin through Temps is use of the
        // service just like one through the proxy: keep it from being
        // stopped for being idle.
        if let Some(waker) = self
            .services
            .runtime
            .read()
            .await
            .as_ref()
            .and_then(|runtime| runtime.waker.clone())
        {
            waker.record_activity(binding.environment_id);
        }
        Some(target)
    }

    /// Whether a plugin Temps is not attached to should be started to
    /// receive an event: it runs as a service, the manifest it last answered
    /// with asks for events, and an administrator granted them. A plugin that
    /// may not receive events is never started for one.
    pub(crate) async fn stopped_service_takes_events(&self, plugin_name: &str) -> bool {
        use temps_plugin_protocol::channel::PluginHostPermission;
        let asks = self
            .services
            .dormant
            .read()
            .await
            .get(plugin_name)
            .filter(|dormant| !dormant.disabled)
            .and_then(|dormant| dormant.manifest.as_ref())
            .is_some_and(|manifest| {
                manifest
                    .host_permissions
                    .contains(&PluginHostPermission::EventsRead)
            });
        asks && PluginGrantService::new(self.db.clone())
            .get(plugin_name)
            .await
            .is_ok_and(|grants| {
                grants
                    .config
                    .permissions
                    .contains(&PluginHostPermission::EventsRead)
            })
    }

    /// Let the next request try a plugin again at once, as if the pause
    /// after a failed start had passed.
    #[cfg(test)]
    pub(crate) async fn end_retry_pause(&self, plugin_name: &str) {
        if let Some(entry) = self.services.dormant.write().await.get_mut(plugin_name) {
            entry.retry_after = None;
        }
    }

    /// Attach to a plugin Temps is not attached to, starting its environment
    /// when it is stopped for being idle.
    pub(crate) async fn revive_service(&self, plugin_name: &str) -> Result<(), String> {
        let _attaching = self.services.attach.lock().await;
        if self.plugins.read().await.contains_key(plugin_name) {
            return Ok(());
        }
        let (binding, waiting) = {
            let dormant = self.services.dormant.read().await;
            let Some(entry) = dormant.get(plugin_name) else {
                return Err(format!("Plugin '{plugin_name}' is not installed."));
            };
            let Some(binding) = entry.binding else {
                return Err(entry.reason.clone());
            };
            if entry.disabled {
                return Err(entry.reason.clone());
            }
            let waiting = entry
                .retry_after
                .as_ref()
                .filter(|(retry_after, _)| Instant::now() < *retry_after)
                .map(|(_, reason)| reason.clone());
            (binding, waiting)
        };
        if let Some(reason) = waiting {
            return Err(reason);
        }

        let mut outcome = self
            .attach_service_locked(binding, Some(plugin_name), BuildApproval::Pinned)
            .await;
        if outcome.as_ref().is_err_and(ServiceAttachError::is_asleep) {
            if let Err(reason) = self.wake(plugin_name, binding).await {
                // A start that just failed is not tried again for every
                // request and event that arrives meanwhile.
                if let Some(entry) = self.services.dormant.write().await.get_mut(plugin_name) {
                    entry.retry_after = Some((Instant::now() + RETRY_COOLDOWN, reason.clone()));
                }
                return Err(reason);
            }
            outcome = self
                .attach_service_locked(binding, Some(plugin_name), BuildApproval::Pinned)
                .await;
        }
        match outcome {
            Ok(_) => {
                self.services.changed.notify_one();
                Ok(())
            }
            Err(error) => {
                let retry_after = (!error.is_asleep()).then(|| Instant::now() + RETRY_COOLDOWN);
                self.mark_dormant(plugin_name, binding, &error, retry_after)
                    .await;
                Err(error.public_reason())
            }
        }
    }

    /// Start the environment of an idle plugin and wait until it is healthy.
    async fn wake(&self, plugin_name: &str, binding: ServiceBinding) -> Result<(), String> {
        let waker = self
            .services
            .runtime
            .read()
            .await
            .as_ref()
            .and_then(|runtime| runtime.waker.clone());
        let Some(waker) = waker else {
            return Err(format!(
                "Plugin '{plugin_name}' is stopped because its service was idle, and this Temps process does not manage idle services. Start the environment from the service's page."
            ));
        };
        let timeout = match temps_entities::environments::Entity::find_by_id(binding.environment_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(environment)) => environment
                .deployment_config
                .map(|config| config.wake_timeout_seconds)
                .filter(|seconds| *seconds > 0)
                .unwrap_or(DEFAULT_WAKE_TIMEOUT_SECONDS),
            _ => DEFAULT_WAKE_TIMEOUT_SECONDS,
        };
        info!(plugin = %plugin_name, environment_id = binding.environment_id, "Starting an idle plugin's service");
        waker
            .wake_environment(binding.environment_id, timeout)
            .await
            .map_err(|error| {
                warn!(plugin = %plugin_name, %error, "An idle plugin's service did not start");
                format!(
                    "Plugin '{plugin_name}' was stopped for being idle and its service did not start within {timeout} seconds. Check the service's deployment."
                )
            })
    }
}

/// What a request to a disabled plugin, and its row in the list, are told.
fn disabled_reason(plugin_name: &str) -> String {
    ServiceAttachError::Disabled {
        plugin: plugin_name.to_string(),
    }
    .public_reason()
}

/// Temps held an update of a plugin on its own: nobody asked, so the entry
/// names the plugin and no user.
#[derive(Serialize)]
struct HeldUpdateAudit {
    plugin: String,
    /// The deployment the plugin is pinned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_deployment_id: Option<i32>,
    /// The deployment that is held.
    deployment_id: i32,
    difference: AccessDifference,
}

impl temps_core::audit::AuditOperation for HeldUpdateAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGIN_UPDATE_HELD".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        None
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "external-plugin-host"
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[cfg(test)]
pub(crate) mod tests;
