// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Administrator-granted exceptions, stored as a typed JSON column. The catalog
//! and the field mapping shared by validation and the API are in
//! `temps_presets::compose_security`.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposeSecurityCheck {
    Extends,
    Include,
    Interpolation,
    InlineServices,
    InlineSections,
    InlineFields,
    Privileged,
    DockerSocket,
    Capabilities,
    DropCapabilities,
    SecurityOptions,
    NoNewPrivileges,
    Devices,
    DeviceRules,
    Gpu,
    Sysctls,
    Groups,
    CgroupParent,
    Runtime,
    LifecycleHooks,
    Provider,
    ContainerName,
    Init,
    HostNetwork,
    HostPid,
    HostIpc,
    HostUts,
    HostCgroup,
    HostUser,
    ContainerNamespace,
    NetworkMode,
    ExternalNetworks,
    NetworkNames,
    NetworkDrivers,
    NetworkOptions,
    NetworkIpam,
    ExternalLinks,
    PublishedPorts,
    BindMounts,
    VolumeDrivers,
    VolumeNetworkFilesystems,
    VolumeHostPaths,
    VolumeOptions,
    ExternalVolumes,
    VolumeNames,
    VolumesFrom,
    ConfigPaths,
    SecretPaths,
    ExternalConfigs,
    ExternalSecrets,
    EnvFiles,
    LabelFiles,
    StorageOptions,
    OomKiller,
    ServiceShm,
    AggregateShm,
    Tmpfs,
    Ulimits,
    Pids,
    Memory,
    Logging,
    Blkio,
    Swap,
    Replicas,
    RemoteBuild,
    BuildContext,
    Dockerfile,
    BuildPrivileged,
    BuildEntitlements,
    BuildNetwork,
    BuildSsh,
    BuildShm,
    BuildUlimits,
    BuildAdditionalContexts,
    BuildCacheFrom,
    BuildCacheTo,
    BuildTags,
    ImageReferences,
    BuildImage,
    PullPolicy,
}

impl ComposeSecurityCheck {
    /// Stable, non-identifying telemetry label. Matches the stored policy label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extends => "extends",
            Self::Include => "include",
            Self::Interpolation => "interpolation",
            Self::InlineServices => "inline_services",
            Self::InlineSections => "inline_sections",
            Self::InlineFields => "inline_fields",
            Self::Privileged => "privileged",
            Self::DockerSocket => "docker_socket",
            Self::Capabilities => "capabilities",
            Self::DropCapabilities => "drop_capabilities",
            Self::SecurityOptions => "security_options",
            Self::NoNewPrivileges => "no_new_privileges",
            Self::Devices => "devices",
            Self::DeviceRules => "device_rules",
            Self::Gpu => "gpu",
            Self::Sysctls => "sysctls",
            Self::Groups => "groups",
            Self::CgroupParent => "cgroup_parent",
            Self::Runtime => "runtime",
            Self::LifecycleHooks => "lifecycle_hooks",
            Self::Provider => "provider",
            Self::ContainerName => "container_name",
            Self::Init => "init",
            Self::HostNetwork => "host_network",
            Self::HostPid => "host_pid",
            Self::HostIpc => "host_ipc",
            Self::HostUts => "host_uts",
            Self::HostCgroup => "host_cgroup",
            Self::HostUser => "host_user",
            Self::ContainerNamespace => "container_namespace",
            Self::NetworkMode => "network_mode",
            Self::ExternalNetworks => "external_networks",
            Self::NetworkNames => "network_names",
            Self::NetworkDrivers => "network_drivers",
            Self::NetworkOptions => "network_options",
            Self::NetworkIpam => "network_ipam",
            Self::ExternalLinks => "external_links",
            Self::PublishedPorts => "published_ports",
            Self::BindMounts => "bind_mounts",
            Self::VolumeDrivers => "volume_drivers",
            Self::VolumeNetworkFilesystems => "volume_network_filesystems",
            Self::VolumeHostPaths => "volume_host_paths",
            Self::VolumeOptions => "volume_options",
            Self::ExternalVolumes => "external_volumes",
            Self::VolumeNames => "volume_names",
            Self::VolumesFrom => "volumes_from",
            Self::ConfigPaths => "config_paths",
            Self::SecretPaths => "secret_paths",
            Self::ExternalConfigs => "external_configs",
            Self::ExternalSecrets => "external_secrets",
            Self::EnvFiles => "env_files",
            Self::LabelFiles => "label_files",
            Self::StorageOptions => "storage_options",
            Self::OomKiller => "oom_killer",
            Self::ServiceShm => "service_shm",
            Self::AggregateShm => "aggregate_shm",
            Self::Tmpfs => "tmpfs",
            Self::Ulimits => "ulimits",
            Self::Pids => "pids",
            Self::Memory => "memory",
            Self::Logging => "logging",
            Self::Blkio => "blkio",
            Self::Swap => "swap",
            Self::Replicas => "replicas",
            Self::RemoteBuild => "remote_build",
            Self::BuildContext => "build_context",
            Self::Dockerfile => "dockerfile",
            Self::BuildPrivileged => "build_privileged",
            Self::BuildEntitlements => "build_entitlements",
            Self::BuildNetwork => "build_network",
            Self::BuildSsh => "build_ssh",
            Self::BuildShm => "build_shm",
            Self::BuildUlimits => "build_ulimits",
            Self::BuildAdditionalContexts => "build_additional_contexts",
            Self::BuildCacheFrom => "build_cache_from",
            Self::BuildCacheTo => "build_cache_to",
            Self::BuildTags => "build_tags",
            Self::ImageReferences => "image_references",
            Self::BuildImage => "build_image",
            Self::PullPolicy => "pull_policy",
        }
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    ToSchema,
    sea_orm::FromJsonQueryResult,
)]
#[serde(deny_unknown_fields)]
pub struct ComposeSecurityPolicy {
    #[serde(default)]
    pub disabled_checks: BTreeSet<ComposeSecurityCheck>,
}

impl ComposeSecurityPolicy {
    pub fn enforced(&self, check: ComposeSecurityCheck) -> bool {
        !self.disabled_checks.contains(&check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exceptions_round_trip_and_unknown_checks_fail_closed() {
        let policy = ComposeSecurityPolicy {
            disabled_checks: BTreeSet::from([ComposeSecurityCheck::Extends]),
        };
        let encoded = serde_json::to_string(&policy).unwrap();
        assert_eq!(
            serde_json::from_str::<ComposeSecurityPolicy>(&encoded).unwrap(),
            policy
        );
        assert!(policy.enforced(ComposeSecurityCheck::Privileged));
        assert!(!policy.enforced(ComposeSecurityCheck::Extends));
        assert!(serde_json::from_str::<ComposeSecurityPolicy>(
            r#"{"disabled_checks":["unknown"]}"#
        )
        .is_err());
    }
}
