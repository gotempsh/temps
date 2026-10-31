// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The security policy applied to compose documents and overrides: the
//! deny-list of keys, host namespaces, devices, sockets and images.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use temps_entities::compose_security::{
    ComposeSecurityCheck as PolicyCheck, ComposeSecurityPolicy,
};

use super::{ComposeError, ComposeExecutor, MAX_RESOLVED_COMPOSE_SERVICES, TEMPS_SECRETS_OVERRIDE};
use yaml_serde::value::{Tag, TaggedValue};
use yaml_serde::Value as YamlValue;
use yaml_serde::{Mapping, Value};

/// Keys an inline override may not set because [`ComposeExecutor`]'s
/// deploy-time security policy rejects them outright, wherever they appear.
/// Telling someone to move one of these into the repository compose file would
/// be sending them to do work that fails later, at deploy, with a different
/// error. Mirrors `temps-presets::docker_compose::NEVER_ALLOWED_SERVICE_KEYS`,
/// which guards the same override on the preview path.
const NEVER_ALLOWED_OVERRIDE_KEYS: &[&str] = &[
    "privileged",
    "cgroup_parent",
    "cap_add",
    "devices",
    "device_cgroup_rules",
    "security_opt",
    "sysctls",
    "volumes_from",
    "external_links",
    "label_file",
    "post_start",
    "pre_stop",
    "provider",
    "container_name",
    "blkio_config",
    "storage_opt",
    "memswap_limit",
    "group_add",
    "runtime",
    "oom_kill_disable",
    "tmpfs",
    "ulimits",
];

/// Keys an inline override may not set, but which the repository compose file
/// legitimately can — [`ComposeExecutor::validate_compose_security_policy`]
/// admits them subject to its own checks (bind-mount path confinement for
/// `volumes`, byte caps for `shm_size`, host-namespace rejection for
/// `pid`/`ipc`/`network_mode` and friends). Kept separate from
/// [`NEVER_ALLOWED_OVERRIDE_KEYS`] so the error can point somewhere that
/// actually works instead of issuing the same blanket "move it to the
/// repository" advice for every key.
const REPO_ONLY_OVERRIDE_KEYS: &[&str] = &[
    "network_mode",
    "pid",
    "ipc",
    "uts",
    "cgroup",
    "userns_mode",
    "cap_drop",
    "volumes",
    "shm_size",
    "labels",
    "build",
    "env_file",
];

