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
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

/// Public material only. Read through the authenticated worker channel, never
/// accept a guest's asserted worker identity when authorizing a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentity {
    #[schema(value_type = String, format = "uuid")]
    pub boot_id: Uuid,
    pub seal_public_key: [u8; 32],
}

/// Control-plane registration for exactly one worker boot. The caller must
/// authenticate the control plane before invoking `configure`. A different
/// signing authority cannot replace an initialized worker in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerAuthority {
    #[schema(value_type = String, format = "uuid")]
    pub boot_id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub node_id: Uuid,
    pub snapshot_verify_key: [u8; 32],
}

struct Initialized {
    authority: WorkerAuthority,
    proxy: WorkerProxy,
    // Control-plane installs only, never held on the proxy request path.
    installed: Mutex<Option<InstalledPolicy>>,
}

struct InstalledPolicy {
    version: u64,
    signature: String,
    key_id: String,
    certificate: String,
    expires: tokio::time::Instant,
}

/// Constructed only from a verified, successfully installed SDK policy. The
/// sealed private key is never part of this value or the guest mount.
#[derive(Debug)]
pub struct PublicCertificate(String);
impl PublicCertificate {
    pub fn as_pem(&self) -> &str {
        &self.0
    }
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
                    installed: Mutex::new(None),
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
        self.install_with_certificate(boot_id, snapshot)
            .await
            .map(|(version, _)| version)
    }

    pub async fn install_with_certificate(
        &self,
        boot_id: Uuid,
        snapshot: &SignedSnapshot,
    ) -> Result<(u64, PublicCertificate), RuntimeError> {
        let initialized = self.initialized(boot_id)?;
        if snapshot.body.len() > crate::MAX_SNAPSHOT_BYTES {
            return Err(self.invalid("install policy", "snapshot exceeds delivery limit"));
        }
        // An empty body references only the exact snapshot already verified on
        // this boot. It cannot install rules or extend the original lease.
        if snapshot.body.is_empty() {
            let installed = initialized.installed.lock().await;
            let previous = installed
                .as_ref()
                .filter(|previous| {
                    previous.signature == snapshot.signature
                        && previous.key_id == snapshot.kid
                        && tokio::time::Instant::now() < previous.expires
                })
                .ok_or_else(|| {
                    self.invalid(
                        "reuse policy",
                        "referenced policy is missing, replaced or expired",
                    )
                })?;
            return Ok((
                previous.version,
                PublicCertificate(previous.certificate.clone()),
            ));
        }
        let key = VerifyingKey::from_bytes(&initialized.authority.snapshot_verify_key)
            .map_err(|_| self.invalid("install policy", "invalid pinned signing key"))?;
        let body = snapshot.verify(&key)?;
        let der = body.interception_ca.cert_der;
        let now = chrono::Utc::now();
        let lease_end = body
            .issued_at
            .checked_add_signed(chrono::Duration::seconds(i64::from(body.lease_secs)))
            .ok_or_else(|| {
                self.invalid(
                    "install policy",
                    "snapshot lease deadline is outside the supported time range",
                )
            })?;
        let remaining = lease_end
            .signed_duration_since(now)
            .to_std()
            .map_err(|_| self.invalid("install policy", "snapshot lease expired"))?;
        // Cache only a verified signature (bound to exact bytes), version and
        // monotonic deadline. Replaying an accepted snapshot cannot renew its
        // lease, reinstall old rules or replace a newer snapshot.
        let expires = tokio::time::Instant::now()
            + remaining.min(std::time::Duration::from_secs(u64::from(
                crate::MAX_POLICY_LEASE_SECONDS,
            )));
        let mut installed = initialized.installed.lock().await;
        if der.len() > 16 * 1024 {
            return Err(self.invalid("publish CA", "public certificate exceeds 16 KiB"));
        }
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        // The SDK has decoded and validated this certificate before success.
        for chunk in der.as_bytes().chunks(64) {
            let text = std::str::from_utf8(chunk)
                .map_err(|_| self.invalid("publish CA", "certificate encoding is invalid"))?;
            pem.push_str(text);
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        let version = if let Some(previous) = installed.as_ref().filter(|previous| {
            previous.signature == snapshot.signature && previous.version == body.version
        }) {
            if tokio::time::Instant::now() >= previous.expires {
                return Err(self.invalid("install policy", "snapshot lease expired"));
            }
            previous.version
        } else {
            let version = initialized.proxy.install(snapshot).await?;
            *installed = Some(InstalledPolicy {
                version,
                signature: snapshot.signature.clone(),
                key_id: snapshot.kid.clone(),
                certificate: pem.clone(),
                expires,
            });
            version
        };
        Ok((version, PublicCertificate(pem)))
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
