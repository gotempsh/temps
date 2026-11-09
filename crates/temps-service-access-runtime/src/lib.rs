// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Trusted worker proxy. Only authenticated host code installs snapshots and
//! creates attachments. Guest byte streams receive a specific binding and may
//! never select their own identity, session or node.
mod control;
pub use control::{PublicCertificate, WorkerAuthority, WorkerControl, WorkerIdentity};
mod protocol;
pub use protocol::{HostAudit, PolicyEnvelope, SessionDelivery};
#[cfg(unix)]
mod attachment;
#[cfg(unix)]
pub use attachment::UnixAttachment;
#[cfg(unix)]
mod mount;
#[cfg(unix)]
pub use mount::MountDirectory;

use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokenless_core::{
    session::verify_session,
    snapshot::{SignedSnapshot, UnmatchedHostPolicy},
};
use tokenless_dataplane::{
    audit::AuditShipper, capabilities::Capability, state::NodeKeys, Binding, BindingSpec,
    BindingStatus, BindingUpdate, CutReport, DataPlane, DataPlaneConfig, DataPlaneError,
    SnapshotInstaller,
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

pub const MAX_WORKER_BINDINGS: usize = 128;
pub const MAX_WORKER_CONNECTIONS: usize = 256;
pub const MAX_SANDBOX_CONNECTIONS: usize = 32;
pub const MAX_POLICY_LEASE_SECONDS: u32 = 30;
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Worker credential proxy could not {operation} for binding {binding}")]
    Attachment {
        operation: &'static str,
        binding: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Worker credential proxy rejected {operation} for binding {binding}: {reason}")]
    Invalid {
        operation: &'static str,
        binding: String,
        reason: &'static str,
    },
    // SDK errors are retained as sources without rendering request/token data.
    #[error("Worker credential proxy could not {operation} for binding {binding}")]
    DataPlane {
        operation: &'static str,
        binding: String,
        #[source]
        source: DataPlaneError,
    },
    #[error("Worker credential proxy could not verify the signed policy snapshot")]
    Snapshot(#[from] tokenless_core::CoreError),
}

/// Opaque attachment: callers can forward bytes or revoke it, but cannot
/// replace its signed sessions without the worker's validation.
pub struct SandboxBinding {
    inner: Binding,
    _admission: OwnedSemaphorePermit,
    owner: Arc<()>,
}

impl SandboxBinding {
    pub async fn ready(&self, snapshot_version: u64) -> Result<(), RuntimeError> {
        self.inner
            .ready(snapshot_version, Duration::from_secs(5))
            .await
            .map_err(|source| RuntimeError::DataPlane {
                operation: "confirm installed policy",
                binding: self.id().into(),
                source,
            })
    }
    pub fn id(&self) -> &str {
        self.inner.id()
    }
    pub fn status(&self) -> BindingStatus {
        self.inner.status()
    }
    pub async fn serve<S>(&self, stream: S) -> Result<(), RuntimeError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        self.inner
            .serve(stream)
            .await
            .map_err(|source| RuntimeError::DataPlane {
                operation: "forward traffic",
                binding: self.id().into(),
                source,
            })
    }
    pub async fn revoke_and_wait(&self, timeout: Duration) -> Result<CutReport, RuntimeError> {
        self.inner
            .revoke_and_wait(timeout)
            .await
            .map_err(|source| RuntimeError::DataPlane {
                operation: "confirm revocation",
                binding: self.id().into(),
                source,
            })
    }
    pub fn revoke(&self) {
        self.inner.revoke();
    }
}

struct Installed {
    session_keys: HashMap<String, VerifyingKey>,
}

/// Lives for the worker process. Dropping it aborts its owned SDK tasks and
/// closes connections; the control plane must deliver policy again on restart.
pub struct WorkerProxy {
    // Taken only by Drop; startup always supplies the required plane.
    plane: Option<DataPlane>,
    installer: SnapshotInstaller,
    snapshot_key: VerifyingKey,
    installed: Mutex<Installed>,
    bindings: Arc<Semaphore>,
    owner: Arc<()>,
}

impl WorkerProxy {
    pub async fn start(keys: NodeKeys, audit: Arc<dyn AuditShipper>) -> Result<Self, RuntimeError> {
        Self::start_config(keys, audit, DataPlaneConfig::default()).await
    }

    async fn start_config(
        keys: NodeKeys,
        audit: Arc<dyn AuditShipper>,
        transport: DataPlaneConfig,
    ) -> Result<Self, RuntimeError> {
        let snapshot_key = keys.snapshot_key;
        let (plane, installer) = DataPlane::start_embedded(
            DataPlaneConfig {
                listen: ([127, 0, 0, 1], 0).into(),
                max_connections: MAX_WORKER_CONNECTIONS,
                audit_capacity: 4096,
                audit_batch_max: 128,
                connect_timeout: Duration::from_secs(10),
                ..transport
            },
            keys,
            audit,
        )
        .await
        .map_err(|source| RuntimeError::DataPlane {
            operation: "start",
            binding: "worker".into(),
            source,
        })?;
        if let Err(source) = plane.capabilities().require(&[
            Capability::StrictExpiry,
            Capability::RuleDeadlines,
            Capability::BindingGenerations,
            Capability::BindingUpdates,
            Capability::CredentialExpiry,
            Capability::RevocationAck,
        ]) {
            plane.abort();
            return Err(RuntimeError::DataPlane {
                operation: "check capabilities",
                binding: "worker".into(),
                source,
            });
        }
        Ok(Self {
            plane: Some(plane),
            bindings: Arc::new(Semaphore::new(MAX_WORKER_BINDINGS)),
            owner: Arc::new(()),
            installer,
            snapshot_key,
            installed: Mutex::new(Installed {
                session_keys: HashMap::new(),
            }),
        })
    }

    /// Serialize policy install with attachment verification. No guest request
    /// holds this lock; snapshot installation is control-plane work only.
    pub async fn install(&self, snapshot: &SignedSnapshot) -> Result<u64, RuntimeError> {
        if snapshot.body.len() > MAX_SNAPSHOT_BYTES {
            return Err(RuntimeError::Invalid {
                operation: "install snapshot",
                binding: "worker".into(),
                reason: "snapshot exceeds 4 MiB",
            });
        }
        let mut installed = self.installed.lock().await;
        let body = snapshot.verify(&self.snapshot_key)?;
        if body.lease_secs == 0
            || body.lease_secs > MAX_POLICY_LEASE_SECONDS
            || body.unmatched_host_policy != UnmatchedHostPolicy::Deny
            || !body.passthrough_hosts.is_empty()
            || !body.egress.block_private
        {
            return Err(RuntimeError::Invalid { operation: "install snapshot", binding: "worker".into(), reason: "policy requires a 1 to 30 second lease, denied unmatched hosts and private-address filtering" });
        }
        let keys = body
            .session_keys
            .iter()
            .map(|key| Ok((key.kid.clone(), key.decode()?)))
            .collect::<Result<HashMap<_, _>, tokenless_core::CoreError>>()?;
        let version =
            self.installer
                .install(snapshot)
                .map_err(|source| RuntimeError::DataPlane {
                    operation: "install snapshot",
                    binding: "worker".into(),
                    source,
                })?;
        installed.session_keys = keys;
        Ok(version)
    }

    /// The caller resolves the real sandbox incarnation before constructing
    /// `spec`; it must not forward identity fields from a guest request.
    pub async fn bind(
        &self,
        spec: BindingSpec,
        snapshot_version: u64,
    ) -> Result<Arc<SandboxBinding>, RuntimeError> {
        let id = spec.binding_id.clone();
        if spec.max_connections == 0
            || spec.max_connections > MAX_SANDBOX_CONNECTIONS
            || spec.version == 0
        {
            return Err(RuntimeError::Invalid {
                operation: "bind",
                binding: id,
                reason: "binding needs a positive version and 1 to 32 connections",
            });
        }
        let admission = Arc::clone(&self.bindings)
            .try_acquire_owned()
            .map_err(|_| RuntimeError::Invalid {
                operation: "bind",
                binding: id.clone(),
                reason: "worker binding capacity is full",
            })?;
        let binding = {
            let installed = self.installed.lock().await;
            validate_sessions(&installed, &spec.sessions, &id)?;
            Arc::new(SandboxBinding {
                _admission: admission,
                inner: self
                    .plane
                    .as_ref()
                    .ok_or_else(|| RuntimeError::Invalid {
                        operation: "bind",
                        binding: id.clone(),
                        reason: "worker proxy is shutting down",
                    })?
                    .bind(spec)
                    .map_err(|source| RuntimeError::DataPlane {
                        operation: "bind",
                        binding: id.clone(),
                        source,
                    })?,
                owner: Arc::clone(&self.owner),
            })
        };
        if let Err(source) = binding
            .inner
            .ready(snapshot_version, Duration::from_secs(5))
            .await
        {
            binding.revoke();
            return Err(RuntimeError::DataPlane {
                operation: "wait for readiness",
                binding: id,
                source,
            });
        }
        Ok(binding)
    }
    /// Revalidates strict sessions on every update. Success installs policy; it
    /// is not a receipt proving that traffic for a removed rule has stopped.
    pub async fn update(
        &self,
        binding: &SandboxBinding,
        update: BindingUpdate,
    ) -> Result<(), RuntimeError> {
        if !Arc::ptr_eq(&self.owner, &binding.owner) {
            return Err(RuntimeError::Invalid {
                operation: "update",
                binding: binding.id().into(),
                reason: "binding belongs to another worker runtime",
            });
        }
        let installed = self.installed.lock().await;
        validate_sessions(&installed, &update.sessions, binding.id())?;
        binding
            .inner
            .update(update)
            .map_err(|source| RuntimeError::DataPlane {
                operation: "update",
                binding: binding.id().into(),
                source,
            })
    }
}

fn validate_sessions(
    installed: &Installed,
    sessions: &[String],
    id: &str,
) -> Result<(), RuntimeError> {
    let invalid = |reason| RuntimeError::Invalid {
        operation: "validate sessions",
        binding: id.into(),
        reason,
    };
    if sessions.len() != 1 {
        return Err(invalid("a binding requires exactly one grouped session"));
    }
    for token in sessions {
        if token.len() > 32 * 1024 {
            return Err(invalid("session exceeds 32 KiB"));
        }
        let claims = verify_session(token, &installed.session_keys, Utc::now())
            .map_err(|_| invalid("session signature or lifetime is invalid"))?;
        if !claims.strict
            || claims.aud.is_none()
            || claims.sub.is_none()
            || claims.rules.is_empty()
            || claims.rules.len() > 16
        {
            return Err(invalid(
                "sessions require strict expiry, node and incarnation binding, and 1 to 16 rules",
            ));
        }
    }
    Ok(())
}

impl Drop for WorkerProxy {
    fn drop(&mut self) {
        if let Some(plane) = self.plane.take() {
            plane.abort();
        }
    }
}

#[cfg(test)]
mod tests;
