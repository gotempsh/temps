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
use utoipa::ToSchema;

use super::*;
use crate::grants::PluginGrantService;
use crate::proxy::ServiceRoute;
use crate::service_runtime::{
    self, service_auth_secret, ServiceAttachError, ServiceBinding, ServiceRuntime,
};

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
        self.attach_service_locked(binding, expected_name).await
    }

    async fn attach_service_locked(
        &self,
        binding: ServiceBinding,
        expected_name: Option<&str>,
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
        .await
        .ok_or_else(|| ServiceAttachError::ChannelRefused {
            plugin: manifest.name.clone(),
        })?;
        PluginGrantService::new(self.db.clone())
            .bind_service(&manifest.name, binding, &manifest)
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
            binary_path: PathBuf::new(),
            sha256: target.build,
            endpoint: target.endpoint,
            auth_secret,
            pid_file_path: PathBuf::new(),
            runtime: PluginRuntime::Service(AttachedService {
                binding,
                deployment_id: target.deployment_id,
                generation,
            }),
            isolation: IsolationMode::Service,
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
        match self.plugins.read().await.get(name).map(|p| &p.runtime) {
            None => {}
            Some(PluginRuntime::Service(attached)) if attached.binding == binding => {}
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
            let binding = match plugins.get(name).map(|process| &process.runtime) {
                Some(PluginRuntime::Service(attached)) if attached.generation == generation => {
                    attached.binding
                }
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
            match self.attach_service_locked(binding, Some(name)).await {
                Ok(_) => {
                    self.services.changed.notify_one();
                    return;
                }
                Err(error) => {
                    let asleep = error.is_asleep();
                    self.mark_dormant(name, binding, &error, None).await;
                    if asleep {
                        info!(plugin = %name, "The plugin's service is stopped for being idle");
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
        let manifest = dormant.remove(name).and_then(|previous| previous.manifest);
        dormant.insert(
            name.to_string(),
            DormantService {
                binding: Some(binding),
                manifest,
                asleep,
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
            let _attaching = self.services.attach.lock().await;
            match self
                .attach_service_locked(binding, Some(&plugin.name))
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
                            reason: String::new(),
                            retry_after: None,
                        },
                    );
                    self.mark_dormant(&plugin.name, binding, &error, None).await;
                    match plugin.manifest.filter(|_| error.is_asleep()) {
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
        if let Some(PluginRuntime::Service(attached)) = self
            .plugins
            .read()
            .await
            .get(plugin_name)
            .map(|process| &process.runtime)
        {
            return Some(attached.binding);
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
        matches!(
            self.plugins
                .read()
                .await
                .get(plugin_name)
                .map(|process| &process.runtime),
            Some(PluginRuntime::Service(_))
        ) || self.services.dormant.read().await.contains_key(plugin_name)
    }

    /// Every plugin that runs as a service and how it is doing, by name.
    pub async fn service_plugin_statuses(&self) -> Vec<ServicePluginStatus> {
        let mut statuses: Vec<ServicePluginStatus> = self
            .plugins
            .read()
            .await
            .iter()
            .filter_map(|(name, process)| match &process.runtime {
                PluginRuntime::Service(attached) => Some(ServicePluginStatus {
                    name: name.clone(),
                    binding: Some(attached.binding),
                    deployment_id: Some(attached.deployment_id),
                    state: ServicePluginState::Running,
                    detail: None,
                }),
                _ => None,
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
                    state: if dormant.asleep {
                        ServicePluginState::Asleep
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
    pub(crate) async fn detach_service(&self, plugin_name: &str) -> bool {
        let _attaching = self.services.attach.lock().await;
        let was_dormant = self
            .services
            .dormant
            .write()
            .await
            .remove(plugin_name)
            .is_some();
        let mut plugins = self.plugins.write().await;
        let attached = matches!(
            plugins.get(plugin_name).map(|process| &process.runtime),
            Some(PluginRuntime::Service(_))
        );
        if attached {
            plugins.remove(plugin_name);
        }
        drop(plugins);
        self.start_failures.write().await.remove(plugin_name);
        attached || was_dormant
    }

    /// The proxy of a plugin that runs as a service: it looks the address up
    /// for every request, so it stays valid across deployments and idle
    /// stops. `None` for other plugins, and for a manager that is not shared.
    pub(super) async fn service_proxy_for(&self, plugin_name: &str) -> Option<PluginProxy> {
        let this = self.services.this.get()?.clone();
        let manifest = match self.plugins.read().await.get(plugin_name) {
            Some(process) if matches!(process.runtime, PluginRuntime::Service(_)) => {
                Some(process.manifest.clone())
            }
            Some(_) => return None,
            None => None,
        };
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
    ) -> Result<(PluginEndpoint, String), String> {
        if let Some(target) = self.attached_target(plugin_name).await {
            return Ok(target);
        }
        self.revive_service(plugin_name).await?;
        self.attached_target(plugin_name)
            .await
            .ok_or_else(|| format!("Plugin '{plugin_name}' stopped while it was being started."))
    }

    async fn attached_target(&self, plugin_name: &str) -> Option<(PluginEndpoint, String)> {
        let (binding, target) = {
            let plugins = self.plugins.read().await;
            let process = plugins.get(plugin_name)?;
            let PluginRuntime::Service(attached) = &process.runtime else {
                return None;
            };
            (
                attached.binding,
                (process.endpoint.clone(), process.auth_secret.clone()),
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

        let mut outcome = self.attach_service_locked(binding, Some(plugin_name)).await;
        if outcome.as_ref().is_err_and(ServiceAttachError::is_asleep) {
            if let Err(reason) = self.wake(plugin_name, binding).await {
                // A start that just failed is not tried again for every
                // request and event that arrives meanwhile.
                if let Some(entry) = self.services.dormant.write().await.get_mut(plugin_name) {
                    entry.retry_after = Some((Instant::now() + RETRY_COOLDOWN, reason.clone()));
                }
                return Err(reason);
            }
            outcome = self.attach_service_locked(binding, Some(plugin_name)).await;
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

#[cfg(test)]
pub(crate) mod tests;
