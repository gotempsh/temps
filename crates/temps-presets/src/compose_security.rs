// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Compose security check catalog and the mapping from restricted service
//! fields to checks, shared by validation and the API.
//!
//! `ComposeSecurityCheck` itself stays in `temps_entities::compose_security`
//! because the policy column stores it, so these are an extension trait.
use serde::Serialize;
use temps_entities::compose_security::ComposeSecurityCheck;

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComposeSecurityCheckDefinition {
    pub id: ComposeSecurityCheck,
    pub group: &'static str,
    pub label: &'static str,
    pub consequence: &'static str,
}

pub trait ComposeSecurityCheckExt: Sized {
    /// The deploy-time check that guards a service field forbidden by default
    /// in inline overrides. Keep preview and deployment aligned on the grant
    /// that makes each field usable.
    fn for_restricted_service_field(field: &str) -> Option<Self>;

    fn catalog() -> Vec<ComposeSecurityCheckDefinition>;
}

impl ComposeSecurityCheckExt for ComposeSecurityCheck {
    fn for_restricted_service_field(field: &str) -> Option<Self> {
        Some(match field {
            "privileged" => Self::Privileged,
            "cgroup_parent" => Self::CgroupParent,
            "cap_add" => Self::Capabilities,
            "devices" => Self::Devices,
            "device_cgroup_rules" => Self::DeviceRules,
            "security_opt" => Self::SecurityOptions,
            "sysctls" => Self::Sysctls,
            "volumes_from" => Self::VolumesFrom,
            "external_links" => Self::ExternalLinks,
            "label_file" => Self::LabelFiles,
            "post_start" | "pre_stop" => Self::LifecycleHooks,
            "provider" => Self::Provider,
            "container_name" => Self::ContainerName,
            "blkio_config" => Self::Blkio,
            "storage_opt" => Self::StorageOptions,
            "memswap_limit" => Self::Swap,
            "group_add" => Self::Groups,
            "runtime" => Self::Runtime,
            "oom_kill_disable" => Self::OomKiller,
            "tmpfs" => Self::Tmpfs,
            "ulimits" => Self::Ulimits,
            _ => return None,
        })
    }

