// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane transport over the existing authenticated provider. Identity
//! comes from a scoped database row and host inspection, never a guest request.
use std::{future::Future, sync::Arc, time::Duration};
use temps_agent_core::error::AgentError;
use temps_entities::{sandbox_access_delivery, sandbox_access_runtimes, sandboxes};
use temps_sandbox_providers::{
    service_access::{ServiceAccessCommand, ServiceAccessReply, ServiceAccessTarget},
    SandboxBackend, SandboxHandle, SandboxProvider, SANDBOX_WORK_DIR,
};
use temps_service_access_runtime::SessionDelivery;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::service::HostObservation;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("Cannot {operation} service access for sandbox {sandbox_id}: {reason}")]
    Invalid {
        sandbox_id: String,
        operation: &'static str,
        reason: &'static str,
    },
    #[error("Cannot {operation} service access for sandbox {sandbox_id}: worker did not respond within {seconds} seconds")]
    Timeout {
        sandbox_id: String,
        operation: &'static str,
        seconds: u64,
    },
    #[error("Cannot {operation} service access for sandbox {sandbox_id}: two worker operations are already in progress; retry later")]
    Busy {
        sandbox_id: String,
        operation: &'static str,
    },
    #[error(
        "Cannot {operation} service access for sandbox {sandbox_id}: the assigned provider failed"
    )]
    Provider {
        sandbox_id: String,
        operation: &'static str,
        #[source]
        source: Box<AgentError>,
    },
}
fn invalid(id: &str, operation: &'static str, reason: &'static str) -> TransportError {
    TransportError::Invalid {
        sandbox_id: id.into(),
        operation,
        reason,
    }
}

/// Exact immutable destination persisted with consent and copied into deletion
/// commands. Constructing it does not authorize access; the issuer does that.
#[derive(Debug)]
pub struct StoredTarget {
    handle: SandboxHandle,
    target: ServiceAccessTarget,
    boot_id: Uuid,
}
impl StoredTarget {
    pub fn from_runtime(row: &sandbox_access_runtimes::Model) -> Result<Self, TransportError> {
        Self::new(
            row.id,
            row.node_id,
            row.authority_id,
            Some(&row.provider_identity),
            row.provider_name.as_deref(),
            row.provider_backend.as_deref(),
        )
    }
    pub fn from_command(row: &sandbox_access_delivery::Model) -> Result<Self, TransportError> {
        Self::new(
            row.runtime_id,
            row.node_id,
            row.worker_boot_id,
            row.provider_identity.as_deref(),
            row.provider_name.as_deref(),
            row.provider_backend.as_deref(),
        )
    }
    fn new(
        incarnation: Uuid,
        node_id: Option<i32>,
        boot_id: Option<Uuid>,
        container_id: Option<&str>,
        name: Option<&str>,
        backend: Option<&str>,
    ) -> Result<Self, TransportError> {
        let id = incarnation.to_string();
        let error = || {
            invalid(
                &id,
                "resolve stored target",
                "worker boot, immutable provider identity and backend are required",
            )
        };
        let boot_id = boot_id.filter(|id| !id.is_nil()).ok_or_else(error)?;
        let container_id = container_id
            .filter(|id| valid_text(id, 512))
            .ok_or_else(error)?;
        let name = name
            .filter(|name| valid_text(name, 255))
            .ok_or_else(error)?;
        let backend = parse_backend(backend).ok_or_else(error)?;
        if incarnation.is_nil() || node_id.is_some_and(|id| id <= 0) {
            return Err(error());
        }
        Ok(Self {
            handle: SandboxHandle {
                node_id,
                sandbox_id: container_id.into(),
                sandbox_name: name.into(),
                work_dir: SANDBOX_WORK_DIR.into(),
                backend,
                image: String::new(),
                conditions: Vec::new(),
            },
            target: ServiceAccessTarget {
                incarnation: id,
                container_id: container_id.into(),
            },
            boot_id,
        })
    }
}
fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn parse_backend(value: Option<&str>) -> Option<SandboxBackend> {
    match value {
        Some("docker") => Some(SandboxBackend::Docker),
        Some("firecracker") => Some(SandboxBackend::Firecracker),
        Some("microsandbox") => Some(SandboxBackend::Microsandbox),
        _ => None,
    }
}

