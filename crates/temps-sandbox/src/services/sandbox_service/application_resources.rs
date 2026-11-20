// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use temps_sandbox_providers::SandboxHostCapacity;

impl ApplicationWorkspaceConfig {
    pub const DEFAULT_CPU: f64 = 4.0;
    pub const DEFAULT_MEMORY_MB: u64 = 8192;

    /// Preserve the requested limit on reads so an operator can correct an
    /// override after moving to a smaller host.
    pub fn display_limits(&self, host: Option<SandboxHostCapacity>) -> (f64, u64) {
        (
            self.cpu_limit.unwrap_or_else(|| {
                host.map_or(Self::DEFAULT_CPU, |h| Self::DEFAULT_CPU.min(h.cpu_cores))
            }),
            self.memory_limit_mb.unwrap_or_else(|| {
                host.map_or(Self::DEFAULT_MEMORY_MB, |h| {
                    Self::DEFAULT_MEMORY_MB.min(h.memory_mb)
                })
            }),
        )
    }
}

impl SandboxService {
    /// A managed workspace binds its directory on the control plane. Ask the
    /// driver that would actually accept its image, disk and process limits.
    pub async fn application_workspace_capacity(
        &self,
        workspace: &str,
        config: &ApplicationWorkspaceConfig,
        backend: Option<&str>,
    ) -> Result<Option<SandboxHostCapacity>, SandboxError> {
        let backend =
            backend
                .map(str::parse)
                .transpose()
                .map_err(|_| SandboxError::Validation {
                    message: format!(
                        "Application workspace {workspace} has an unsupported backend"
                    ),
                })?;
        let mut plan =
            config.compute_config(0, Some(workspace.to_owned()), PathBuf::new(), backend);
        self.workspace_plan_capacity(&mut plan)
            .await
            .map_err(|e| from_agent_error(workspace, e))
    }

    pub(super) async fn apply_application_limits(
        &self,
        config: &mut SandboxCreateConfig,
    ) -> Result<(), AgentError> {
        let cpu = config.cpu_limit;
        let memory = config.memory_limit_mb;
        if let Some(host) = self.workspace_plan_capacity(config).await? {
            apply_host_limits(config, host, cpu, memory)?;
        }
        Ok(())
    }

    async fn workspace_plan_capacity(
        &self,
        config: &mut SandboxCreateConfig,
    ) -> Result<Option<SandboxHostCapacity>, AgentError> {
        // Select the driver with both resource requirements, just like the final create.
        config
            .cpu_limit
            .get_or_insert(ApplicationWorkspaceConfig::DEFAULT_CPU);
        config
            .memory_limit_mb
            .get_or_insert(ApplicationWorkspaceConfig::DEFAULT_MEMORY_MB);
        self.registry.provider().capacity_for_create(config).await
    }
}

impl ApplicationWorkspaceConfig {
    pub fn validate_resource_overrides(
        &self,
        workspace: &str,
        host: Option<SandboxHostCapacity>,
    ) -> Result<(), SandboxError> {
        create_policy::check_cpu_limit(workspace, self.cpu_limit, host.map(|h| h.cpu_cores), None)?;
        create_policy::check_host_memory(
            &CreateSandboxRequest {
                memory_limit_mb: self.memory_limit_mb,
                ..Default::default()
            },
            host.map(|h| h.memory_mb),
        )
    }
}

fn apply_host_limits(
    config: &mut SandboxCreateConfig,
    host: SandboxHostCapacity,
    explicit_cpu: Option<f64>,
    explicit_memory: Option<u64>,
) -> Result<(), AgentError> {
    let cpu = host.cpu_limit(
        config.run_id,
        explicit_cpu,
        ApplicationWorkspaceConfig::DEFAULT_CPU,
    )?;
    let memory = host.memory_limit(
        config.run_id,
        explicit_memory,
        ApplicationWorkspaceConfig::DEFAULT_MEMORY_MB,
    )?;
    config.cpu_limit = Some(cpu);
    config.memory_limit_mb = Some(memory);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_creation_and_rebuild_keep_defaults_distinct_from_overrides(
    ) -> Result<(), AgentError> {
        let default = ApplicationWorkspaceConfig::default();
        let host = SandboxHostCapacity {
            cpu_cores: 2.0,
            memory_mb: 4096,
        };
        let mut compute = default.compute_config(7, None, PathBuf::new(), None);
        let cpu = compute.cpu_limit;
        let memory = compute.memory_limit_mb;
        assert_eq!((cpu, memory), (None, None));
        apply_host_limits(&mut compute, host, cpu, memory)?;
        assert_eq!(
            (compute.cpu_limit, compute.memory_limit_mb),
            (Some(2.0), Some(4096))
        );
        assert_eq!(default.display_limits(Some(host)), (2.0, 4096));
        let memory_bound = SandboxHostCapacity {
            cpu_cores: 8.0,
            ..host
        };
        apply_host_limits(&mut compute, memory_bound, None, None)?;
        assert_eq!(
            (compute.cpu_limit, compute.memory_limit_mb),
            (Some(4.0), Some(4096))
        );
        assert!(matches!(
            apply_host_limits(&mut compute, host, Some(4.0), None),
            Err(AgentError::SandboxCpuLimit {
                run_id: 7,
                requested: 4.0,
                host: 2.0
            })
        ));
        assert!(matches!(
            apply_host_limits(&mut compute, host, None, Some(8192)),
            Err(AgentError::Validation { .. })
        ));
        apply_host_limits(&mut compute, host, Some(0.5), Some(512))?;
        assert_eq!(
            (compute.cpu_limit, compute.memory_limit_mb),
            (Some(0.5), Some(512))
        );
        Ok(())
    }
}
