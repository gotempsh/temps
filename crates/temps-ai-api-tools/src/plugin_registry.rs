// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{InternalApiCaller, PluginToolAccess, PluginToolsIndex};
use arc_swap::ArcSwap;
use std::sync::{Arc, Mutex, Weak};
use temps_core::secret_string::SecretString;

type Subscriber = Box<dyn Fn(Arc<PluginToolsIndex>) + Send + Sync>;

/// Control-plane publication is serialized; execution only reads ArcSwap.
pub struct PluginToolsRegistry {
    current: ArcSwap<PluginToolsIndex>,
    subscribers: Mutex<Vec<Subscriber>>,
}

impl Default for PluginToolsRegistry {
    fn default() -> Self {
        Self {
            current: ArcSwap::from_pointee(PluginToolsIndex::default()),
            subscribers: Mutex::new(Vec::new()),
        }
    }
}

impl PluginToolsRegistry {
    pub fn snapshot(&self) -> Arc<PluginToolsIndex> {
        self.current.load_full()
    }

    pub fn publish(&self, mut index: PluginToolsIndex) {
        index.revision = uuid::Uuid::new_v4();
        let subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let snapshot = Arc::new(index);
        self.current.store(snapshot.clone());
        for subscriber in subscribers.iter() {
            subscriber(snapshot.clone());
        }
    }

    /// Rebuild complete callers only on publication, preserving curated core indexes.
    pub fn bind(
        self: &Arc<Self>,
        core: InternalApiCaller,
        access: PluginToolAccess,
        audit: Arc<dyn temps_core::AuditLogger>,
        publish: impl Fn(InternalApiCaller) + Send + Sync + 'static,
    ) {
        let registry = Arc::downgrade(self);
        let subscriber: Subscriber = Box::new(move |snapshot| {
            publish(core.clone().with_plugin_snapshot(PluginBinding {
                registry: registry.clone(),
                snapshot,
                access,
                audit: audit.clone(),
            }));
        });
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        subscriber(self.snapshot());
        subscribers.push(subscriber);
    }
}

#[derive(Clone)]
pub(crate) struct PluginBinding {
    pub registry: Weak<PluginToolsRegistry>,
    pub snapshot: Arc<PluginToolsIndex>,
    pub access: PluginToolAccess,
    pub audit: Arc<dyn temps_core::AuditLogger>,
}

#[derive(serde::Serialize)]
pub(crate) struct PluginWriteAudit<'a> {
    pub plugin: &'a str,
    pub operation_id: &'a str,
    pub user_id: Option<i32>,
    /// None records an attempt before dispatch; cancellation may leave outcome unknown.
    pub succeeded: Option<bool>,
}

impl temps_core::audit::AuditOperation for PluginWriteAudit<'_> {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGIN_AI_WRITE".into()
    }
    fn user_id(&self) -> Option<i32> {
        self.user_id
    }
    fn ip_address(&self) -> Option<String> {
        None
    }
    fn user_agent(&self) -> &str {
        "Temps AI assistant"
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// In-process capability carried to the proxy, never accepted from HTTP headers.
/// The proxy validates after connecting so dispatch is pinned to that instance.
#[derive(Clone)]
pub struct PluginDispatchGuard {
    pub(crate) binding: PluginBinding,
    pub(crate) operation_id: String,
    response_credentials: Arc<ArcSwap<Vec<SecretString>>>,
}

impl PluginDispatchGuard {
    pub(crate) fn new(binding: PluginBinding, operation_id: String) -> Self {
        Self {
            binding,
            operation_id,
            response_credentials: Arc::new(ArcSwap::from_pointee(Vec::new())),
        }
    }

    /// The proxy records only the two credentials it actually injected. This
    /// per-call slot is never serialized or logged and uses no application lock.
    pub fn record_response_credentials(&self, signature: Option<&str>, actor_token: Option<&str>) {
        self.response_credentials.store(Arc::new(
            [signature, actor_token]
                .into_iter()
                .flatten()
                .map(SecretString::new)
                .collect(),
        ));
    }

    pub(crate) fn scrub_response(&self, response: &mut serde_json::Value) {
        crate::secrets::scrub(response, &self.response_credentials.load());
    }

    pub fn is_current(&self) -> bool {
        self.binding.registry.upgrade().is_some_and(|registry| {
            Arc::ptr_eq(&registry.snapshot(), &self.binding.snapshot)
                && self
                    .binding
                    .snapshot
                    .callable(&self.operation_id, self.binding.access)
                    .is_some()
        })
    }
}