/// One instance per control plane. Admission is bounded and has no waiting
/// queue. Provider routing preserves the owning node and never falls back.
pub struct WorkerTransport {
    provider: Arc<dyn SandboxProvider>,
    slots: Semaphore,
}
impl WorkerTransport {
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            slots: Semaphore::new(2),
        }
    }
    async fn bounded<T>(
        &self,
        sandbox_id: &str,
        operation: &'static str,
        seconds: u64,
        work: impl Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        let _permit = self.slots.try_acquire().map_err(|_| TransportError::Busy {
            sandbox_id: sandbox_id.into(),
            operation,
        })?;
        tokio::time::timeout(Duration::from_secs(seconds), work)
            .await
            .map_err(|_| TransportError::Timeout {
                sandbox_id: sandbox_id.into(),
                operation,
                seconds,
            })?
    }
    fn provider_error(id: &str, operation: &'static str, source: AgentError) -> TransportError {
        TransportError::Provider {
            sandbox_id: id.into(),
            operation,
            source: Box::new(source),
        }
    }
    /// The caller loads and authorizes this sandbox row before inspection. A
    /// subsequent consent transaction rechecks placement and running state.
    pub async fn inspect(
        &self,
        sandbox: &sandboxes::Model,
    ) -> Result<HostObservation, TransportError> {
        let id = sandbox.public_id.as_str();
        let backend = parse_backend(sandbox.backend.as_deref()).ok_or_else(|| {
            invalid(
                id,
                "inspect",
                "sandbox backend does not support credential transport",
            )
        })?;
        if sandbox.status != "running"
            || sandbox.node_id.is_some_and(|id| id <= 0)
            || !valid_text(id, 128)
        {
            return Err(invalid(
                id,
                "inspect",
                "sandbox must be running with a valid owner and identity",
            ));
        }
        let name = format!("temps-sandbox-{id}");
        self.bounded(id, "inspect", 10, async {
            let handle = self
                .provider
                .recover_by_name_on(sandbox.node_id, &name)
                .await
                .map_err(|source| Self::provider_error(id, "recover owner", source))?
                .ok_or_else(|| {
                    invalid(id, "inspect", "assigned worker no longer has this sandbox")
                })?;
            if handle.node_id != sandbox.node_id
                || handle.backend != backend
                || handle.sandbox_name != name
                || !valid_text(&handle.sandbox_id, 512)
            {
                return Err(invalid(
                    id,
                    "inspect",
                    "recovered sandbox has a different owner, name or backend",
                ));
            }
            let reply = self
                .provider
                .service_access(&handle, ServiceAccessCommand::Inspect)
                .await
                .map_err(|source| Self::provider_error(id, "inspect", source))?;
            match reply {
                ServiceAccessReply::Inspect { target, worker, .. } => {
                    let incarnation = Uuid::parse_str(&target.incarnation).map_err(|_| {
                        invalid(id, "inspect", "worker returned an invalid incarnation")
                    })?;
                    if incarnation.is_nil()
                        || incarnation.to_string() != target.incarnation
                        || target.container_id != handle.sandbox_id
                        || worker.boot_id.is_nil()
                        || worker.seal_public_key == [0; 32]
                    {
                        return Err(invalid(
                            id,
                            "inspect",
                            "worker identity does not match the recovered sandbox",
                        ));
                    }
                    Ok(HostObservation {
                        worker,
                        incarnation,
                        node_id: handle.node_id,
                        container_id: handle.sandbox_id,
                        container_name: handle.sandbox_name,
                        backend: backend.to_string(),
                    })
                }
                _ => Err(invalid(
                    id,
                    "inspect",
                    "worker returned a different operation receipt",
                )),
            }
        })
        .await
    }
    /// The caller supplies the expected version from its signed issuance result,
    /// never a value received from a worker or guest.
    pub async fn apply(
        &self,
        target: &StoredTarget,
        snapshot_version: u64,
        delivery: SessionDelivery,
    ) -> Result<(), TransportError> {
        let id = target.target.incarnation.as_str();
        if delivery.authority.boot_id != target.boot_id
            || snapshot_version == 0
            || delivery.validate().is_err()
        {
            return Err(invalid(
                id,
                "apply",
                "signed delivery belongs to another worker or exceeds protocol bounds",
            ));
        }
        let binding_version = delivery.version;
        self.bounded(id, "apply", 10, async {
            let reply = self
                .provider
                .service_access(
                    &target.handle,
                    ServiceAccessCommand::Apply {
                        expected: target.target.clone(),
                        delivery: Box::new(delivery),
                    },
                )
                .await
                .map_err(|source| Self::provider_error(id, "apply", source))?;
            match reply {
                ServiceAccessReply::Applied {
                    target: applied,
                    snapshot_version: snapshot,
                    binding_version: binding,
                } if applied == target.target
                    && snapshot == snapshot_version
                    && binding == binding_version =>
                {
                    Ok(())
                }
                _ => Err(invalid(
                    id,
                    "apply",
                    "worker did not acknowledge the exact target and policy versions",
                )),
            }
        })
        .await
    }
    /// Retirement uses the saved immutable target even after deletion. It must
    /// never recover by name, which could now identify a replacement sandbox.
    pub async fn retire(&self, target: &StoredTarget) -> Result<(), TransportError> {
        let id = target.target.incarnation.as_str();
        self.bounded(id, "retire", 5, async {
            let reply = self
                .provider
                .service_access(
                    &target.handle,
                    ServiceAccessCommand::Retire {
                        expected: target.target.clone(),
                        boot_id: target.boot_id,
                    },
                )
                .await
                .map_err(|source| Self::provider_error(id, "retire", source))?;
            match reply {
                ServiceAccessReply::Retired { target: retired } if retired == target.target => {
                    Ok(())
                }
                _ => Err(invalid(
                    id,
                    "retire",
                    "worker did not acknowledge retirement of the exact target",
                )),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests;