    fn catalog() -> Vec<ComposeSecurityCheckDefinition> {
        vec![
            ComposeSecurityCheckDefinition { id: Self::Extends, group: "Composition", label: "Block extends", consequence: "Reuse service definitions from other Compose files." },
            ComposeSecurityCheckDefinition { id: Self::Include, group: "Composition", label: "Block include", consequence: "Load additional Compose files and their services." },
            ComposeSecurityCheckDefinition { id: Self::Interpolation, group: "Composition", label: "Block variables in guarded fields", consequence: "Resolve variables in security-sensitive settings before validating their values." },
            ComposeSecurityCheckDefinition { id: Self::InlineServices, group: "Composition", label: "Block new services in inline overrides", consequence: "Add services through the inline Compose override." },
            ComposeSecurityCheckDefinition { id: Self::InlineSections, group: "Composition", label: "Block top-level inline override sections", consequence: "Add networks, volumes, configs, and secrets through an override." },
            ComposeSecurityCheckDefinition { id: Self::InlineFields, group: "Composition", label: "Block restricted inline override fields", consequence: "Set restricted fields in overrides; their individual policies still apply." },
            ComposeSecurityCheckDefinition { id: Self::Privileged, group: "Runtime", label: "Block privileged containers", consequence: "Give containers privileged access to the Docker host." },
            ComposeSecurityCheckDefinition { id: Self::DockerSocket, group: "Runtime", label: "Block Docker Engine access", consequence: "Mount the Docker socket or use use_api_socket to control the host daemon." },
            ComposeSecurityCheckDefinition { id: Self::Capabilities, group: "Runtime", label: "Block additional capabilities", consequence: "Grant custom Linux capabilities with cap_add." },
            ComposeSecurityCheckDefinition { id: Self::DropCapabilities, group: "Runtime", label: "Drop Linux capabilities", consequence: "Restore Docker default capabilities instead of the Temps restricted set." },
            ComposeSecurityCheckDefinition { id: Self::SecurityOptions, group: "Runtime", label: "Block custom security options", consequence: "Customize seccomp, AppArmor, and other security_opt settings." },
            ComposeSecurityCheckDefinition { id: Self::NoNewPrivileges, group: "Runtime", label: "Prevent sudo and setuid privilege elevation", consequence: "Allow programs such as sudo to gain root privileges through setuid or file capabilities inside containers. Some images need this during startup." },
            ComposeSecurityCheckDefinition { id: Self::Devices, group: "Runtime", label: "Block host devices", consequence: "Map host devices into containers." },
            ComposeSecurityCheckDefinition { id: Self::DeviceRules, group: "Runtime", label: "Block device cgroup rules", consequence: "Configure device access rules." },
            ComposeSecurityCheckDefinition { id: Self::Gpu, group: "Runtime", label: "Block GPU access", consequence: "Expose GPUs and reserved devices to containers." },
            ComposeSecurityCheckDefinition { id: Self::Sysctls, group: "Runtime", label: "Block kernel parameters", consequence: "Set namespaced kernel parameters through sysctls." },
            ComposeSecurityCheckDefinition { id: Self::Groups, group: "Runtime", label: "Block supplementary groups", consequence: "Add supplementary groups through group_add." },
            ComposeSecurityCheckDefinition { id: Self::CgroupParent, group: "Runtime", label: "Block custom cgroup placement", consequence: "Place containers under a custom cgroup parent." },
            ComposeSecurityCheckDefinition { id: Self::Runtime, group: "Runtime", label: "Block alternative runtimes", consequence: "Select another installed OCI runtime." },
            ComposeSecurityCheckDefinition { id: Self::LifecycleHooks, group: "Runtime", label: "Block lifecycle hooks", consequence: "Run post_start and pre_stop hooks, including privileged hooks." },
            ComposeSecurityCheckDefinition { id: Self::Provider, group: "Runtime", label: "Block Compose providers", consequence: "Invoke installed Compose provider plugins." },
            ComposeSecurityCheckDefinition { id: Self::ContainerName, group: "Runtime", label: "Block custom container names", consequence: "Use daemon-global container names that may collide with other projects." },
            ComposeSecurityCheckDefinition { id: Self::Init, group: "Runtime", label: "Inject Docker init", consequence: "Let the Compose file and image control init behavior." },
            ComposeSecurityCheckDefinition { id: Self::HostNetwork, group: "Networking", label: "Block host networking", consequence: "Share the host network namespace." },
            ComposeSecurityCheckDefinition { id: Self::HostPid, group: "Networking", label: "Block host PID namespace", consequence: "Share the host process namespace." },
            ComposeSecurityCheckDefinition { id: Self::HostIpc, group: "Networking", label: "Block host IPC namespace", consequence: "Share the host IPC namespace." },
            ComposeSecurityCheckDefinition { id: Self::HostUts, group: "Networking", label: "Block host UTS namespace", consequence: "Share the host hostname namespace." },
            ComposeSecurityCheckDefinition { id: Self::HostCgroup, group: "Networking", label: "Block host cgroup namespace", consequence: "Share the host cgroup namespace." },
            ComposeSecurityCheckDefinition { id: Self::HostUser, group: "Networking", label: "Block host user namespace", consequence: "Use the host user namespace." },
            ComposeSecurityCheckDefinition { id: Self::ContainerNamespace, group: "Networking", label: "Block other-container namespaces", consequence: "Join namespaces of arbitrary existing containers." },
            ComposeSecurityCheckDefinition { id: Self::NetworkMode, group: "Networking", label: "Restrict other network modes", consequence: "Use network modes outside project networks, none, and declared services." },
            ComposeSecurityCheckDefinition { id: Self::ExternalNetworks, group: "Networking", label: "Block external networks", consequence: "Connect to existing Docker networks." },
            ComposeSecurityCheckDefinition { id: Self::NetworkNames, group: "Networking", label: "Block custom network names", consequence: "Use daemon-global Docker network names." },
            ComposeSecurityCheckDefinition { id: Self::NetworkDrivers, group: "Networking", label: "Restrict network drivers to bridge", consequence: "Use other installed network drivers." },
            ComposeSecurityCheckDefinition { id: Self::NetworkOptions, group: "Networking", label: "Block network driver options", consequence: "Change network driver options affecting host networking." },
            ComposeSecurityCheckDefinition { id: Self::NetworkIpam, group: "Networking", label: "Block custom IPAM", consequence: "Configure network address allocation and routing." },
            ComposeSecurityCheckDefinition { id: Self::ExternalLinks, group: "Networking", label: "Block external container links", consequence: "Connect to containers outside this project." },
            ComposeSecurityCheckDefinition { id: Self::PublishedPorts, group: "Networking", label: "Require loopback port bindings", consequence: "Publish ports on other host addresses, bypassing the Temps proxy." },
            ComposeSecurityCheckDefinition { id: Self::BindMounts, group: "Storage", label: "Confine bind mounts to the project", consequence: "Mount absolute host paths or paths outside the project." },
            ComposeSecurityCheckDefinition { id: Self::VolumeDrivers, group: "Storage", label: "Block custom volume drivers", consequence: "Use non-local volume drivers." },
            ComposeSecurityCheckDefinition { id: Self::VolumeNetworkFilesystems, group: "Storage", label: "Block network filesystem volumes", consequence: "Mount NFS, CIFS, and other network filesystems through volume options." },
            ComposeSecurityCheckDefinition { id: Self::VolumeHostPaths, group: "Storage", label: "Confine named-volume host paths", consequence: "Use host paths through named-volume driver options." },
            ComposeSecurityCheckDefinition { id: Self::VolumeOptions, group: "Storage", label: "Block other volume driver options", consequence: "Pass custom options to volume drivers." },
            ComposeSecurityCheckDefinition { id: Self::ExternalVolumes, group: "Storage", label: "Block external volumes", consequence: "Attach existing Docker volumes, including other projects' data." },
            ComposeSecurityCheckDefinition { id: Self::VolumeNames, group: "Storage", label: "Block custom volume names", consequence: "Use daemon-global volume names." },
            ComposeSecurityCheckDefinition { id: Self::VolumesFrom, group: "Storage", label: "Block inherited container volumes", consequence: "Inherit mounts from another container with volumes_from." },
            ComposeSecurityCheckDefinition { id: Self::ConfigPaths, group: "Storage", label: "Confine config file paths", consequence: "Read Compose configs from outside the project." },
            ComposeSecurityCheckDefinition { id: Self::SecretPaths, group: "Storage", label: "Confine secret file paths", consequence: "Read Compose secrets from outside the project." },
            ComposeSecurityCheckDefinition { id: Self::ExternalConfigs, group: "Storage", label: "Block external configs", consequence: "Reference daemon-global or externally named configs." },
            ComposeSecurityCheckDefinition { id: Self::ExternalSecrets, group: "Storage", label: "Block external secrets", consequence: "Reference daemon-global or externally named secrets." },
            ComposeSecurityCheckDefinition { id: Self::EnvFiles, group: "Storage", label: "Confine environment file paths", consequence: "Read existing environment files outside the project; Temps never writes there." },
            ComposeSecurityCheckDefinition { id: Self::LabelFiles, group: "Storage", label: "Block label files", consequence: "Read container labels from files on the deployment host." },
            ComposeSecurityCheckDefinition { id: Self::StorageOptions, group: "Storage", label: "Block storage options", consequence: "Customize Docker storage driver options." },
            ComposeSecurityCheckDefinition { id: Self::OomKiller, group: "Resources", label: "Protect OOM-killer behavior", consequence: "Disable the container OOM killer." },
            ComposeSecurityCheckDefinition { id: Self::ServiceShm, group: "Resources", label: "Limit service shared memory", consequence: "Configure more than 512 MiB shared memory per service." },
            ComposeSecurityCheckDefinition { id: Self::AggregateShm, group: "Resources", label: "Limit aggregate shared memory", consequence: "Configure more than 1 GiB total shared memory per stack." },
            ComposeSecurityCheckDefinition { id: Self::Tmpfs, group: "Resources", label: "Block memory-backed mounts", consequence: "Create tmpfs mounts." },
            ComposeSecurityCheckDefinition { id: Self::Ulimits, group: "Resources", label: "Block custom resource limits", consequence: "Set container ulimits." },
            ComposeSecurityCheckDefinition { id: Self::Pids, group: "Resources", label: "Enforce PID limit", consequence: "Remove the injected 512-process limit." },
            ComposeSecurityCheckDefinition { id: Self::Memory, group: "Resources", label: "Enforce memory limit", consequence: "Remove the injected 4 GiB memory limit." },
            ComposeSecurityCheckDefinition { id: Self::Logging, group: "Resources", label: "Enforce bounded Docker logs", consequence: "Use custom logging settings instead of Temps log rotation." },
            ComposeSecurityCheckDefinition { id: Self::Blkio, group: "Resources", label: "Block block-device I/O options", consequence: "Configure blkio_config scheduling." },
            ComposeSecurityCheckDefinition { id: Self::Swap, group: "Resources", label: "Block custom swap limits", consequence: "Configure memswap_limit." },
            ComposeSecurityCheckDefinition { id: Self::Replicas, group: "Resources", label: "Restrict replicas", consequence: "Use custom scale, replica counts, or deployment modes." },
            ComposeSecurityCheckDefinition { id: Self::RemoteBuild, group: "Build", label: "Block remote build contexts", consequence: "Fetch Git or URL build contexts." },
            ComposeSecurityCheckDefinition { id: Self::BuildContext, group: "Build", label: "Confine build context paths", consequence: "Read build contexts outside the project." },
            ComposeSecurityCheckDefinition { id: Self::Dockerfile, group: "Build", label: "Confine Dockerfile paths", consequence: "Read Dockerfiles outside the project." },
            ComposeSecurityCheckDefinition { id: Self::BuildPrivileged, group: "Build", label: "Block privileged builds", consequence: "Run privileged build steps." },
            ComposeSecurityCheckDefinition { id: Self::BuildEntitlements, group: "Build", label: "Block build entitlements", consequence: "Grant build entitlements such as security.insecure." },
            ComposeSecurityCheckDefinition { id: Self::BuildNetwork, group: "Build", label: "Restrict build networking", consequence: "Use host or named networks during builds." },
            ComposeSecurityCheckDefinition { id: Self::BuildSsh, group: "Build", label: "Block build SSH forwarding", consequence: "Forward configured SSH agents or keys into builds." },
            ComposeSecurityCheckDefinition { id: Self::BuildShm, group: "Build", label: "Block build shared-memory overrides", consequence: "Set build.shm_size." },
            ComposeSecurityCheckDefinition { id: Self::BuildUlimits, group: "Build", label: "Block build resource-limit overrides", consequence: "Set build.ulimits." },
            ComposeSecurityCheckDefinition { id: Self::BuildAdditionalContexts, group: "Build", label: "Block additional build contexts", consequence: "Read additional local or remote build contexts." },
            ComposeSecurityCheckDefinition { id: Self::BuildCacheFrom, group: "Build", label: "Block build cache imports", consequence: "Import build cache from external locations." },
            ComposeSecurityCheckDefinition { id: Self::BuildCacheTo, group: "Build", label: "Block build cache exports", consequence: "Export build cache to local or external locations." },
            ComposeSecurityCheckDefinition { id: Self::BuildTags, group: "Build", label: "Block additional build tags", consequence: "Assign additional daemon-global image tags." },
            ComposeSecurityCheckDefinition { id: Self::ImageReferences, group: "Build", label: "Restrict local image references", consequence: "Use raw image IDs or Temps-internal images." },
            ComposeSecurityCheckDefinition { id: Self::BuildImage, group: "Build", label: "Block custom build image names", consequence: "Assign an explicit image tag to a build." },
            ComposeSecurityCheckDefinition { id: Self::PullPolicy, group: "Build", label: "Enforce registry pulls", consequence: "Use custom pull policies instead of always pulling image services." },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use temps_entities::compose_security::ComposeSecurityPolicy;

    #[test]
    fn default_enforces_every_catalog_check_and_ids_are_unique() {
        let policy = ComposeSecurityPolicy::default();
        let catalog = ComposeSecurityCheck::catalog();
        let ids: BTreeSet<_> = catalog.iter().map(|definition| definition.id).collect();
        assert_eq!(ids.len(), catalog.len());
        assert!(ids.iter().all(|check| policy.enforced(*check)));
    }

    #[test]
    fn restricted_override_fields_map_to_catalog_checks() {
        let catalog: BTreeSet<_> = ComposeSecurityCheck::catalog()
            .into_iter()
            .map(|definition| definition.id)
            .collect();
        for field in ["privileged", "cap_add", "label_file", "tmpfs", "ulimits"] {
            let check = ComposeSecurityCheck::for_restricted_service_field(field)
                .expect("restricted field must have a check");
            assert!(
                catalog.contains(&check),
                "{field} is missing from the catalog"
            );
        }
        assert_eq!(
            ComposeSecurityCheck::for_restricted_service_field("image"),
            None
        );
    }

    #[test]
    fn catalog_definition_keeps_its_api_shape() {
        let definition = ComposeSecurityCheck::catalog()
            .into_iter()
            .find(|definition| definition.id == ComposeSecurityCheck::Extends)
            .expect("extends is in the catalog");
        assert_eq!(
            serde_json::to_value(&definition).unwrap(),
            serde_json::json!({
                "id": "extends",
                "group": "Composition",
                "label": "Block extends",
                "consequence": "Reuse service definitions from other Compose files.",
            })
        );
    }
}