impl ComposeExecutor {
    pub(super) fn validate_source_security(&self, content: &str) -> Result<(), ComposeError> {
        let mut source: Value =
            yaml_serde::from_str(content).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose source".to_string(),
                reason: error.to_string(),
            })?;
        source
            .apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose source".to_string(),
                reason: error.to_string(),
            })?;
        if let Some(services) = source.get_mut("services").and_then(Value::as_mapping_mut) {
            for (name, definition) in services.iter_mut() {
                if let Some(service) = definition.as_mapping_mut() {
                    self.reject_interpolation_in_guarded_fields(
                        service,
                        name.as_str().unwrap_or("<source>"),
                    )?;
                    // Only read-sensitive fields are consumed by `config` itself.
                    // Runtime values (including inherited shm_size) must be checked
                    // after Compose applies extends, overrides and !reset tags.
                    service.retain(|key, _| {
                        matches!(key.as_str(), Some("extends" | "label_file" | "env_file"))
                    });
                }
            }
        }
        if let Some(root) = source.as_mapping_mut() {
            root.remove("volumes");
            root.remove("networks");
        }
        let source =
            yaml_serde::to_string(&source).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "Compose source".to_string(),
                reason: error.to_string(),
            })?;
        self.validate_compose_security_policy("Compose source", &source)
    }

    pub(super) fn contains_compose_tag(value: &Value) -> bool {
        match value {
            Value::Tagged(_) => true,
            Value::Sequence(values) => values.iter().any(Self::contains_compose_tag),
            Value::Mapping(values) => values.values().any(Self::contains_compose_tag),
            _ => false,
        }
    }

    /// Preflight security validation. Run this BEFORE tearing down the existing
    /// stack so a policy rejection does not cause downtime on the running deployment.
    pub fn preflight_validate(
        &self,
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> Result<(), ComposeError> {
        self.validate_compose_security_policy("compose file", compose_content)?;
        if let Some(override_content) = compose_override {
            self.validate_compose_security_policy("compose override", override_content)?;
            Self::validate_compose_override_with_policy(
                "compose-preflight",
                compose_content,
                override_content,
                &self.policy,
            )?;
        }
        Ok(())
    }

    /// Filesystem-aware preflight for repository-backed Compose deployments.
    /// This runs before the old stack is torn down, while `deploy` repeats the
    /// checks immediately before build/up as defense against later changes.
    pub fn preflight_validate_filesystem(
        &self,
        project_dir: &Path,
        compose_file: &str,
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> Result<(), ComposeError> {
        Self::validate_compose_filesystem_confinement_with_policy(
            project_dir,
            compose_file,
            "compose file",
            compose_content,
            &self.policy,
        )?;
        if let Some(override_content) = compose_override {
            Self::validate_compose_filesystem_confinement_with_policy(
                project_dir,
                compose_file,
                "compose override",
                override_content,
                &self.policy,
            )?;
        }

        // Validate all possible repository write destinations up front. The
        // write path checks are repeated at the actual write to reduce the
        // check/use window.
        for (path, field) in [
            (compose_file, "compose_path"),
            (".env", ".env"),
            (".env.temps", ".env.temps"),
            (
                "docker-compose.temps-env.yml",
                "docker-compose.temps-env.yml",
            ),
            (
                "docker-compose.temps-network.yml",
                "docker-compose.temps-network.yml",
            ),
            (
                "docker-compose.temps-security.yml",
                "docker-compose.temps-security.yml",
            ),
            (
                "docker-compose.temps-labels.yml",
                "docker-compose.temps-labels.yml",
            ),
            (
                "docker-compose.temps-override.yml",
                "docker-compose.temps-override.yml",
            ),
            (TEMPS_SECRETS_OVERRIDE, TEMPS_SECRETS_OVERRIDE),
        ] {
            Self::confined_write_path(project_dir, Path::new(path), field)?;
        }
        Ok(())
    }

    pub(super) fn validate_compose_security_policy(
        &self,
        source: &str,
        compose_content: &str,
    ) -> Result<(), ComposeError> {
        if compose_content.trim().is_empty() {
            return Ok(());
        }

        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: e.to_string(),
            }
        })?;

        // Expand YAML merge keys (`<<`) so settings inherited from an anchor
        // (privileged, devices, volumes, ...) are visible during validation
        // instead of hiding behind the raw `<<` key. Fail closed if expansion
        // errors — otherwise inherited settings could hide from the checks below
        // while `docker compose` still applies them at runtime.
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        // Reject the top-level `include:` directive. Compose merges included
        // files (repo-controlled) into the project at runtime, but only this
        // document's `services:` are validated here — an included file could
        // reintroduce privileged services, host mounts, etc. Inline the
        // referenced services into the reviewed compose file instead.
        if let Some(root_map) = root.as_mapping() {
            if self.enforced(PolicyCheck::Include)
                && root_map.contains_key(YamlValue::String("include".to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "include".to_string(),
                    reason: "top-level 'include' pulls in unvalidated compose files; \
                             inline the referenced services into this compose file instead"
                        .to_string(),
                });
            }
        }

        // Top-level named volumes whose driver options bind a forbidden host
        // path (e.g. `driver_opts: {type: none, o: bind, device: /}`). Service
        // mounts that reference these by name are rejected below.
        let forbidden_named_volumes = self.validate_top_level_volumes(&root)?;

        // Block host files exposed through top-level configs/secrets `file:` paths.
        self.validate_top_level_files(&root, "configs")?;
        self.validate_top_level_files(&root, "secrets")?;
        self.validate_top_level_networks(&root)?;

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };
        if services.len() > MAX_RESOLVED_COMPOSE_SERVICES {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "services".to_string(),
                reason: format!(
                    "Compose deployments may declare at most {} services; found {}",
                    MAX_RESOLVED_COMPOSE_SERVICES,
                    services.len()
                ),
            });
        }

        const MAX_SERVICE_SHM_BYTES: u64 = 512 * 1024 * 1024;
        const MAX_COMPOSE_SHM_BYTES: u64 = 1024 * 1024 * 1024;
        let mut total_shm_bytes = 0_u64;

        for (service_key, service_value) in services {
            // Service names must be quoted strings. A bare `true`/`false`/`null`
            // or numeric key parses as a non-string scalar here, so it would be
            // dropped by `parse_service_names_yaml` (which keys off `as_str()`)
            // and silently skip the injected security override, while a compose
            // parser may still treat it as a service. Fail closed instead.
            let Some(service_name) = service_key.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<non-string>".to_string(),
                    field: "services".to_string(),
                    reason: "service names must be quoted strings; non-string scalar keys \
                             (booleans, null, or numbers) are ambiguous across compose parsers \
                             and are not allowed"
                        .to_string(),
                });
            };
            // Service names are interpolated verbatim into the generated
            // `docker-compose.temps-security.yml` override. A name containing a
            // newline or `: ` would corrupt that YAML and could silently drop the
            // sandbox-hardening layer, so constrain names to the Compose spec
            // character set.
            if !Self::is_valid_service_name(service_name) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "services".to_string(),
                    reason: "service names may only contain letters, digits, '.', '_' and '-' \
                             (and must start alphanumeric); other characters can corrupt the \
                             generated security override"
                        .to_string(),
                });
            }
            let Some(service) = service_value.as_mapping() else {
                continue;
            };

            // Reject `${...}`/`$(...)` interpolation in security-guarded fields
            // first. Otherwise `network_mode: ${NET:-host}` or
            // `privileged: ${P:-true}` slip past the literal `host`/`true`
            // checks below because the YAML value is an interpolation string.
            self.reject_interpolation_in_guarded_fields(service, service_name)?;

            self.reject_bool(
                service,
                service_name,
                "privileged",
                true,
                "privileged containers can bypass the host sandbox",
            )?;
            self.reject_bool(
                service,
                service_name,
                "use_api_socket",
                true,
                "use_api_socket exposes the docker engine API socket to the container",
            )?;
            self.reject_present(
                service,
                service_name,
                "cap_add",
                "adding Linux capabilities is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "devices",
                "host device passthrough is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "device_cgroup_rules",
                "device cgroup rules can grant host device access",
            )?;
            self.reject_present(
                service,
                service_name,
                "security_opt",
                "custom security options can disable no-new-privileges or confinement",
            )?;
            self.reject_present(
                service,
                service_name,
                "gpus",
                "GPU device requests expose host accelerators and are not allowed",
            )?;
            self.reject_present(
                service,
                service_name,
                "extends",
                "extends can import privileged settings from another compose file; \
                 inline the service definition instead",
            )?;
            self.reject_present(
                service,
                service_name,
                "volumes_from",
                "volumes_from can inherit volumes from arbitrary host containers \
                 outside this deployment (e.g. other tenants' or Temps infrastructure \
                 containers)",
            )?;
            self.reject_present(
                service,
                service_name,
                "external_links",
                "external_links can connect to arbitrary host containers outside this deployment",
            )?;
            self.reject_present(
                service,
                service_name,
                "label_file",
                "label_file can read arbitrary files from the deployment host",
            )?;
            for (field, reason) in [
                (
                    "post_start",
                    "post_start hooks can run privileged commands after container startup",
                ),
                (
                    "pre_stop",
                    "pre_stop hooks can run privileged commands during container shutdown",
                ),
                (
                    "provider",
                    "Compose providers invoke daemon-host plugins and are not allowed",
                ),
                (
                    "container_name",
                    "container_name is daemon-global and can collide with another deployment",
                ),
                (
                    "blkio_config",
                    "blkio_config can alter host device I/O scheduling",
                ),
                (
                    "storage_opt",
                    "storage_opt can allocate unbounded daemon-host storage",
                ),
                (
                    "memswap_limit",
                    "custom swap limits can bypass the platform memory-pressure policy",
                ),
            ] {
                self.reject_present(service, service_name, field, reason)?;
            }
            self.reject_present(
                service,
                service_name,
                "sysctls",
                "setting kernel parameters (sysctls) is not allowed for compose deployments",
            )?;
            self.reject_present(
                service,
                service_name,
                "group_add",
                "adding supplementary groups (e.g. the docker group) can escalate host access",
            )?;
            self.reject_present(
                service,
                service_name,
                "cgroup_parent",
                "cgroup_parent can place the container in an arbitrary host cgroup",
            )?;
            self.reject_present(
                service,
                service_name,
                "runtime",
                "selecting an alternate OCI runtime can bypass the enforced container sandbox",
            )?;
            self.reject_present(
                service,
                service_name,
                "oom_kill_disable",
                "disabling the OOM killer can turn container memory pressure into a host-wide denial of service",
            )?;
            if let Some(shm_size) = service.get(YamlValue::String("shm_size".to_string())) {
                let bytes = Self::parse_compose_byte_size(shm_size).ok_or_else(|| {
                    ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: "shared-memory size must be a positive byte count or a value such as '128m', '256mb', or '512MiB'".to_string(),
                    }
                })?;
                if self.enforced(PolicyCheck::ServiceShm) && bytes > MAX_SERVICE_SHM_BYTES {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: format!(
                            "'Limit service shared memory' (Resources) limits shared-memory size to {} MiB per service; lower shm_size or disable that check for a trusted stack",
                            MAX_SERVICE_SHM_BYTES / 1024 / 1024
                        ),
                    });
                }
                total_shm_bytes = total_shm_bytes.checked_add(bytes).ok_or_else(|| {
                    ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: "aggregate shared-memory sizing exceeds the supported range"
                            .to_string(),
                    }
                })?;
                if self.enforced(PolicyCheck::AggregateShm)
                    && total_shm_bytes > MAX_COMPOSE_SHM_BYTES
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "shm_size".to_string(),
                        reason: format!(
                            "'Limit aggregate shared memory' (Resources) limits total shared-memory size to {} MiB per Compose deployment; lower the stack total or disable that check for a trusted stack",
                            MAX_COMPOSE_SHM_BYTES / 1024 / 1024
                        ),
                    });
                }
            }
            self.reject_present(
                service,
                service_name,
                "tmpfs",
                "user-defined tmpfs mounts are not allowed because their aggregate host-memory use is not bounded",
            )?;
            self.reject_present(
                service,
                service_name,
                "ulimits",
                "overriding container ulimits is not allowed for compose deployments",
            )?;
            self.validate_network_mode(service, service_name, services)?;
            self.reject_host_namespace(service, service_name, "pid")?;
            self.reject_host_namespace(service, service_name, "ipc")?;
            self.reject_host_namespace(service, service_name, "cgroup")?;
            self.reject_host_namespace(service, service_name, "uts")?;
            self.reject_host_namespace(service, service_name, "userns_mode")?;
            self.reject_deploy_devices(service, service_name)?;
            self.validate_replica_count(service, service_name)?;
            self.validate_service_image(service, service_name)?;
            self.validate_build_options(service, service_name)?;
            self.validate_env_files(service, service_name)?;
            self.validate_published_ports(service, service_name)?;
            self.validate_service_volumes(service, service_name, &forbidden_named_volumes)?;
        }

        Ok(())
    }

    fn validate_top_level_networks(&self, root: &YamlValue) -> Result<(), ComposeError> {
        let Some(networks) = root.get("networks") else {
            return Ok(());
        };
        let Some(networks) = networks.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "networks".to_string(),
                reason: "top-level networks must be a mapping".to_string(),
            });
        };

        for (network_name, network) in networks {
            let Some(name) = network_name.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "networks".to_string(),
                    reason: "network names must be strings".to_string(),
                });
            };
            if network.is_null() {
                continue;
            }
            let Some(options) = network.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: format!("networks.{name}"),
                    reason: "network configuration must be a mapping".to_string(),
                });
            };

            if self.enforced(PolicyCheck::NetworkNames)
                && options.contains_key(YamlValue::String("name".to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: format!("networks.{name}.name"),
                    reason: "custom Docker network names can attach a service to a daemon-global network outside this deployment"
                        .to_string(),
                });
            }
            if let Some(external) = options.get(YamlValue::String("external".to_string())) {
                if self.enforced(PolicyCheck::ExternalNetworks)
                    && !external.is_null()
                    && external.as_bool() != Some(false)
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.external"),
                        reason: "external Compose networks bypass Temps-managed network policy"
                            .to_string(),
                    });
                }
            }

            if let Some(driver) = options.get(YamlValue::String("driver".to_string())) {
                let Some(driver) = driver.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.driver"),
                        reason: "network driver must be the literal value 'bridge'".to_string(),
                    });
                };
                if self.enforced(PolicyCheck::NetworkDrivers) && driver != "bridge" {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.driver"),
                        reason: format!(
                            "network driver '{driver}' can bypass routed host filtering; \
                             only the bridge driver is allowed"
                        ),
                    });
                }
            }
            for field in ["driver_opts", "ipam"] {
                if self.enforced(if field == "ipam" {
                    PolicyCheck::NetworkIpam
                } else {
                    PolicyCheck::NetworkOptions
                }) && options.contains_key(YamlValue::String(field.to_string()))
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<top-level>".to_string(),
                        field: format!("networks.{name}.{field}"),
                        reason: format!(
                            "custom network {field} can alter daemon-host routing and is not allowed"
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate top-level named volumes and collect definitions whose driver
    /// options would escape the project. Docker volume names are daemon-global,
    /// so `external` and custom `name` values can otherwise mount another
    /// project's database volume into this deployment.
    fn validate_top_level_volumes(
        &self,
        root: &YamlValue,
    ) -> Result<HashSet<String>, ComposeError> {
        let mut forbidden = HashSet::new();
        let Some(volumes_value) = root.get("volumes") else {
            return Ok(forbidden);
        };
        let Some(volumes) = volumes_value.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: "volumes".to_string(),
                reason: "top-level volumes must be a mapping".to_string(),
            });
        };
        for (name, def) in volumes {
            let Some(name) = name.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<top-level>".to_string(),
                    field: "volumes".to_string(),
                    reason: "volume names must be strings".to_string(),
                });
            };
            if def.is_null() {
                continue;
            }
            let Some(def_map) = def.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("volumes.{name}"),
                    field: format!("volumes.{name}"),
                    reason: "volume configuration must be a mapping".to_string(),
                });
            };

            if self.enforced(PolicyCheck::VolumeNames)
                && def_map.contains_key(YamlValue::String("name".to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("volumes.{name}"),
                    field: format!("volumes.{name}.name"),
                    reason: "custom Docker volume names are daemon-global and can reference another deployment's data"
                        .to_string(),
                });
            }
            if let Some(external) = def_map.get(YamlValue::String("external".to_string())) {
                if self.enforced(PolicyCheck::ExternalVolumes)
                    && !external.is_null()
                    && external.as_bool() != Some(false)
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("volumes.{name}"),
                        field: format!("volumes.{name}.external"),
                        reason:
                            "external Docker volumes can expose data owned by another deployment"
                                .to_string(),
                    });
                }
            }

            // A non-`local` volume driver invokes an external volume plugin
            // (NFS/CIFS clients, cloud plugins) that can mount attacker-controlled
            // remote or host filesystems into the container.
            if let Some(driver) = def_map.get(YamlValue::String("driver".to_string())) {
                let Some(driver) = driver.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("volumes.{name}"),
                        field: format!("volumes.{name}.driver"),
                        reason: "volume driver must be the literal value 'local'".to_string(),
                    });
                };
                if self.enforced(PolicyCheck::VolumeDrivers) && driver != "local" {
                    forbidden.insert(name.to_string());
                    continue;
                }
            }

            let Some(driver_opts_value) = def_map.get(YamlValue::String("driver_opts".to_string()))
            else {
                continue;
            };
            let options =
                driver_opts_value
                    .as_mapping()
                    .ok_or_else(|| ComposeError::InvalidComposeYaml {
                        compose_source: format!("volume '{name}'"),
                        reason: "driver_opts must be a mapping".to_string(),
                    })?;
            let fs_type = options
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let network_fs = ["nfs", "nfs4", "cifs", "smb", "smbfs", "glusterfs", "ceph"]
                .contains(&fs_type.to_ascii_lowercase().as_str());
            let bind = fs_type == "none"
                || options
                    .get("o")
                    .and_then(Value::as_str)
                    .is_some_and(|o| o.split(',').any(|v| v == "bind"));
            let check = if network_fs {
                PolicyCheck::VolumeNetworkFilesystems
            } else if bind {
                PolicyCheck::VolumeHostPaths
            } else {
                PolicyCheck::VolumeOptions
            };
            if !self.enforced(check) {
                continue;
            }
            return Err(ComposeError::SecurityPolicyViolation {
                service: format!("volumes.{name}"),
                field: format!("volumes.{name}.driver_opts"),
                reason: "volume driver_opts can make the Docker daemon mount host or network filesystems and are not allowed"
                    .to_string(),
            });
        }
        Ok(forbidden)
    }

    /// Reject top-level `configs.*.file` / `secrets.*.file` entries that point at
    /// forbidden or project-escaping host paths (e.g. `/etc/passwd`).
    fn validate_top_level_files(&self, root: &YamlValue, key: &str) -> Result<(), ComposeError> {
        let Some(value) = root.get(key) else {
            return Ok(());
        };
        let Some(map) = value.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<top-level>".to_string(),
                field: key.to_string(),
                reason: format!("top-level {key} must be a mapping"),
            });
        };
        let paths = if key == "configs" {
            PolicyCheck::ConfigPaths
        } else {
            PolicyCheck::SecretPaths
        };
        let external_check = if key == "configs" {
            PolicyCheck::ExternalConfigs
        } else {
            PolicyCheck::ExternalSecrets
        };
        for (name, def) in map {
            let name = name.as_str().unwrap_or("<unknown>");
            if def.is_null() {
                continue;
            }
            let Some(def_map) = def.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("{key}.{name}"),
                    field: format!("{key}.{name}"),
                    reason: format!("{key} configuration must be a mapping"),
                });
            };
            if self.enforced(external_check)
                && def_map.contains_key(YamlValue::String("name".to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: format!("{key}.{name}"),
                    field: format!("{key}.{name}.name"),
                    reason: format!(
                        "custom Docker {key} names can reference daemon-global objects outside this deployment"
                    ),
                });
            }
            if let Some(external) = def_map.get(YamlValue::String("external".to_string())) {
                if self.enforced(external_check)
                    && !external.is_null()
                    && external.as_bool() != Some(false)
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.{name}.external"),
                        reason: format!(
                            "external Docker {key} can reference objects outside this deployment"
                        ),
                    });
                }
            }
            if let Some(file) = def_map.get(YamlValue::String("file".to_string())) {
                let Some(file) = file.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.file"),
                        reason: format!("{key} file must be a confined literal path"),
                    });
                };
                if self.enforced(paths) && Self::is_dangerous_host_path(file) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: format!("{key}.{name}"),
                        field: format!("{key}.file"),
                        reason: format!("host file '{file}' exposed through {key} is not allowed"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Reject privileged build options before `docker compose build` runs them.
    fn validate_build_options(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(build) = service.get(YamlValue::String("build".to_string())) else {
            return Ok(());
        };
        // Short form (`build: .`) is itself a context path. It needs the same
        // lexical and canonical checks as long-form `build.context`.
        if let Some(context) = build.as_str() {
            if self.enforced(PolicyCheck::RemoteBuild) && Self::is_remote_build_context(context) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.context".to_string(),
                    reason: "remote build contexts are not allowed because Docker would fetch them from the host network".to_string(),
                });
            }
            if self.enforced(PolicyCheck::BuildContext)
                && !Self::is_remote_build_context(context)
                && Self::is_dangerous_host_path(context)
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.context".to_string(),
                    reason: "build context must be a confined relative path; absolute, project-escaping, or interpolated values are not allowed".to_string(),
                });
            }
            return Ok(());
        }
        let Some(build_map) = build.as_mapping() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build".to_string(),
                reason:
                    "build must be a relative context path or a mapping of validated build options"
                        .to_string(),
            });
        };

        if let Some(privileged) = build_map.get(YamlValue::String("privileged".to_string())) {
            match privileged.as_bool() {
                Some(false) => {}
                Some(true) if !self.enforced(PolicyCheck::BuildPrivileged) => {}
                Some(true) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.privileged".to_string(),
                        reason: "privileged build steps can escape the build sandbox".to_string(),
                    });
                }
                None => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.privileged".to_string(),
                        reason: "build.privileged must be the literal boolean false; strings, interpolation, and other value types are not allowed"
                            .to_string(),
                    });
                }
            }
        }
        if self.enforced(PolicyCheck::BuildEntitlements)
            && build_map.contains_key(YamlValue::String("entitlements".to_string()))
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.entitlements".to_string(),
                reason: "build entitlements (e.g. security.insecure) grant host access".to_string(),
            });
        }
        for field in ["additional_contexts", "cache_from", "cache_to", "tags"] {
            if self.field_enforced(&format!("build.{field}"))
                && build_map.contains_key(YamlValue::String(field.to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: format!("build.{field}"),
                    reason: format!(
                        "build.{field} is not allowed because it can read from or write to daemon-host and external locations"
                    ),
                });
            }
        }
        if let Some(network) = build_map.get(YamlValue::String("network".to_string())) {
            let Some(network) = network.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "build.network must be a literal network name; interpolation and other value types are not allowed"
                        .to_string(),
                });
            };
            if Self::contains_interpolation(network) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "interpolation in build.network is not allowed because it can resolve to the host network after validation"
                        .to_string(),
                });
            }
            if self.enforced(PolicyCheck::BuildNetwork)
                && !matches!(network.trim(), "default" | "none")
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "build.network".to_string(),
                    reason: "build.network must be 'default' or 'none'; named and host networks can expose other deployments to build steps"
                        .to_string(),
                });
            }
        }
        if self.enforced(PolicyCheck::BuildSsh)
            && build_map.contains_key(YamlValue::String("ssh".to_string()))
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "build.ssh".to_string(),
                reason: "build.ssh forwards the host SSH agent into the build and can leak \
                         host keys"
                    .to_string(),
            });
        }
        for field in ["shm_size", "ulimits"] {
            if self.field_enforced(&format!("build.{field}"))
                && build_map.contains_key(YamlValue::String(field.to_string()))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: format!("build.{field}"),
                    reason: format!(
                        "build.{field} is not allowed because build resource overrides are not host-bounded"
                    ),
                });
            }
        }
        // `context` and `dockerfile` become the Docker build context / Dockerfile
        // path. An absolute or project-escaping host path (or an interpolated one)
        // would send arbitrary host directories into the build (`COPY . /`), so
        // confine them the same way as bind sources.
        for field in ["context", "dockerfile"] {
            if let Some(raw_value) = build_map.get(YamlValue::String(field.to_string())) {
                let Some(value) = raw_value.as_str() else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: format!("build.{field}"),
                        reason: format!("build.{field} must be a literal relative path"),
                    });
                };
                if self.enforced(PolicyCheck::RemoteBuild)
                    && field == "context"
                    && Self::is_remote_build_context(value)
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "build.context".to_string(),
                        reason: "remote build contexts are not allowed because Docker would fetch them from the host network".to_string(),
                    });
                }
                if self.enforced(if field == "context" {
                    PolicyCheck::BuildContext
                } else {
                    PolicyCheck::Dockerfile
                }) && !(field == "context" && Self::is_remote_build_context(value))
                    && Self::is_dangerous_host_path(value)
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: format!("build.{field}"),
                        reason: format!(
                            "build.{field} must be a confined relative path; absolute, \
                             project-escaping, or interpolated values are not allowed"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// Reject `deploy.resources.reservations.devices` — the long-form equivalent
    /// of the already-blocked `gpus:` short form. Docker Compose translates
    /// `gpus:` into exactly this structure, so leaving it unchecked allows the
    /// same host-accelerator passthrough the `gpus` guard is meant to prevent.
    fn reject_deploy_devices(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        if !self.enforced(PolicyCheck::Gpu) {
            return Ok(());
        }
        let has_devices = service
            .get(YamlValue::String("deploy".to_string()))
            .and_then(YamlValue::as_mapping)
            .and_then(|d| d.get(YamlValue::String("resources".to_string())))
            .and_then(YamlValue::as_mapping)
            .and_then(|r| r.get(YamlValue::String("reservations".to_string())))
            .and_then(YamlValue::as_mapping)
            .is_some_and(|res| res.contains_key(YamlValue::String("devices".to_string())));
        if has_devices {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "deploy.resources.reservations.devices".to_string(),
                reason: "device reservations expose host accelerators/devices (equivalent to \
                         the blocked 'gpus') and are not allowed"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn validate_replica_count(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        if !self.enforced(PolicyCheck::Replicas) {
            return Ok(());
        }
        if let Some(scale) = service.get(YamlValue::String("scale".to_string())) {
            if scale.as_u64() != Some(1) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "scale".to_string(),
                    reason: "Compose services are limited to one replica; scale must be the literal integer 1"
                        .to_string(),
                });
            }
        }

        if let Some(deploy) = service.get(YamlValue::String("deploy".to_string())) {
            let Some(deploy) = deploy.as_mapping() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "deploy".to_string(),
                    reason: "deploy must be a literal mapping".to_string(),
                });
            };
            if let Some(replicas) = deploy.get(YamlValue::String("replicas".to_string())) {
                if replicas.as_u64() != Some(1) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "deploy.replicas".to_string(),
                        reason: "Compose services are limited to one replica; deploy.replicas must be the literal integer 1"
                            .to_string(),
                    });
                }
            }
            if let Some(mode) = deploy.get(YamlValue::String("mode".to_string())) {
                if mode.as_str() != Some("replicated") {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "deploy.mode".to_string(),
                        reason: "only deploy.mode 'replicated' is allowed".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_service_image(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let has_build = service.contains_key(YamlValue::String("build".to_string()));
        if let Some(image) = service.get(YamlValue::String("image".to_string())) {
            let Some(image) = image.as_str() else {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason: "image must be a literal registry reference".to_string(),
                });
            };
            let normalized = image.trim().to_ascii_lowercase();
            let is_raw_image_id = normalized.starts_with("sha256:")
                || (normalized.len() == 64
                    && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()));
            let is_temps_internal =
                normalized == "temps.internal" || normalized.starts_with("temps.internal/");
            // Each cause gets its own reason and the fix that applies to it: a
            // shared message cannot tell the reader which of the rules fired,
            // nor which one they can change. Only an unresolved `${...}`
            // expression is quoted. It is repository text, never a value:
            // when the interpolation check is relaxed, this runs on the
            // resolved model, where a literal may have come from a project
            // variable and must not be copied into a deployment error.
            let rejection = if Self::contains_interpolation(image) {
                Some(if has_build {
                    format!(
                        "image '{image}' uses a variable, and this service also has build:. \
                         Remove image: from the service; Temps names the image it builds itself"
                    )
                } else {
                    format!(
                        "image '{image}' still contains a variable after Temps resolved the \
                         project environment and the repository .env, so the image it selects \
                         cannot be checked. Define the variable in the environment's variables \
                         or the repository .env, give it a default (${{VAR:-ghcr.io/org/app:1}}), \
                         or write the literal registry reference (for example ghcr.io/org/app:1.2.3)"
                    )
                })
            } else if self.enforced(PolicyCheck::ImageReferences) && is_temps_internal {
                Some(
                    "image refers to a Temps-internal (temps.internal/...) image, which may \
                     belong to another deployment. Use build: to build this service from \
                     source, or a registry reference"
                        .to_string(),
                )
            } else if self.enforced(PolicyCheck::ImageReferences) && is_raw_image_id {
                Some(
                    "image is a raw image ID (sha256:...), which can select another \
                     deployment's local image. Use a registry reference such as \
                     ghcr.io/org/app:1.2.3 or ghcr.io/org/app@sha256:<digest>"
                        .to_string(),
                )
            } else {
                None
            };
            if let Some(reason) = rejection {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason,
                });
            }
            if self.enforced(PolicyCheck::BuildImage) && has_build {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "image".to_string(),
                    reason: "image is set on a service that also has build:. Remove image:; \
                             Temps assigns a deployment-scoped name to the image it builds so \
                             it cannot collide with another deployment's tag"
                        .to_string(),
                });
            }
        }

        if let Some(pull_policy) = service.get(YamlValue::String("pull_policy".to_string())) {
            if self.enforced(PolicyCheck::PullPolicy)
                && (has_build || pull_policy.as_str() != Some("always"))
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "pull_policy".to_string(),
                    reason: "image services always pull from their registry and build services use a Temps-scoped local image; custom pull_policy values are not allowed"
                        .to_string(),
                });
            }
        }

        Ok(())
    }

    fn validate_env_files(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let Some(env_file) = service.get(YamlValue::String("env_file".to_string())) else {
            return Ok(());
        };
        let validate_path = |path: &str| -> Result<(), ComposeError> {
            if self.enforced(PolicyCheck::EnvFiles)
                && (Self::contains_interpolation(path)
                    || Self::validate_relative_path(path, "env_file").is_err())
            {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "env_file".to_string(),
                    reason: "env_file paths must be literal relative paths confined to the project"
                        .to_string(),
                });
            }
            Ok(())
        };
        match env_file {
            YamlValue::String(path) => validate_path(path),
            YamlValue::Sequence(entries) => {
                for entry in entries {
                    match entry {
                        YamlValue::String(path) => validate_path(path)?,
                        YamlValue::Mapping(options) => {
                            let Some(path) = options
                                .get(YamlValue::String("path".to_string()))
                                .and_then(YamlValue::as_str)
                            else {
                                return Err(ComposeError::SecurityPolicyViolation {
                                    service: service_name.to_string(),
                                    field: "env_file".to_string(),
                                    reason:
                                        "long-form env_file entries must contain a literal path"
                                            .to_string(),
                                });
                            };
                            validate_path(path)?;
                        }
                        _ => {
                            return Err(ComposeError::SecurityPolicyViolation {
                                service: service_name.to_string(),
                                field: "env_file".to_string(),
                                reason: "env_file must be a path or a sequence of path entries"
                                    .to_string(),
                            });
                        }
                    }
                }
                Ok(())
            }
            _ => Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "env_file".to_string(),
                reason: "env_file must be a literal path or sequence".to_string(),
            }),
        }
    }

    fn validate_published_ports(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        if !self.enforced(PolicyCheck::PublishedPorts) {
            return Ok(());
        }
        let Some(ports) = service.get(YamlValue::String("ports".to_string())) else {
            return Ok(());
        };
        let Some(ports) = ports.as_sequence() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "ports".to_string(),
                reason: "ports must be a sequence of loopback-only bindings".to_string(),
            });
        };
        for port in ports {
            let loopback_only = match port {
                YamlValue::String(binding) => {
                    !Self::contains_interpolation(binding)
                        && binding.starts_with("127.0.0.1:")
                        && binding.split(':').count() >= 3
                }
                YamlValue::Mapping(binding) => {
                    binding
                        .get(YamlValue::String("host_ip".to_string()))
                        .and_then(YamlValue::as_str)
                        == Some("127.0.0.1")
                }
                _ => false,
            };
            if !loopback_only {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "ports".to_string(),
                    reason: Self::loopback_port_remediation(port),
                });
            }
        }
        Ok(())
    }

    /// Rejection text for a published port that is not loopback-only, naming
    /// the exact entry and the exact replacement. Temps does not rewrite the
    /// file itself: which ports a stack exposes is the author's decision.
    fn loopback_port_remediation(port: &YamlValue) -> String {
        const WHY: &str = "published ports must explicitly bind to 127.0.0.1 so traffic cannot \
                           bypass the Temps proxy";
        const ROUTE: &str = "then select the service as public in the project settings so Temps \
                             routes traffic to it";
        let short_form = match port {
            YamlValue::String(binding) => Some(binding.clone()),
            YamlValue::Number(number) => Some(number.to_string()),
            _ => None,
        };
        match short_form {
            Some(binding) if Self::contains_interpolation(&binding) => format!(
                "{WHY}: port \"{binding}\" uses a variable, which cannot be validated; replace \
                 it with a literal loopback binding such as \"127.0.0.1::<container-port>\" \
                 (or remove the host port), {ROUTE}"
            ),
            Some(binding) => {
                // `[host_ip:][host_port:]container_port[/protocol]`: keep only
                // the container side and let Docker pick a free loopback port.
                let container = binding.rsplit(':').next().unwrap_or(&binding);
                format!(
                    "{WHY}: change \"{binding}\" to \"127.0.0.1::{container}\" (or remove the \
                     host port), {ROUTE}"
                )
            }
            None => format!(
                "{WHY}: add `host_ip: 127.0.0.1` to the long-form port entry (or remove \
                 `published` so no fixed host port is claimed), {ROUTE}"
            ),
        }
    }

    /// Whether a Compose service name is safe to interpolate into generated YAML.
    /// Restricts to the Compose spec character set (alphanumeric start, then
    /// letters/digits/`.`/`_`/`-`), rejecting names with newlines, colons, or
    /// spaces that could corrupt the generated security override.
    fn is_valid_service_name(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphanumeric() => {}
            _ => return false,
        }
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }

    /// Parse the byte-value syntax accepted by Compose for `shm_size` while
    /// keeping validation deterministic and interpolation-free. Compose's
    /// conventional k/m/g units are binary multiples; explicit `iB` and `B`
    /// suffixes are accepted as well.
    pub(crate) fn parse_compose_byte_size(value: &YamlValue) -> Option<u64> {
        if let Some(bytes) = value.as_u64() {
            return (bytes > 0).then_some(bytes);
        }
        let raw = value.as_str()?.trim().to_ascii_lowercase();
        let digit_count = raw.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 {
            return None;
        }
        let amount = raw[..digit_count].parse::<u64>().ok()?;
        if amount == 0 {
            return None;
        }
        let multiplier = match raw[digit_count..].trim() {
            "" | "b" => 1,
            "k" | "kb" | "kib" => 1024,
            "m" | "mb" | "mib" => 1024 * 1024,
            "g" | "gb" | "gib" => 1024 * 1024 * 1024,
            _ => return None,
        };
        amount.checked_mul(multiplier)
    }

    /// Service fields whose value (or any nested sequence/mapping value) must
    /// never contain `${...}` / `$(...)` interpolation. An attacker could
    /// otherwise smuggle host/privileged access past the static checks via env
    /// defaults like `network_mode: ${NET:-host}` or `privileged: ${P:-true}`,
    /// because the literal YAML value is an interpolation string rather than
    /// `host`/`true`.
    const INTERPOLATION_GUARDED_FIELDS: &'static [&'static str] = &[
        "privileged",
        "use_api_socket",
        "network_mode",
        "pid",
        "ipc",
        "userns_mode",
        "uts",
        "cgroup",
        "cap_add",
        "devices",
        "volumes",
        "security_opt",
        "group_add",
        "device_cgroup_rules",
        "volumes_from",
        "runtime",
        "oom_kill_disable",
        "shm_size",
        "tmpfs",
        "ulimits",
    ];

    /// Reject `${...}` / `$(...)` interpolation appearing anywhere within a
    /// security-guarded field's value (recursing into sequences and mappings).
    fn reject_interpolation_in_guarded_fields(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        if !self.enforced(PolicyCheck::Interpolation) {
            return Ok(());
        }
        for field in Self::INTERPOLATION_GUARDED_FIELDS {
            let Some(value) = service.get(YamlValue::String((*field).to_string())) else {
                continue;
            };
            if Self::value_contains_interpolation(value) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: (*field).to_string(),
                    reason: format!(
                        "the 'Block variables in guarded fields' security check rejects \
                         '${{...}}' interpolation in guarded field '{field}'; \
                         it can smuggle host/privileged access past static validation"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Recursively check whether a YAML value (string, sequence, or mapping)
    /// contains shell/compose variable interpolation.
    fn value_contains_interpolation(value: &YamlValue) -> bool {
        match value {
            YamlValue::String(s) => Self::contains_interpolation(s),
            YamlValue::Sequence(seq) => seq.iter().any(Self::value_contains_interpolation),
            YamlValue::Mapping(map) => map.values().any(Self::value_contains_interpolation),
            YamlValue::Tagged(value) => Self::value_contains_interpolation(&value.value),
            _ => false,
        }
    }

    fn reject_bool(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
        field: &str,
        rejected: bool,
        reason: &str,
    ) -> Result<(), ComposeError> {
        let Some(value) = service.get(YamlValue::String(field.to_string())) else {
            return Ok(());
        };
        match value.as_bool() {
            Some(value) if value == rejected && self.field_enforced(field) => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: field.to_string(),
                    reason: reason.to_string(),
                });
            }
            Some(_) => {}
            None => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: field.to_string(),
                    reason: format!(
                        "{field} must be a literal boolean; quoted values, interpolation, and other value types are not allowed"
                    ),
                });
            }
        }
        Ok(())
    }

    fn reject_present(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
        field: &str,
        reason: &str,
    ) -> Result<(), ComposeError> {
        if !self.field_enforced(field) {
            return Ok(());
        }
        if service.contains_key(YamlValue::String(field.to_string())) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: reason.to_string(),
            });
        }
        Ok(())
    }

    fn validate_network_mode(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
        declared_services: &yaml_serde::Mapping,
    ) -> Result<(), ComposeError> {
        let Some(value) = service.get(YamlValue::String("network_mode".to_string())) else {
            return Ok(());
        };
        let Some(mode) = value.as_str() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "network_mode".to_string(),
                reason: "network_mode must be omitted, 'none', or 'service:<declared-service>'"
                    .to_string(),
            });
        };
        if (mode == "host" && !self.enforced(PolicyCheck::HostNetwork))
            || (mode.starts_with("container:") && !self.enforced(PolicyCheck::ContainerNamespace))
            || (!matches!(mode, "host")
                && !mode.starts_with("container:")
                && !mode.starts_with("service:")
                && !self.enforced(PolicyCheck::NetworkMode))
        {
            return Ok(());
        }
        if mode == "none" {
            return Ok(());
        }
        if let Some(target) = mode.strip_prefix("service:") {
            if !target.is_empty()
                && declared_services.contains_key(YamlValue::String(target.to_string()))
            {
                return Ok(());
            }
        }

        Err(ComposeError::SecurityPolicyViolation {
            service: service_name.to_string(),
            field: "network_mode".to_string(),
            reason: "network_mode may not join host, default bridge, arbitrary container, or external namespaces; omit it for the project network, use 'none', or share a declared service namespace"
                .to_string(),
        })
    }

    fn reject_host_namespace(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
        field: &str,
    ) -> Result<(), ComposeError> {
        let Some(value) = service.get(YamlValue::String(field.to_string())) else {
            return Ok(());
        };
        let Some(mode) = value.as_str() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: format!("{field} must be a literal namespace mode"),
            });
        };
        let check = match field {
            "pid" => PolicyCheck::HostPid,
            "ipc" => PolicyCheck::HostIpc,
            "uts" => PolicyCheck::HostUts,
            "cgroup" => PolicyCheck::HostCgroup,
            "userns_mode" => PolicyCheck::HostUser,
            _ => PolicyCheck::HostNetwork,
        };
        if mode == "host" && self.enforced(check) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: "host namespace sharing is not allowed for compose deployments".to_string(),
            });
        }
        // `container:<name|id>` joins the namespace of an arbitrary container on
        // the host — including other tenants' and Temps' own infrastructure
        // containers. Only intra-project `service:<name>` sharing is acceptable.
        if mode.starts_with("container:") && self.enforced(PolicyCheck::ContainerNamespace) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: field.to_string(),
                reason: "joining another container's namespace via 'container:' is not allowed; \
                         it can target containers outside this deployment"
                    .to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn socket_source(source: &str) -> bool {
        Path::new(source)
            .file_name()
            .is_some_and(|name| name == "docker.sock")
    }

    /// Bind exceptions do not grant access to the Docker API, including symlinks
    /// or parent directories exposing a known local daemon socket.
    pub(super) fn validate_socket_mount(
        base: &Path,
        source: &str,
        service: &str,
        policy: &ComposeSecurityPolicy,
    ) -> Result<(), ComposeError> {
        if !policy.enforced(PolicyCheck::DockerSocket) {
            return Ok(());
        }
        let original = base.join(source);
        let candidate = std::fs::canonicalize(&original).unwrap_or_else(|_| original.clone());
        let mut sockets = vec![
            PathBuf::from("/var/run/docker.sock"),
            PathBuf::from("/run/docker.sock"),
        ];
        #[allow(
            clippy::disallowed_methods,
            reason = "host lookup: DOCKER_HOST names this host's daemon socket, which a bind mount must not expose"
        )]
        if let Ok(host) = std::env::var("DOCKER_HOST") {
            if let Some(socket) = host.strip_prefix("unix://") {
                sockets.push(PathBuf::from(socket));
            }
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "host lookup: HOME locates per-user Docker/Colima daemon sockets on this host"
        )]
        if let Some(home) = std::env::var_os("HOME") {
            sockets.push(PathBuf::from(&home).join(".docker/run/docker.sock"));
            sockets.push(PathBuf::from(home).join(".colima/default/docker.sock"));
        }
        let exposes_socket = Self::socket_source(source)
            || candidate
                .file_name()
                .is_some_and(|name| name == "docker.sock")
            || sockets.into_iter().any(|socket| {
                let lexical_match = socket.starts_with(&original);
                let socket = std::fs::canonicalize(&socket).unwrap_or(socket);
                lexical_match || socket.starts_with(&candidate)
            });
        if exposes_socket {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(), field: "volumes".to_string(),
                reason: "this mount exposes a Docker daemon socket; the Docker socket policy must also be disabled".to_string(),
            });
        }
        Ok(())
    }

    fn bind_path_enforced(&self, source: &str) -> bool {
        self.enforced(if Self::socket_source(source) {
            PolicyCheck::DockerSocket
        } else {
            PolicyCheck::BindMounts
        })
    }

    fn validate_service_volumes(
        &self,
        service: &yaml_serde::Mapping,
        service_name: &str,
        forbidden_named_volumes: &HashSet<String>,
    ) -> Result<(), ComposeError> {
        let Some(volumes) = service.get(YamlValue::String("volumes".to_string())) else {
            return Ok(());
        };
        let Some(entries) = volumes.as_sequence() else {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service_name.to_string(),
                field: "volumes".to_string(),
                reason: "volumes must be a sequence of validated mount entries".to_string(),
            });
        };

        for entry in entries {
            let long_form_type = if let Some(mapping) = entry.as_mapping() {
                let Some(mount_type) = mapping
                    .get(YamlValue::String("type".to_string()))
                    .and_then(YamlValue::as_str)
                else {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.type".to_string(),
                        reason: "long-form volume mounts must declare a literal type of 'bind' or 'volume'"
                            .to_string(),
                    });
                };
                if mount_type == "tmpfs" && !self.enforced(PolicyCheck::Tmpfs) {
                    continue;
                }
                if Self::contains_interpolation(mount_type)
                    || !matches!(mount_type, "bind" | "volume")
                {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.type".to_string(),
                        reason: format!(
                            "long-form volume mount type '{mount_type}' is not allowed; only literal 'bind' and 'volume' mounts are supported"
                        ),
                    });
                }
                Some(mount_type)
            } else {
                None
            };
            let Some(source) = Self::volume_source(entry) else {
                if long_form_type == Some("bind") {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.source".to_string(),
                        reason: "long-form bind mounts must declare a literal source path"
                            .to_string(),
                    });
                }
                continue;
            };

            // Reject interpolation in bind sources. `${HOST_ROOT:-/}` cannot be
            // statically validated, so a `/`-style check is trivially bypassed.
            if self.enforced(PolicyCheck::Interpolation) && Self::contains_interpolation(&source) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "volumes".to_string(),
                    reason: format!(
                        "the 'Block variables in guarded fields' security check rejects \
                         interpolation in bind mount source '{source}'; \
                         it cannot be statically validated"
                    ),
                });
            }

            // A bare name (no path separators, not relative) is a named volume
            // reference, not a host bind. It is only dangerous if the named
            // volume binds a forbidden host path via driver_opts.
            if long_form_type == Some("volume") {
                if !Self::is_named_volume_ref(&source) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes.source".to_string(),
                        reason: format!(
                            "long-form volume source '{source}' must be a project-scoped named volume"
                        ),
                    });
                }
                if forbidden_named_volumes.contains(&source) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes".to_string(),
                        reason: format!(
                            "named volume '{source}' binds a forbidden host path via driver_opts"
                        ),
                    });
                }
                continue;
            }

            if long_form_type.is_none() && Self::is_named_volume_ref(&source) {
                if forbidden_named_volumes.contains(&source) {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service_name.to_string(),
                        field: "volumes".to_string(),
                        reason: format!(
                            "named volume '{source}' binds a forbidden host path via driver_opts"
                        ),
                    });
                }
                continue;
            }

            // Host bind mount: normalize `..`/`.` and reject absolute host paths
            // outside the sandbox or relative paths that escape the project dir.
            if self.bind_path_enforced(&source) && Self::is_dangerous_host_path(&source) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: service_name.to_string(),
                    field: "volumes".to_string(),
                    reason: format!("host bind mount source '{source}' is not allowed"),
                });
            }
        }

        Ok(())
    }

    /// The host side of a volume entry, or `None` when the entry has none.
    ///
    /// Short syntax with no `:` (`- /app/node_modules`) is an *anonymous
    /// volume*: the single path is the mount point inside the container and
    /// Docker creates a fresh volume for it. There is no host path to
    /// validate, so it is reported as sourceless — exactly like a long-form
    /// `type: volume` entry without `source`. The one exception is an entry
    /// containing interpolation: `${MOUNT}` can expand to `/etc:/etc`, so it
    /// is returned as-is for the interpolation guard to reject.
    pub(super) fn volume_source(entry: &YamlValue) -> Option<String> {
        if let Some(value) = entry.as_str() {
            if !value.contains(':') && !Self::contains_interpolation(value) {
                return None;
            }
            return value.split(':').next().map(str::to_string);
        }

        let mapping = entry.as_mapping()?;
        mapping
            .get(YamlValue::String("source".to_string()))
            .and_then(YamlValue::as_str)
            .map(str::to_string)
    }

    pub(super) fn is_explicit_bind_mount(entry: &YamlValue) -> bool {
        entry
            .as_mapping()
            .and_then(|mapping| mapping.get(YamlValue::String("type".to_string())))
            .and_then(YamlValue::as_str)
            == Some("bind")
    }

    /// Whether a string contains compose/shell variable interpolation.
    ///
    /// Docker Compose interpolates `${VAR}`, `$(cmd)`, AND the braceless `$VAR`
    /// form; `$$` is an escaped literal dollar. Matching only `${`/`$(` let
    /// `network_mode: $NET` or `volumes: [$SRC:/host]` slip past the guard and
    /// resolve to attacker-controlled values from the repo `.env` at runtime,
    /// so treat any real `$` sigil as interpolation.
    pub(super) fn contains_interpolation(value: &str) -> bool {
        let bytes = value.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                match bytes.get(i + 1).copied() {
                    // `$$` escapes a literal dollar — not interpolation.
                    Some(b'$') => {
                        i += 2;
                        continue;
                    }
                    // `${VAR}` / `$(cmd)` / `$VAR` are all interpolation.
                    Some(b'{') | Some(b'(') => return true,
                    Some(c) if c.is_ascii_alphabetic() || c == b'_' => return true,
                    _ => {}
                }
            }
            i += 1;
        }
        false
    }

    /// A bare volume name (no path separators and not relative) references a
    /// named volume rather than a host bind path.
    pub(super) fn is_named_volume_ref(source: &str) -> bool {
        !source.contains('/')
            && !source.contains('\\')
            && !source.starts_with('.')
            && !source.starts_with('~')
            && !source.is_empty()
    }

    /// Whether a host path is dangerous: it interpolates, is any absolute host
    /// path, or escapes the compose project directory via `..`. Paths are
    /// normalized lexically first so `../../etc` and `/tmp/../etc` cannot bypass
    /// the block.
    ///
    /// Bind sources in user compose must be relative to the per-project working
    /// directory (compose runs with `current_dir(project_dir)`). Absolute host
    /// paths are rejected unconditionally — there is no allowed absolute prefix,
    /// because a world-writable location like `/tmp` can hold other tenants'
    /// project artifacts (`.env.temps`, encryption keys) when the data dir lives
    /// under it, and shared host paths are exactly the escape this guard exists
    /// to prevent.
    pub(super) fn is_dangerous_host_path(source: &str) -> bool {
        if Self::contains_interpolation(source) {
            return true;
        }
        // Docker Compose expands home-relative paths before invoking Docker.
        // Treat both Unix and Windows separators as host-path escapes instead
        // of validating them as a literal directory beneath the checkout.
        if source.starts_with('~') {
            return true;
        }
        let normalized = Self::lexically_normalize(source);
        // Relative path that climbs above the project directory.
        if normalized == ".." || normalized.starts_with("../") {
            return true;
        }
        // Any absolute host path.
        if normalized.starts_with('/') {
            return true;
        }
        false
    }

    /// Structural allow-list for inline compose overrides. Complements the
    /// value-level `validate_compose_security_policy`: an inline override may
    /// only modify services that already exist in the base compose file, may not
    /// introduce top-level keys other than `services`, and may not use
    /// host-affecting service keys (privileged, network_mode, volumes, ...).
    #[cfg(test)]
    fn validate_compose_override(
        project_name: &str,
        compose_content: &str,
        override_content: &str,
    ) -> Result<(), ComposeError> {
        Self::validate_compose_override_with_policy(
            project_name,
            compose_content,
            override_content,
            &ComposeSecurityPolicy::default(),
        )
    }

    pub(super) fn validate_compose_override_with_policy(
        project_name: &str,
        compose_content: &str,
        override_content: &str,
        policy: &ComposeSecurityPolicy,
    ) -> Result<(), ComposeError> {
        let base = Self::parse_compose_yaml(project_name, compose_content, "compose file")?;
        let override_yaml =
            Self::parse_compose_yaml(project_name, override_content, "compose override")?;

        let empty_services = Mapping::new();
        let base_services = match Self::compose_services(&base) {
            Some(services) => services,
            None if !policy.enforced(PolicyCheck::InlineServices) && base.get("services").is_none() => &empty_services,
            None => return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: "base compose file must define a services mapping before an inline override can be applied".to_string(),
            }),
        };

        let Some(override_root) = override_yaml.as_mapping() else {
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: "inline compose override must be a mapping".to_string(),
            });
        };
        for key in override_root.keys().filter_map(Self::yaml_key) {
            if policy.enforced(PolicyCheck::InlineSections)
                && key != "services"
                && !(key == "include" && !policy.enforced(PolicyCheck::Include))
            {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "inline compose override cannot set top-level key '{key}'; an instance administrator can disable the Inline Sections check in Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings for a trusted stack"
                    ),
                });
            }
        }

        let Some(override_services) = Self::compose_services(&override_yaml) else {
            if override_yaml.get("include").is_some() && !policy.enforced(PolicyCheck::Include) {
                return Ok(());
            }
            if !policy.enforced(PolicyCheck::InlineSections)
                && override_yaml.get("services").is_none()
            {
                return Ok(());
            }
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason:
                    "inline compose override must define only service-level changes under services"
                        .to_string(),
            });
        };

        let base_service_names: HashSet<String> =
            base_services.keys().filter_map(Self::yaml_key).collect();
        for (service_name_value, service_config) in override_services {
            let service_name = Self::yaml_key(service_name_value).ok_or_else(|| {
                ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: "service names in inline compose override must be strings".to_string(),
                }
            })?;

            if policy.enforced(PolicyCheck::InlineServices)
                && !base_service_names.contains(&service_name)
            {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "inline compose override cannot add service '{service_name}'; add it to the repository compose file, or an instance administrator can disable the Inline Services check in Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings for a trusted stack"
                    ),
                });
            }

            Self::validate_override_service(project_name, &service_name, service_config, policy)?;
        }

        Ok(())
    }

    /// Compose re-applies an inherited service when the same `extends` appears
    /// in both input files. That can restore fields the repository file removed
    /// with `!override`, such as an inherited `depends_on`. The repeated
    /// reference adds no intended setting, so omit it from the inline file.
    pub(super) fn without_duplicate_extends(
        project_name: &str,
        compose_content: &str,
        override_content: &str,
    ) -> Result<String, ComposeError> {
        let base = Self::parse_compose_yaml(project_name, compose_content, "compose file")?;
        let mut override_yaml =
            Self::parse_compose_yaml(project_name, override_content, "compose override")?;
        let Some(base_services) = Self::compose_services(&base) else {
            return Ok(override_content.to_string());
        };
        let Some(override_services) = override_yaml
            .get_mut("services")
            .and_then(Value::as_mapping_mut)
        else {
            return Ok(override_content.to_string());
        };
        let mut changed = false;
        for (name, service) in override_services {
            let Some(base_extends) = base_services.get(name).and_then(|base| base.get("extends"))
            else {
                continue;
            };
            let Some(fields) = service.as_mapping_mut() else {
                continue;
            };
            let extends = Value::String("extends".to_string());
            if fields.get(&extends) == Some(base_extends) {
                fields.remove(&extends);
                changed = true;
            }
        }
        if !changed {
            return Ok(override_content.to_string());
        }
        yaml_serde::to_string(&override_yaml).map_err(|error| ComposeError::InvalidOverride {
            project: project_name.to_string(),
            reason: format!("failed to serialize inline override after merging extends: {error}"),
        })
    }

    pub(super) fn port_reset_override(
        project_name: &str,
        override_content: &str,
    ) -> Result<Option<String>, ComposeError> {
        let override_yaml =
            Self::parse_compose_yaml(project_name, override_content, "compose override")?;
        let Some(services) = Self::compose_services(&override_yaml) else {
            return Ok(None);
        };
        let mut reset_services = Mapping::new();
        for (name, service) in services {
            if service.get("ports").is_none() {
                continue;
            }
            let mut reset = Mapping::new();
            reset.insert(
                Value::String("ports".to_string()),
                Value::Tagged(Box::new(TaggedValue {
                    tag: Tag::new("!reset"),
                    value: Value::Sequence(Vec::new()),
                })),
            );
            reset_services.insert(name.clone(), Value::Mapping(reset));
        }
        if reset_services.is_empty() {
            return Ok(None);
        }
        let mut root = Mapping::new();
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(reset_services),
        );
        yaml_serde::to_string(&Value::Mapping(root))
            .map(Some)
            .map_err(|error| ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("failed to serialize port reset layer: {error}"),
            })
    }

    fn parse_compose_yaml(
        project_name: &str,
        content: &str,
        label: &str,
    ) -> Result<Value, ComposeError> {
        let mut compose =
            yaml_serde::from_str::<Value>(content).map_err(|e| ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("failed to parse {label} YAML: {e}"),
            })?;
        compose
            .apply_merge()
            .map_err(|e| ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("failed to expand YAML merge keys in {label}: {e}"),
            })?;
        Ok(compose)
    }

    fn compose_services(compose: &Value) -> Option<&Mapping> {
        compose
            .as_mapping()?
            .get(Value::String("services".to_string()))?
            .as_mapping()
    }

    fn yaml_key(value: &Value) -> Option<String> {
        value.as_str().map(ToString::to_string)
    }

    fn validate_override_service(
        project_name: &str,
        service_name: &str,
        service_config: &Value,
        policy: &ComposeSecurityPolicy,
    ) -> Result<(), ComposeError> {
        let Some(service) = service_config.as_mapping() else {
            return Err(ComposeError::InvalidOverride {
                project: project_name.to_string(),
                reason: format!("service '{service_name}' override must be a mapping"),
            });
        };

        for key in service.keys().filter_map(Self::yaml_key) {
            if NEVER_ALLOWED_OVERRIDE_KEYS.contains(&key.as_str()) {
                let check = PolicyCheck::for_restricted_service_field(&key)
                    .unwrap_or(PolicyCheck::InlineFields);
                if policy.enforced(check) {
                    return Err(ComposeError::InvalidOverride {
                        project: project_name.to_string(),
                        reason: format!("service '{service_name}' uses '{key}', which the {check:?} check rejects; an instance administrator can disable that check in Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings for a trusted stack"),
                    });
                }
            }
            if policy.enforced(PolicyCheck::InlineFields)
                && REPO_ONLY_OVERRIDE_KEYS.contains(&key.as_str())
            {
                return Err(ComposeError::InvalidOverride {
                    project: project_name.to_string(),
                    reason: format!(
                        "service '{service_name}' cannot set '{key}' as an inline override; \
                         declare it in the repository compose file, or an instance administrator \
                         can disable the Inline Fields check in Project Settings → Build & deploy → \
                         Build → Docker Compose → Advanced security settings for a trusted stack"
                    ),
                });
            }
        }

        Ok(())
    }

    /// Check if a compose file contains build: directives (services that need building)
    pub(super) fn has_build_directives(&self, compose_content: &str) -> bool {
        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed == "build:" || trimmed.starts_with("build:") {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests;
