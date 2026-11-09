// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::service_access::{ServiceAccessCommand, ServiceAccessReply};
use temps_service_access_runtime::SessionDelivery;

/// A timeout or cancellation during delivery must not leave an unacknowledged
/// binding serving requests. Revocation is synchronous; recovery needs a fresh
/// incarnation rather than silently reusing the interrupted authorization.
struct DeliveryGuard(Option<Arc<SandboxBinding>>);
impl Drop for DeliveryGuard {
    fn drop(&mut self) {
        if let Some(binding) = self.0.take() {
            binding.revoke();
        }
    }
}

impl DockerSandboxProvider {
    pub(in crate::docker) async fn service_access_command(
        &self,
        handle: &SandboxHandle,
        command: ServiceAccessCommand,
    ) -> Result<ServiceAccessReply, AgentError> {
        let mounts = self.service_access.as_ref().ok_or_else(|| {
            failure(
                &handle.sandbox_id,
                "deliver command",
                "local Unix mount transport is unavailable",
            )
        })?;
        if let ServiceAccessCommand::Retire { expected, boot_id } = command {
            if expected.container_id != handle.sandbox_id {
                return Err(failure(
                    &handle.sandbox_id,
                    "retire binding",
                    "container identity changed",
                ));
            }
            if boot_id != mounts.control.identity().boot_id {
                // This authenticated provider owns a fresh, process-local control
                // plane. Bindings from another boot cannot survive in it. Acknowledge
                // only the saved target; never retire its replacement incarnation.
                tracing::info!(container_id = %expected.container_id, incarnation = %expected.incarnation, retired_boot = %boot_id, "Obsolete worker boot retirement confirmed");
                return Ok(ServiceAccessReply::Retired { target: expected });
            }
            self.retire_service_access_target(&expected).await?;
            tracing::info!(container_id = %expected.container_id, incarnation = %expected.incarnation, "Sandbox service access retired");
            return Ok(ServiceAccessReply::Retired { target: expected });
        }
        let _operation = mounts.operation(&handle.sandbox_id)?;
        let info = self.inspect_handle_policy(handle).await?;
        match command {
            ServiceAccessCommand::Inspect => {
                let target = match mounts.target(&info) {
                    Ok(target) => target,
                    Err(_) => {
                        mounts.close(&handle.sandbox_id).await?;
                        mounts.recover(&info)?
                    }
                };
                let entries = mounts.entries();
                let binding = entries
                    .get(&target.container_id)
                    .and_then(|entry| entry.binding.as_ref());
                let connected = binding.is_some_and(|(binding, _)| {
                    let status = binding.status();
                    !status.sessions.is_empty() && status.problem().is_none()
                });
                Ok(ServiceAccessReply::Inspect {
                    target,
                    worker: mounts.control.identity(),
                    connected,
                    binding_version: binding.map(|(_, version)| *version),
                })
            }
            ServiceAccessCommand::Apply { expected, delivery } => {
                if mounts.target(&info)? != expected {
                    return Err(failure(
                        &handle.sandbox_id,
                        "apply grant",
                        "sandbox incarnation changed",
                    ));
                }
                self.apply_service_access(mounts, handle, expected, *delivery)
                    .await
            }
            ServiceAccessCommand::Retire { .. } => Err(failure(
                &handle.sandbox_id,
                "retire binding",
                "invalid retirement dispatch",
            )),
        }
    }

    async fn apply_service_access(
        &self,
        mounts: &AccessMounts,
        handle: &SandboxHandle,
        expected: ServiceAccessTarget,
        delivery: SessionDelivery,
    ) -> Result<ServiceAccessReply, AgentError> {
        let rejected = |operation| {
            failure(&handle.sandbox_id, operation, "worker rejected policy delivery; verify the current boot, incarnation, version, signed scope and lease")
        };
        delivery
            .validate()
            .map_err(|_| rejected("validate delivery"))?;
        let previous = mounts
            .entries()
            .get(&expected.container_id)
            .and_then(|entry| entry.binding.clone());
        if previous
            .as_ref()
            .is_some_and(|(_, version)| *version >= delivery.version)
        {
            return Err(failure(
                &handle.sandbox_id,
                "apply grant",
                "binding version must advance; inspect the current receipt before retrying",
            ));
        }
        let boot = delivery.authority.boot_id;
        mounts
            .control
            .configure(delivery.authority)
            .await
            .map_err(|_| rejected("configure authority"))?;
        let snapshot = delivery
            .policy
            .into_snapshot()
            .map_err(|_| rejected("decode policy"))?;
        let (snapshot_version, certificate) = mounts
            .control
            .install_with_certificate(boot, &snapshot)
            .await
            .map_err(|_| rejected("install policy"))?;
        let mut guard = DeliveryGuard(previous.as_ref().map(|(binding, _)| Arc::clone(binding)));
        let binding = match previous {
            Some((binding, _)) => {
                mounts
                    .control
                    .update(
                        boot,
                        &binding,
                        SessionDelivery::binding_update(delivery.session, delivery.version),
                    )
                    .await
                    .map_err(|_| rejected("update binding"))?;
                binding
            }
            None => mounts
                .control
                .bind(
                    boot,
                    SessionDelivery::binding_spec(
                        expected.incarnation.clone(),
                        delivery.session,
                        delivery.version,
                    ),
                    snapshot_version,
                )
                .await
                .map_err(|_| rejected("create binding"))?,
        };
        guard.0 = Some(Arc::clone(&binding));
        // Re-inspect after asynchronous policy preparation. Do not publish a
        // successful receipt for a container stopped or replaced in the meantime.
        let result = async {
            binding
                .ready(snapshot_version)
                .await
                .map_err(|_| rejected("wait for ready binding"))?;
            let info = self.inspect_handle_policy(handle).await?;
            if mounts.target(&info)? != expected {
                return Err(rejected("verify final incarnation"));
            }
            let entries = mounts.entries();
            let entry = entries
                .get(&expected.container_id)
                .ok_or_else(|| rejected("publish public CA"))?;
            entry
                .attachment
                .publish_certificate(&certificate)
                .map_err(|_| rejected("publish public CA"))?;
            if delivery.github_git {
                entry
                    .attachment
                    .publish_github_git_route()
                    .map_err(|_| rejected("publish Git routing ownership"))?;
            }
            drop(entries);
            mounts.activate(&info, &expected, Arc::clone(&binding))?;
            let mut entries = mounts.entries();
            let entry = entries
                .get_mut(&expected.container_id)
                .ok_or_else(|| rejected("record binding version"))?;
            entry.binding = Some((Arc::clone(&binding), delivery.version));
            Ok(())
        }
        .await;
        result?;
        guard.0 = None;
        tracing::info!(container_id = %expected.container_id, incarnation = %expected.incarnation, binding_version = delivery.version, snapshot_version, "Sandbox service access policy applied");
        Ok(ServiceAccessReply::Applied {
            target: expected,
            snapshot_version,
            binding_version: delivery.version,
        })
    }
}

#[cfg(test)]
mod tests;
