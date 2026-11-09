// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated host control boundary. A worker boot has one sealing key and
//! pins one control-plane authority. Neither identity nor authority is accepted
//! from guest traffic. Restart generates a new identity and needs fresh consent.
use crate::{RuntimeError, SandboxBinding, WorkerProxy};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokenless_core::{
    ids::NodeId,
    seal::{SealPublicKey, SealSecretKey},
    snapshot::SignedSnapshot,
};
use tokenless_dataplane::{audit::AuditShipper, state::NodeKeys, BindingSpec, BindingUpdate};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Public material only. Read through the authenticated worker channel, never
/// accept a guest's asserted worker identity when authorizing a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentity {
    pub boot_id: Uuid,
    pub seal_public_key: [u8; 32],
}

/// Control-plane registration for exactly one worker boot. The caller must
/// authenticate the control plane before invoking `configure`. A different
/// signing authority cannot replace an initialized worker in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerAuthority {
    pub boot_id: Uuid,
    pub node_id: Uuid,
    pub snapshot_verify_key: [u8; 32],
}

struct Initialized {
    authority: WorkerAuthority,
    proxy: WorkerProxy,
}

/// Construct once per trusted worker process, not once per request. Keys stay
/// in host memory. The SDK starts lazily after authenticated registration; an
/// unconfigured worker cannot install policy or open a binding.
pub struct WorkerControl {
    identity: WorkerIdentity,
    seal_secret: SealSecretKey,
    initialized: OnceCell<Initialized>,
    audit: Arc<dyn AuditShipper>,
}

impl WorkerControl {
    pub fn new(audit: Arc<dyn AuditShipper>) -> Self {
        let seal_secret = SealSecretKey::random_from_rng(rand08::rngs::OsRng);
        Self {
            identity: WorkerIdentity {
                boot_id: Uuid::new_v4(),
                seal_public_key: SealPublicKey::from(&seal_secret).to_bytes(),
            },
            seal_secret,
            initialized: OnceCell::new(),
            audit,
        }
    }

    pub fn identity(&self) -> WorkerIdentity {
        self.identity.clone()
    }

    fn invalid(&self, operation: &'static str, reason: &'static str) -> RuntimeError {
        RuntimeError::Invalid {
            operation,
            binding: format!("worker:{}", self.identity.boot_id),
            reason,
        }
    }

    pub async fn configure(&self, authority: WorkerAuthority) -> Result<(), RuntimeError> {
        if authority.boot_id != self.identity.boot_id || authority.node_id.is_nil() {
            return Err(self.invalid(
                "configure authority",
                "authority must address this worker boot and a non-nil node",
            ));
        }
        let snapshot_key = VerifyingKey::from_bytes(&authority.snapshot_verify_key)
            .map_err(|_| self.invalid("configure authority", "invalid signing public key"))?;
        if snapshot_key.is_weak() {
            return Err(self.invalid("configure authority", "weak signing public key"));
        }
        let initialized = self
            .initialized
            .get_or_try_init(|| async {
                Ok::<_, RuntimeError>(Initialized {
                    proxy: WorkerProxy::start(
                        NodeKeys {
                            node_id: NodeId(authority.node_id),
                            seal_secret: self.seal_secret.clone(),
                            snapshot_key,
                        },
                        Arc::clone(&self.audit),
                    )
                    .await?,
                    authority: authority.clone(),
                })
            })
            .await?;
        if initialized.authority != authority {
            return Err(self.invalid(
                "configure authority",
                "worker is already pinned to another node or signing authority",
            ));
        }
        Ok(())
    }

    fn initialized(&self, boot_id: Uuid) -> Result<&Initialized, RuntimeError> {
        if boot_id != self.identity.boot_id {
            return Err(self.invalid("apply command", "worker boot changed"));
        }
        self.initialized
            .get()
            .ok_or_else(|| self.invalid("apply command", "worker authority is not configured"))
    }

    pub async fn install(
        &self,
        boot_id: Uuid,
        snapshot: &SignedSnapshot,
    ) -> Result<u64, RuntimeError> {
        self.initialized(boot_id)?.proxy.install(snapshot).await
    }

    pub async fn bind(
        &self,
        boot_id: Uuid,
        spec: BindingSpec,
        snapshot_version: u64,
    ) -> Result<Arc<SandboxBinding>, RuntimeError> {
        self.initialized(boot_id)?
            .proxy
            .bind(spec, snapshot_version)
            .await
    }

    pub async fn update(
        &self,
        boot_id: Uuid,
        binding: &SandboxBinding,
        update: BindingUpdate,
    ) -> Result<(), RuntimeError> {
        self.initialized(boot_id)?
            .proxy
            .update(binding, update)
            .await
    }
}

#[cfg(test)]
mod tests;
