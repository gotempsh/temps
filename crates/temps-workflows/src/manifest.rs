// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflow manifest of each environment, read from its executor and
//! cached in memory, never stored (ADR-106 §3).
//!
//! An entry is keyed on the environment's current deployment, so a new
//! deployment is read afresh; it otherwise lives [`MANIFEST_TTL`]. The cache
//! holds at most [`MAX_ENTRIES`] environments, evicting the oldest. Failed
//! reads are cached briefly too ([`FAILURE_TTL`]), so a service without the
//! SDK is not asked again on every claim.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use temps_core::ExecutorEndpointSet;

use crate::executor::ExecutorClient;
use crate::protocol::Manifest;
use crate::store::WorkflowLimit;
use crate::token::InvocationSigner;

pub const MANIFEST_TTL: Duration = Duration::from_secs(600);
pub const FAILURE_TTL: Duration = Duration::from_secs(30);
pub const MAX_ENTRIES: usize = 4_096;

#[derive(Clone)]
struct Entry {
    project_id: i32,
    deployment_id: Option<i32>,
    result: Result<Arc<Manifest>, String>,
    fetched_at: Instant,
}

impl Entry {
    fn fresh(&self, deployment_id: Option<i32>, now: Instant) -> bool {
        let ttl = if self.result.is_ok() {
            MANIFEST_TTL
        } else {
            FAILURE_TTL
        };
        self.deployment_id == deployment_id && now.duration_since(self.fetched_at) < ttl
    }
}

#[derive(Default)]
pub struct ManifestCache {
    entries: RwLock<HashMap<i32, Entry>>,
}

impl ManifestCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<i32, Entry>> {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<i32, Entry>> {
        self.entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The environment's manifest, read from one of `set`'s replicas when the
    /// cached one is missing, stale or from another deployment. `Err` carries
    /// the reason in words a user can act on.
    pub async fn get(
        &self,
        client: &ExecutorClient,
        signer: &InvocationSigner,
        project_id: i32,
        environment_id: i32,
        set: &ExecutorEndpointSet,
    ) -> Result<Arc<Manifest>, String> {
        let now = Instant::now();
        // A local developer can hot-reload without a deployment ID changing.
        // Read its bounded persisted manifest on each invocation, including on
        // control planes other than the one that accepted the update.
        let development = set
            .endpoints
            .iter()
            .any(|endpoint| endpoint.base_url.starts_with("dev://"));
        if let Some(entry) = self.read().get(&environment_id) {
            if !development && entry.fresh(set.deployment_id, now) {
                return entry.result.clone();
            }
        }
        let Some(endpoint) = set.endpoints.first() else {
            return Err(set
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "No replica of the service is running".to_string()));
        };
        let result = client
            .manifest(signer, project_id, environment_id, endpoint)
            .await
            .map(Arc::new)
            .map_err(|error| format!("Reading the workflow manifest failed: {error}"));
        if let Err(reason) = &result {
            tracing::warn!(project_id, environment_id, %reason, "workflow manifest unavailable");
        }
        self.insert(
            environment_id,
            Entry {
                project_id,
                deployment_id: set.deployment_id,
                result: result.clone(),
                fetched_at: Instant::now(),
            },
        );
        result
    }

    fn insert(&self, environment_id: i32, entry: Entry) {
        let mut entries = self.write();
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&environment_id) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(id, _)| *id)
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(environment_id, entry);
    }

    /// The cached manifest of an environment without fetching, for display.
    pub fn cached(&self, environment_id: i32) -> Option<Result<Arc<Manifest>, String>> {
        self.read()
            .get(&environment_id)
            .map(|entry| entry.result.clone())
    }

    pub fn invalidate(&self, environment_id: i32) {
        self.write().remove(&environment_id);
    }

    /// Per-workflow concurrency limits of every cached manifest, for claims.
    pub fn limits(&self) -> Vec<WorkflowLimit> {
        self.read()
            .iter()
            .filter_map(|(environment_id, entry)| {
                let manifest = entry.result.as_ref().ok()?;
                Some((*environment_id, manifest.clone()))
            })
            .flat_map(|(environment_id, manifest)| {
                manifest
                    .workflows
                    .iter()
                    .filter_map(|workflow| {
                        workflow.concurrency.map(|limit| WorkflowLimit {
                            environment_id,
                            workflow_name: workflow.name.clone(),
                            limit,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Projects with a cached manifest (diagnostics).
    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    #[cfg(test)]
    pub(crate) fn put(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: Option<i32>,
        manifest: Manifest,
    ) {
        self.insert(
            environment_id,
            Entry {
                project_id,
                deployment_id,
                result: Ok(Arc::new(manifest)),
                fetched_at: Instant::now(),
            },
        );
    }

    /// The project of a cached environment.
    pub fn project_of(&self, environment_id: i32) -> Option<i32> {
        self.read()
            .get(&environment_id)
            .map(|entry| entry.project_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ManifestWorkflow;
    use temps_core::{EncryptionService, ExecutorEndpoint};

    fn manifest(concurrency: Option<u32>) -> Manifest {
        Manifest {
            protocol_version: 1,
            sdk: None,
            workflows: vec![ManifestWorkflow {
                name: "orders".into(),
                version: None,
                concurrency,
            }],
        }
    }

    #[test]
    fn limits_come_from_cached_manifests_only_where_declared() {
        let cache = ManifestCache::new();
        cache.put(1, 10, Some(1), manifest(Some(3)));
        cache.put(1, 11, Some(2), manifest(None));
        assert_eq!(
            cache.limits(),
            vec![WorkflowLimit {
                environment_id: 10,
                workflow_name: "orders".into(),
                limit: 3
            }]
        );
        cache.invalidate(10);
        assert!(cache.limits().is_empty());
        assert_eq!(cache.project_of(11), Some(1));
    }

    #[test]
    fn the_cache_is_bounded() {
        let cache = ManifestCache::new();
        for environment_id in 0..(MAX_ENTRIES as i32 + 5) {
            cache.put(1, environment_id, None, manifest(None));
        }
        assert_eq!(cache.len(), MAX_ENTRIES);
    }

    #[tokio::test]
    async fn a_new_deployment_rereads_and_a_failure_is_remembered_briefly() {
        let cache = ManifestCache::new();
        cache.put(1, 10, Some(1), manifest(None));
        let client = ExecutorClient::new().unwrap();
        let signer = InvocationSigner::new(
            &EncryptionService::new("12345678901234567890123456789012").unwrap(),
        );
        // Same deployment: served from the cache without any endpoint.
        let same = ExecutorEndpointSet {
            deployment_id: Some(1),
            endpoints: vec![],
            unavailable_reason: None,
        };
        assert!(cache.get(&client, &signer, 1, 10, &same).await.is_ok());

        // A new deployment whose only replica is unreachable.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let moved = ExecutorEndpointSet {
            deployment_id: Some(2),
            endpoints: vec![ExecutorEndpoint {
                base_url: format!("http://{address}"),
                container_id: 1,
                node_id: None,
            }],
            unavailable_reason: None,
        };
        let error = cache
            .get(&client, &signer, 1, 10, &moved)
            .await
            .unwrap_err();
        assert!(
            error.contains("Reading the workflow manifest failed"),
            "{error}"
        );
        assert!(matches!(cache.cached(10), Some(Err(_))));
    }
}
