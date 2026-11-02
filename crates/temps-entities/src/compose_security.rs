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
