// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

pub struct DockerComposePreset;

#[async_trait]
impl Preset for DockerComposePreset {
    fn slug(&self) -> String {
        "docker-compose".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Docker Compose".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/docker.svg".to_string()
    }

    fn description(&self) -> String {
        "Deploy a multi-container application using Docker Compose".to_string()
    }

    async fn dockerfile(&self, _config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Docker Compose doesn't use a Dockerfile — it manages its own images.
        // Return a placeholder. The deployment pipeline skips the build step
        // for this preset and uses `docker compose up` instead.
        DockerfileWithArgs::new("# Docker Compose preset — no Dockerfile needed".to_string())
    }

    async fn dockerfile_with_build_dir(&self, _local_path: &Path) -> DockerfileWithArgs {
        DockerfileWithArgs::new("# Docker Compose preset — no Dockerfile needed".to_string())
    }

    fn install_command(&self, local_path: &Path) -> String {
        PackageManager::detect(local_path)
            .install_command()
            .to_string()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        // No build step — compose pulls its own images
        String::new()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec![".".to_string()]
    }

    fn default_port(&self) -> u16 {
        // No single default port — compose has multiple services
        0
    }
}

impl fmt::Display for DockerComposePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Common Docker Compose file names to detect
pub const COMPOSE_FILE_NAMES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

/// A single service parsed out of a compose file's `services:` map, for
/// showing the user what they're about to deploy before project creation
/// (e.g. so they can exclude an unmanaged database service from the stack).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeServicePreview {
    pub name: String,
    pub image: Option<String>,
    pub depends_on: Vec<String>,
    /// Environment variable names declared by this service. Values are never
    /// returned: this is discovery metadata for settings/onboarding only.
    pub environment_variables: Vec<String>,
    /// True when the service's image matches a well-known database engine
    /// family (Postgres/MySQL/MariaDB/MongoDB/Redis and their common forks),
    /// by the same registry/tag-stripped basename match the CapRover/
    /// Portainer/Kubernetes importers already use in `detect_database()`.
    /// A raw compose service never becomes a Temps-managed `external_services`
    /// row, so it never gets backup/restore — this is purely an informational
    /// nudge, not a restriction.
    pub looks_like_database: bool,
    /// The specific well-known service family the image matches, if any —
    /// see [`temps_entities::preset::ComposeServiceFamily`]. A superset of
    /// `looks_like_database`: it also covers S3-compatible storage (MinIO),
    /// which isn't part of the capabilities-warning-relevant database set.
    pub detected_service_type: Option<temps_entities::preset::ComposeServiceFamily>,
    /// Compose port mappings. `target` is the container port Temps should
    /// proxy to; `published` is the optional Docker host port.
    pub ports: Vec<temps_entities::preset::ComposePortMapping>,
    /// HTTP path declared by a loopback curl/wget Compose healthcheck. Temps
    /// can reuse this path for deployment readiness and the public uptime
    /// monitor without assuming that `/` is a valid application route.
    pub health_check_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeParseError {
    #[error("Compose file is not valid YAML: {reason}")]
    InvalidYaml { reason: String },
    #[error("Compose file has no top-level 'services:' mapping")]
    MissingServices,
    #[error("Compose override is invalid: {reason}")]
    InvalidOverride { reason: String },
    #[error("Compose preview could not be serialized: {reason}")]
    Serialization { reason: String },
}

/// Redacted effective Compose document shown in project settings. Values from
/// service environments and build arguments are never returned to the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveComposePreview {
    pub yaml: String,
    pub enabled_services: Vec<String>,
    pub disabled_services: Vec<String>,
    pub redacted_values: usize,
}

/// Parses a compose file's `services:` map into a preview list (name, image,
/// `depends_on`, and a "looks like a database" hint). Read-only — never
/// mutates or re-serializes the input, unlike `strip_excluded_services` in
/// `temps-deployer`, which needs a full rewrite at deploy time.
pub fn list_compose_services(yaml: &str) -> Result<Vec<ComposeServicePreview>, ComposeParseError> {
    let mut root: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| ComposeParseError::InvalidYaml {
            reason: e.to_string(),
        })?;
    root.apply_merge()
        .map_err(|e| ComposeParseError::InvalidYaml {
            reason: e.to_string(),
        })?;

    let services = root
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or(ComposeParseError::MissingServices)?;

    let mut previews = Vec::with_capacity(services.len());
    for (key, value) in services {
        let Some(name) = key.as_str() else { continue };
        let service = value.as_mapping();

        let image = service
            .and_then(|s| s.get("image"))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);

        let depends_on = service
            .and_then(|s| s.get("depends_on"))
            .map(depends_on_names)
            .unwrap_or_default();

        let environment_variables = service
            .and_then(|s| s.get("environment"))
            .map(environment_variable_names)
            .unwrap_or_default();

        let ports = service
            .and_then(|s| s.get("ports"))
            .map(compose_port_mappings)
            .unwrap_or_default();

        let health_check_path = service
            .and_then(|s| s.get("healthcheck"))
            .and_then(http_healthcheck_path);

        let detected_service_type = image.as_deref().and_then(detect_service_family);
        let looks_like_database = image
            .as_deref()
            .map(looks_like_database_image)
            .unwrap_or(false);

        previews.push(ComposeServicePreview {
            name: name.to_string(),
            image,
            depends_on,
            environment_variables,
            looks_like_database,
            detected_service_type,
            ports,
            health_check_path,
        });
    }

    Ok(previews)
}

/// Extract one unambiguous HTTP path from a Compose healthcheck that probes a
/// loopback address. We intentionally ignore remote hosts, non-HTTP commands,
/// and checks with multiple different paths.
fn http_healthcheck_path(healthcheck: &serde_yaml::Value) -> Option<String> {
    let mut paths = std::collections::BTreeSet::new();
    visit_healthcheck_strings(healthcheck, &mut |text| {
        for scheme in ["http://", "https://"] {
            let mut remainder = text;
            while let Some(index) = remainder.find(scheme) {
                let candidate_with_tail = &remainder[index..];
                let end = candidate_with_tail
                    .find(|character: char| {
                        character.is_whitespace()
                            || matches!(character, '\'' | '"' | ')' | ']' | ';' | '|' | '&')
                    })
                    .unwrap_or(candidate_with_tail.len());
                let candidate = &candidate_with_tail[..end];
                if let Ok(url) = url::Url::parse(candidate) {
                    let loopback = match url.host() {
                        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                        Some(url::Host::Ipv4(address)) => {
                            address.is_loopback() || address.is_unspecified()
                        }
                        Some(url::Host::Ipv6(address)) => address.is_loopback(),
                        None => false,
                    };
                    if loopback && url.fragment().is_none() {
                        let mut path = url.path().to_string();
                        if let Some(query) = url.query() {
                            path.push('?');
                            path.push_str(query);
                        }
                        if path.starts_with('/') && path.len() <= 2048 {
                            paths.insert(path);
                        }
                    }
                }
                remainder = &candidate_with_tail[end..];
            }
        }
    });
    (paths.len() == 1)
        .then(|| paths.into_iter().next())
        .flatten()
}

fn visit_healthcheck_strings(value: &serde_yaml::Value, visitor: &mut impl FnMut(&str)) {
    match value {
        serde_yaml::Value::String(text) => visitor(text),
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                visit_healthcheck_strings(value, visitor);
            }
        }
        serde_yaml::Value::Mapping(values) => {
            for (key, value) in values {
                visit_healthcheck_strings(key, visitor);
                visit_healthcheck_strings(value, visitor);
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            visit_healthcheck_strings(&tagged.value, visitor);
        }
        serde_yaml::Value::Null | serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {}
    }
}

/// Combine discovery metadata from a repository Compose file and an optional
/// override. For route suggestions we deliberately retain every declared
/// target port from both layers: Docker Compose may replace a published port,
/// but the proxy always cares about the stable container-side target.
pub fn list_compose_services_with_override(
    yaml: &str,
    override_yaml: Option<&str>,
) -> Result<Vec<ComposeServicePreview>, ComposeParseError> {
    let mut services = list_compose_services(yaml)?;
    let Some(override_yaml) = override_yaml.filter(|value| !value.trim().is_empty()) else {
        return Ok(services);
    };

    for override_service in list_compose_services(override_yaml)? {
        if let Some(base) = services
            .iter_mut()
            .find(|service| service.name == override_service.name)
        {
            if override_service.image.is_some() {
                base.image = override_service.image;
                base.looks_like_database = override_service.looks_like_database;
                base.detected_service_type = override_service.detected_service_type;
            }
            base.depends_on.extend(override_service.depends_on);
            base.depends_on.sort();
            base.depends_on.dedup();
            base.environment_variables
                .extend(override_service.environment_variables);
            base.environment_variables.sort();
            base.environment_variables.dedup();
            base.ports.extend(override_service.ports);
            base.ports
                .sort_by_key(|port| (port.target, port.published, port.protocol.clone()));
            base.ports.dedup();
            if override_service.health_check_path.is_some() {
                base.health_check_path = override_service.health_check_path;
            }
        } else {
            services.push(override_service);
        }
    }

    Ok(services)
}

/// Render the user-controlled Compose document Temps will hand to the deploy
/// pipeline after disabled services are removed and the inline override is
/// applied. Platform-owned security, network, labels, and runtime environment
/// layers are intentionally not included; those are injected later and cannot
/// be edited by the user.
///
/// The returned YAML is safe for a browser preview: every service environment
/// value and build argument value is replaced before serialization.
pub fn render_effective_compose_preview(
    yaml: &str,
    override_yaml: Option<&str>,
    excluded_services: &[String],
) -> Result<EffectiveComposePreview, ComposeParseError> {
    let mut base = parse_compose_document(yaml, "Compose file")?;
    strip_excluded_services_value(&mut base, excluded_services);
    normalize_sensitive_maps(&mut base);

    if let Some(override_yaml) = override_yaml.filter(|value| !value.trim().is_empty()) {
        let mut override_document = parse_compose_document(override_yaml, "Compose override")?;
        validate_preview_override(&base, &override_document)?;
        normalize_sensitive_maps(&mut override_document);
        merge_compose_value(&mut base, override_document, &[]);
    }

    let enabled_services = service_names(&base);
    let mut disabled_services = excluded_services.to_vec();
    disabled_services.sort();
    disabled_services.dedup();
    let redacted_values = redact_sensitive_values(&mut base);
    normalize_empty_named_resources(&mut base);
    let yaml = serde_yaml::to_string(&base).map_err(|error| ComposeParseError::Serialization {
        reason: error.to_string(),
    })?;

    Ok(EffectiveComposePreview {
        yaml,
        enabled_services,
        disabled_services,
        redacted_values,
    })
}

/// YAML permits an empty mapping value to be written as `name:`, which parses
/// as null. Compose treats that as a default named-resource declaration, but
/// serializing it back as `name: null` is misleading in an effective manifest.
/// Use the explicit `{}` form for every top-level named resource instead.
fn normalize_empty_named_resources(root: &mut serde_yaml::Value) {
    let Some(root) = root.as_mapping_mut() else {
        return;
    };
    for section_name in ["volumes", "networks", "configs", "secrets"] {
        let Some(section) = root
            .get_mut(section_name)
            .and_then(serde_yaml::Value::as_mapping_mut)
        else {
            continue;
        };
        for definition in section.values_mut() {
            if definition.is_null() {
                *definition = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
            }
        }
    }
}

fn parse_compose_document(
    yaml: &str,
    source: &str,
) -> Result<serde_yaml::Value, ComposeParseError> {
    let mut root: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| ComposeParseError::InvalidYaml {
            reason: format!("{source}: {error}"),
        })?;
    root.apply_merge()
        .map_err(|error| ComposeParseError::InvalidYaml {
            reason: format!("{source}: failed to expand YAML merge keys: {error}"),
        })?;
    if root
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .is_none()
    {
        return Err(ComposeParseError::MissingServices);
    }
    Ok(root)
}

fn service_names(root: &serde_yaml::Value) -> Vec<String> {
    let mut names: Vec<_> = root
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .into_iter()
        .flat_map(|services| services.keys())
        .filter_map(serde_yaml::Value::as_str)
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

fn strip_excluded_services_value(root: &mut serde_yaml::Value, excluded: &[String]) {
    let Some(services) = root
        .get_mut("services")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return;
    };

    for name in excluded {
        services.remove(name.as_str());
    }
    for service in services.values_mut() {
        let Some(depends_on) = service
            .as_mapping_mut()
            .and_then(|mapping| mapping.get_mut("depends_on"))
        else {
            continue;
        };
        if let Some(sequence) = depends_on.as_sequence_mut() {
            sequence.retain(|entry| {
                entry
                    .as_str()
                    .is_none_or(|name| !excluded.iter().any(|excluded| excluded == name))
            });
        } else if let Some(mapping) = depends_on.as_mapping_mut() {
            for name in excluded {
                mapping.remove(name.as_str());
            }
        }
    }
}

/// Keys an inline override may not set because the deploy-time security
/// policy (`temps-deployer::compose::validate_compose_security_policy`)
/// rejects them outright, wherever they appear. Telling someone to move one of
/// these into the repository Compose file would be sending them to do work
/// that fails later, at deploy, with a different error.
const NEVER_ALLOWED_SERVICE_KEYS: &[&str] = &[
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

/// Keys an inline override may not set, but which the repository Compose file
/// legitimately can — the deploy-time policy admits them subject to its own
/// checks (bind-mount path confinement for `volumes`, byte caps for
/// `shm_size`, host-namespace rejection for `pid`/`ipc`/`network_mode` and
/// friends). Kept separate from [`NEVER_ALLOWED_SERVICE_KEYS`] so the error
/// can point somewhere that actually works instead of issuing the same
/// blanket "move it to the repository" advice for every key.
const REPO_ONLY_SERVICE_KEYS: &[&str] = &[
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
    "image",
    "env_file",
];

fn validate_preview_override(
    base: &serde_yaml::Value,
    override_document: &serde_yaml::Value,
) -> Result<(), ComposeParseError> {
    let Some(root) = override_document.as_mapping() else {
        return Err(ComposeParseError::InvalidOverride {
            reason: "override must be a YAML mapping".to_string(),
        });
    };
    if root.keys().any(|key| key.as_str() != Some("services")) {
        return Err(ComposeParseError::InvalidOverride {
            reason: "only service-level changes under 'services' are allowed".to_string(),
        });
    }
    let Some(override_services) = override_document
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Err(ComposeParseError::InvalidOverride {
            reason: "override must define a 'services' mapping".to_string(),
        });
    };
    let enabled: BTreeSet<_> = service_names(base).into_iter().collect();
    for name in override_services
        .keys()
        .filter_map(serde_yaml::Value::as_str)
    {
        if !enabled.contains(name) {
            return Err(ComposeParseError::InvalidOverride {
                reason: format!(
                    "service '{name}' is disabled or is not present in the repository Compose file"
                ),
            });
        }

        let Some(service) = override_services
            .get(serde_yaml::Value::String(name.to_string()))
            .and_then(serde_yaml::Value::as_mapping)
        else {
            return Err(ComposeParseError::InvalidOverride {
                reason: format!("service '{name}' override must be a mapping"),
            });
        };
        if let Some(key) = service
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .find(|key| NEVER_ALLOWED_SERVICE_KEYS.contains(key))
        {
            return Err(ComposeParseError::InvalidOverride {
                reason: format!(
                    "service '{name}' uses forbidden key '{key}', which Compose deployments \
                     do not permit anywhere — the deploy-time security policy rejects it in \
                     the repository Compose file too, so moving it there will not help"
                ),
            });
        }
        if let Some(key) = service
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .find(|key| REPO_ONLY_SERVICE_KEYS.contains(key))
        {
            return Err(ComposeParseError::InvalidOverride {
                reason: format!(
                    "service '{name}' cannot set '{key}' as an inline override; declare it in \
                     the repository Compose file instead, where it is checked against the \
                     deployment security policy (host paths, host namespaces and resource \
                     limits are still rejected there)"
                ),
            });
        }
    }
    Ok(())
}

fn normalize_sensitive_maps(root: &mut serde_yaml::Value) {
    let Some(services) = root
        .get_mut("services")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return;
    };
    for service in services.values_mut() {
        let Some(service) = service.as_mapping_mut() else {
            continue;
        };
        if let Some(environment) = service.get_mut("environment") {
            normalize_key_value_sequence(environment);
        }
        if let Some(args) = service
            .get_mut("build")
            .and_then(serde_yaml::Value::as_mapping_mut)
            .and_then(|build| build.get_mut("args"))
        {
            normalize_key_value_sequence(args);
        }
    }
}

fn normalize_key_value_sequence(value: &mut serde_yaml::Value) {
    let Some(sequence) = value.as_sequence() else {
        return;
    };
    let mut mapping = serde_yaml::Mapping::new();
    for entry in sequence {
        let Some(entry) = entry.as_str() else {
            continue;
        };
        let (key, value) = entry
            .split_once('=')
            .map_or((entry, serde_yaml::Value::Null), |(key, value)| {
                (key, serde_yaml::Value::String(value.to_string()))
            });
        if !key.trim().is_empty() {
            mapping.insert(serde_yaml::Value::String(key.trim().to_string()), value);
        }
    }
    *value = serde_yaml::Value::Mapping(mapping);
}

fn merge_compose_value(
    base: &mut serde_yaml::Value,
    override_value: serde_yaml::Value,
    path: &[String],
) {
    match (base, override_value) {
        (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(override_mapping)) => {
            for (key, value) in override_mapping {
                let key_name = key.as_str().unwrap_or_default().to_string();
                let mut child_path = path.to_vec();
                child_path.push(key_name);
                if let Some(existing) = base.get_mut(&key) {
                    merge_compose_value(existing, value, &child_path);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (
            base @ serde_yaml::Value::Sequence(_),
            serde_yaml::Value::Sequence(mut override_items),
        ) => {
            if compose_sequence_replaces(path) {
                *base = serde_yaml::Value::Sequence(override_items);
            } else if let serde_yaml::Value::Sequence(base_items) = base {
                base_items.append(&mut override_items);
            }
        }
        (base, override_value) => *base = override_value,
    }
}

fn compose_sequence_replaces(path: &[String]) -> bool {
    let Some(field) = path.last().map(String::as_str) else {
        return false;
    };
    field == "ports"
        || field == "command"
        || field == "entrypoint"
        || (field == "test" && path.iter().any(|part| part == "healthcheck"))
}

fn redact_sensitive_values(root: &mut serde_yaml::Value) -> usize {
    let Some(services) = root
        .get_mut("services")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return 0;
    };
    let mut count = 0;
    for service in services.values_mut() {
        let Some(service) = service.as_mapping_mut() else {
            continue;
        };
        if let Some(environment) = service
            .get_mut("environment")
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            for value in environment.values_mut() {
                *value = serde_yaml::Value::String("<redacted>".to_string());
                count += 1;
            }
        }
        if let Some(args) = service
            .get_mut("build")
            .and_then(serde_yaml::Value::as_mapping_mut)
            .and_then(|build| build.get_mut("args"))
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            for value in args.values_mut() {
                *value = serde_yaml::Value::String("<redacted>".to_string());
                count += 1;
            }
        }

        // Commands and health checks frequently embed credentials directly
        // (for example `curl -H 'Authorization: Bearer ...'`). They are useful
        // deployment details, but not safe to send back to a browser preview.
        for field in ["command", "entrypoint"] {
            if let Some(value) = service.get_mut(field) {
                count += redact_value(value);
            }
        }
        if let Some(test) = service
            .get_mut("healthcheck")
            .and_then(serde_yaml::Value::as_mapping_mut)
            .and_then(|healthcheck| healthcheck.get_mut("test"))
        {
            count += redact_value(test);
        }

        // Compose labels are arbitrary key/value data and are commonly used
        // for proxy credentials and middleware secrets. Preserve label names
        // so the effective structure remains inspectable, but hide values.
        if let Some(labels) = service.get_mut("labels") {
            count += redact_label_values(labels);
        }

        count += redact_secret_named_mapping(service);
    }

    // Top-level config `content` is literal file content. It can contain full
    // application configuration and credentials, so never expose it.
    if let Some(configs) = root
        .get_mut("configs")
        .and_then(serde_yaml::Value::as_mapping_mut)
    {
        for config in configs.values_mut() {
            if let Some(content) = config
                .as_mapping_mut()
                .and_then(|mapping| mapping.get_mut("content"))
            {
                count += redact_value(content);
            }
        }
    }
    count
}

fn redact_value(value: &mut serde_yaml::Value) -> usize {
    if value.as_str() == Some("<redacted>") || value.is_null() {
        return 0;
    }
    *value = serde_yaml::Value::String("<redacted>".to_string());
    1
}

fn redact_label_values(labels: &mut serde_yaml::Value) -> usize {
    match labels {
        serde_yaml::Value::Mapping(mapping) => mapping.values_mut().map(redact_value).sum(),
        serde_yaml::Value::Sequence(items) => items.iter_mut().map(redact_value).sum(),
        _ => redact_value(labels),
    }
}

fn redact_secret_named_scalars(value: &mut serde_yaml::Value) -> usize {
    match value {
        serde_yaml::Value::Mapping(mapping) => redact_secret_named_mapping(mapping),
        serde_yaml::Value::Sequence(items) => {
            items.iter_mut().map(redact_secret_named_scalars).sum()
        }
        _ => 0,
    }
}

fn redact_secret_named_mapping(mapping: &mut serde_yaml::Mapping) -> usize {
    let mut count = 0;
    for (key, value) in mapping {
        let key = key.as_str().unwrap_or_default();
        if is_secret_key_name(key) && !value.is_mapping() && !value.is_sequence() {
            count += redact_value(value);
        } else {
            count += redact_secret_named_scalars(value);
        }
    }
    count
}

fn is_secret_key_name(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "password",
        "passwd",
        "token",
        "apikey",
        "accesstoken",
        "refreshtoken",
        "clientsecret",
        "privatekey",
        "secretkey",
        "credential",
        "authorization",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
}

fn compose_port_mappings(
    value: &serde_yaml::Value,
) -> Vec<temps_entities::preset::ComposePortMapping> {
    let Some(entries) = value.as_sequence() else {
        return Vec::new();
    };

    let mut ports: Vec<_> = entries.iter().filter_map(compose_port_mapping).collect();
    ports.sort_by_key(|port| (port.target, port.published, port.protocol.clone()));
    ports.dedup();
    ports
}

fn compose_port_mapping(
    value: &serde_yaml::Value,
) -> Option<temps_entities::preset::ComposePortMapping> {
    use temps_entities::preset::ComposePortMapping;

    if let Some(port) = value.as_u64().and_then(|port| u16::try_from(port).ok()) {
        return Some(ComposePortMapping {
            target: port,
            published: None,
            protocol: "tcp".to_string(),
        });
    }

    if let Some(raw) = value.as_str() {
        let (without_protocol, protocol) = raw
            .rsplit_once('/')
            .map_or((raw, "tcp"), |(ports, protocol)| (ports, protocol));
        let segments: Vec<_> = without_protocol.split(':').collect();
        let target = segments.last()?.trim().parse::<u16>().ok()?;
        let published = (segments.len() >= 2)
            .then(|| segments[segments.len() - 2].trim().parse::<u16>().ok())
            .flatten();
        return Some(ComposePortMapping {
            target,
            published,
            protocol: protocol.to_string(),
        });
    }

    let mapping = value.as_mapping()?;
    let target = yaml_u16(mapping.get("target")?)?;
    let published = mapping.get("published").and_then(yaml_u16);
    let protocol = mapping
        .get("protocol")
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("tcp")
        .to_string();
    Some(ComposePortMapping {
        target,
        published,
        protocol,
    })
}

fn yaml_u16(value: &serde_yaml::Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .or_else(|| value.as_str()?.parse::<u16>().ok())
}

/// Extract Compose `environment:` keys from both supported forms:
///
/// - mapping: `DATABASE_URL: ${DATABASE_URL}`
/// - sequence: `- DATABASE_URL` or `- DATABASE_URL=value`
///
/// A sorted set makes the API deterministic and prevents duplicate rows when
/// malformed/hand-written sequence input repeats a key.
fn environment_variable_names(value: &serde_yaml::Value) -> Vec<String> {
    let mut names = BTreeSet::new();
    if let Some(map) = value.as_mapping() {
        names.extend(
            map.keys()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string),
        );
    } else if let Some(sequence) = value.as_sequence() {
        names.extend(
            sequence
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .filter_map(|entry| {
                    let key = entry.split_once('=').map_or(entry, |(key, _)| key).trim();
                    (!key.is_empty()).then(|| key.to_string())
                }),
        );
    }
    names.into_iter().collect()
}

/// Reads service names out of a `depends_on:` value, which per the Compose
/// spec is either a plain list of service names or a map of service name to
/// `{ condition: ... }`.
fn depends_on_names(value: &serde_yaml::Value) -> Vec<String> {
    if let Some(seq) = value.as_sequence() {
        return seq
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .map(str::to_string)
            .collect();
    }
    if let Some(map) = value.as_mapping() {
        return map
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .map(str::to_string)
            .collect();
    }
    Vec::new()
}

/// Same "is this a database image" heuristic as `detect_database()` in the
/// CapRover/Portainer/Kubernetes importers (`crates/temps-import-*`): strip
/// the registry and tag, then match the image basename against known engine
/// families. Duplicated locally rather than shared — it's ~15 lines and this
/// crate has no existing dependency relationship with the importer crates
/// worth introducing for it.
fn detect_service_family(image: &str) -> Option<temps_entities::preset::ComposeServiceFamily> {
    use temps_entities::preset::ComposeServiceFamily::*;
    let without_tag = image.split(':').next().unwrap_or(image);
    let basename = without_tag.rsplit('/').next().unwrap_or(without_tag);
    match basename {
        "postgres" | "postgresql" | "timescaledb" | "pgvector" => Some(Postgres),
        "mysql" | "mariadb" | "percona" => Some(Mariadb),
        "mongo" | "mongodb" => Some(Mongodb),
        "redis" | "keydb" | "valkey" => Some(Redis),
        "minio" => Some(S3),
        _ => None,
    }
}

/// True for the subset of [`detect_service_family`] matches that need the
/// "Elevated permissions" capability warning — every family except S3, since
/// there's no evidence yet that the MinIO image needs `cap_add` the way
/// postgres/mysql/mariadb/mongo/redis entrypoints do.
fn looks_like_database_image(image: &str) -> bool {
    !matches!(
        detect_service_family(image),
        None | Some(temps_entities::preset::ComposeServiceFamily::S3)
    )
}

#[cfg(test)]
mod compose_preview_tests {
    use super::*;

    #[test]
    fn parses_services_with_sequence_depends_on() {
        let yaml = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      - db
  db:
    image: postgres:16
"#;
        let services = list_compose_services(yaml).unwrap();
        assert_eq!(services.len(), 2);
        let app = services.iter().find(|s| s.name == "app").unwrap();
        assert_eq!(app.image.as_deref(), Some("myapp:latest"));
        assert_eq!(app.depends_on, vec!["db".to_string()]);
        assert!(!app.looks_like_database);
        assert_eq!(app.detected_service_type, None);
        let db = services.iter().find(|s| s.name == "db").unwrap();
        assert!(db.looks_like_database);
        assert_eq!(
            db.detected_service_type,
            Some(temps_entities::preset::ComposeServiceFamily::Postgres)
        );
    }

    #[test]
    fn parses_mapping_form_depends_on() {
        let yaml = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      db:
        condition: service_healthy
  db:
    image: mariadb:11
"#;
        let services = list_compose_services(yaml).unwrap();
        let app = services.iter().find(|s| s.name == "app").unwrap();
        assert_eq!(app.depends_on, vec!["db".to_string()]);
        let db = services.iter().find(|s| s.name == "db").unwrap();
        assert!(db.looks_like_database);
        assert_eq!(
            db.detected_service_type,
            Some(temps_entities::preset::ComposeServiceFamily::Mariadb)
        );
    }

    #[test]
    fn does_not_flag_lookalike_image_names() {
        let yaml = r#"
services:
  admin:
    image: mongo-express:latest
  app:
    image: my-postgres-app:latest
"#;
        let services = list_compose_services(yaml).unwrap();
        assert!(services.iter().all(|s| !s.looks_like_database));
        assert!(services.iter().all(|s| s.detected_service_type.is_none()));
    }

    #[test]
    fn detects_each_known_service_family() {
        use temps_entities::preset::ComposeServiceFamily::*;
        let cases = [
            ("postgres:16", Postgres, true),
            ("timescale/timescaledb:latest-pg16", Postgres, true),
            ("mysql:8", Mariadb, true),
            ("mariadb:11", Mariadb, true),
            ("percona:8", Mariadb, true),
            ("mongo:7", Mongodb, true),
            ("redis:7-alpine", Redis, true),
            ("valkey/valkey:8", Redis, true),
            ("keydb/keydb:latest", Redis, true),
            ("minio/minio:latest", S3, false),
        ];
        for (image, expected_family, expect_database_flag) in cases {
            let yaml = format!("services:\n  svc:\n    image: {image}\n");
            let services = list_compose_services(&yaml).unwrap();
            assert_eq!(services.len(), 1, "image {image}");
            assert_eq!(
                services[0].detected_service_type,
                Some(expected_family),
                "image {image}"
            );
            assert_eq!(
                services[0].looks_like_database, expect_database_flag,
                "image {image}"
            );
        }
    }

    #[test]
    fn handles_missing_image() {
        let yaml = r#"
services:
  build-from-source:
    build: .
"#;
        let services = list_compose_services(yaml).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].image, None);
        assert!(!services[0].looks_like_database);
    }

    #[test]
    fn extracts_http_path_from_loopback_compose_healthcheck() {
        let yaml = r#"
services:
  browserless:
    image: ghcr.io/browserless/chromium
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:3000/docs"]
"#;
        let services = list_compose_services(yaml).expect("valid Compose source");
        assert_eq!(services[0].health_check_path.as_deref(), Some("/docs"));
    }

    #[test]
    fn ignores_remote_and_ambiguous_http_healthchecks() {
        let remote = serde_yaml::from_str::<serde_yaml::Value>(
            "test: ['CMD', 'curl', '-f', 'https://example.com/health']",
        )
        .expect("valid healthcheck");
        assert_eq!(http_healthcheck_path(&remote), None);

        let ambiguous = serde_yaml::from_str::<serde_yaml::Value>(
            "test: ['CMD-SHELL', 'curl http://localhost:3000/ready || curl http://localhost:3000/live']",
        )
        .expect("valid healthcheck");
        assert_eq!(http_healthcheck_path(&ambiguous), None);
    }

    #[test]
    fn extracts_environment_variable_names_per_service_without_values() {
        let yaml = r#"
services:
  app:
    environment:
      DATABASE_URL: ${DATABASE_URL}
      APP_ENV: production
  worker:
    environment:
      - QUEUE_URL
      - LOG_LEVEL=info
      - QUEUE_URL
"#;

        let services = list_compose_services(yaml).unwrap();
        let app = services
            .iter()
            .find(|service| service.name == "app")
            .unwrap();
        let worker = services
            .iter()
            .find(|service| service.name == "worker")
            .unwrap();

        assert_eq!(app.environment_variables, vec!["APP_ENV", "DATABASE_URL"]);
        assert_eq!(worker.environment_variables, vec!["LOG_LEVEL", "QUEUE_URL"]);
    }

    #[test]
    fn parses_short_and_long_port_mappings_as_container_targets() {
        let yaml = r#"
services:
  web:
    ports:
      - "15455:80"
      - "127.0.0.1:8443:443/tcp"
      - 3000
      - target: 8080
        published: 18080
        protocol: udp
"#;

        let services = list_compose_services(yaml).unwrap();
        assert_eq!(
            services[0].ports,
            vec![
                temps_entities::preset::ComposePortMapping {
                    target: 80,
                    published: Some(15455),
                    protocol: "tcp".to_string(),
                },
                temps_entities::preset::ComposePortMapping {
                    target: 443,
                    published: Some(8443),
                    protocol: "tcp".to_string(),
                },
                temps_entities::preset::ComposePortMapping {
                    target: 3000,
                    published: None,
                    protocol: "tcp".to_string(),
                },
                temps_entities::preset::ComposePortMapping {
                    target: 8080,
                    published: Some(18080),
                    protocol: "udp".to_string(),
                },
            ]
        );
    }

    #[test]
    fn combines_repository_and_override_ports_for_route_suggestions() {
        let base = r#"
services:
  web:
    image: example/web:latest
    ports:
      - "3000:3000"
"#;
        let override_yaml = r#"
services:
  web:
    ports:
      - "15455:80"
  metrics:
    ports:
      - "9090"
"#;

        let services = list_compose_services_with_override(base, Some(override_yaml)).unwrap();
        let web = services
            .iter()
            .find(|service| service.name == "web")
            .unwrap();
        assert_eq!(web.image.as_deref(), Some("example/web:latest"));
        assert_eq!(web.ports.len(), 2);
        assert!(web
            .ports
            .iter()
            .any(|port| port.target == 80 && port.published == Some(15455)));
        assert!(web.ports.iter().any(|port| port.target == 3000));
        assert_eq!(
            services
                .iter()
                .find(|service| service.name == "metrics")
                .unwrap()
                .ports[0]
                .target,
            9090
        );
    }

    #[test]
    fn effective_preview_merges_override_removes_disabled_services_and_redacts_values() {
        let base = r#"
services:
  web:
    image: example/web:latest
    depends_on:
      - db
    environment:
      PUBLIC_NAME: visible-in-source
      API_TOKEN: ${API_TOKEN}
    ports:
      - "3000:3000"
  db:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD: secret
"#;
        let override_yaml = r#"
services:
  web:
    environment:
      API_TOKEN: override-secret
      LOG_LEVEL: debug
    ports:
      - "15455:80"
"#;

        let preview =
            render_effective_compose_preview(base, Some(override_yaml), &["db".to_string()])
                .unwrap();

        assert_eq!(preview.enabled_services, vec!["web"]);
        assert_eq!(preview.disabled_services, vec!["db"]);
        assert_eq!(preview.redacted_values, 3);
        assert!(!preview.yaml.contains("override-secret"));
        assert!(!preview.yaml.contains("visible-in-source"));
        assert!(!preview.yaml.contains("POSTGRES_PASSWORD"));
        assert!(preview.yaml.contains("API_TOKEN: <redacted>"));
        assert!(preview.yaml.contains("LOG_LEVEL: <redacted>"));
        assert!(preview.yaml.contains("15455:80"));
        assert!(!preview.yaml.contains("3000:3000"));

        let rendered: serde_yaml::Value = serde_yaml::from_str(&preview.yaml).unwrap();
        let depends_on = rendered["services"]["web"]["depends_on"]
            .as_sequence()
            .unwrap();
        assert!(depends_on.is_empty());
    }

    #[test]
    fn effective_preview_rejects_override_for_a_disabled_service() {
        let base = "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n";
        let override_yaml = "services:\n  db:\n    image: postgres:17\n";

        let error =
            render_effective_compose_preview(base, Some(override_yaml), &["db".to_string()])
                .unwrap_err();

        assert!(matches!(error, ComposeParseError::InvalidOverride { .. }));
        assert!(error.to_string().contains("disabled"));
    }

    #[test]
    fn effective_preview_renders_empty_named_resources_as_mappings() {
        let base = r#"
services:
  web:
    image: nginx
volumes:
  app_data:
networks:
  backend:
configs:
  app_config:
secrets:
  app_secret:
"#;

        let preview = render_effective_compose_preview(base, None, &[]).unwrap();

        for resource in ["app_data", "backend", "app_config", "app_secret"] {
            assert!(
                preview.yaml.contains(&format!("{resource}: {{}}")),
                "effective YAML should render {resource} as an explicit empty mapping:\n{}",
                preview.yaml
            );
            assert!(!preview.yaml.contains(&format!("{resource}: null")));
        }
    }

    #[test]
    fn effective_preview_rejects_host_affecting_override_keys() {
        let base = "services:\n  web:\n    image: nginx\n";
        let override_yaml = "services:\n  web:\n    privileged: true\n";

        let error = render_effective_compose_preview(base, Some(override_yaml), &[]).unwrap_err();

        assert!(error.to_string().contains("forbidden key 'privileged'"));
    }

    /// `privileged` is rejected by the deploy-time policy as well, so the
    /// preview must not send the user off to move it into the repository
    /// Compose file — that trip ends in a second, different failure.
    #[test]
    fn effective_preview_does_not_advise_moving_never_allowed_keys_to_the_repository() {
        let base = "services:\n  web:\n    image: nginx\n";
        let override_yaml = "services:\n  web:\n    privileged: true\n";

        let error = render_effective_compose_preview(base, Some(override_yaml), &[]).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("do not permit anywhere") && message.contains("will not help"),
            "expected the message to rule out the repository Compose file, got {message}"
        );
    }

    /// `volumes` *is* accepted in the repository Compose file (subject to
    /// bind-path confinement), so the preview should point there.
    #[test]
    fn effective_preview_points_repo_only_keys_at_the_repository_compose_file() {
        let base = "services:\n  web:\n    image: nginx\n";
        let override_yaml = "services:\n  web:\n    volumes: ['data:/var/lib/data']\n";

        let error = render_effective_compose_preview(base, Some(override_yaml), &[]).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("declare it in the repository Compose file"),
            "expected the message to point at the repository Compose file, got {message}"
        );
        assert!(
            !message.contains("do not permit anywhere"),
            "'volumes' is not forbidden everywhere, got {message}"
        );
    }

    #[test]
    fn override_key_denylists_are_disjoint() {
        for key in NEVER_ALLOWED_SERVICE_KEYS {
            assert!(
                !REPO_ONLY_SERVICE_KEYS.contains(key),
                "'{key}' is classified both as never-allowed and as repo-only"
            );
        }
    }

    #[test]
    fn effective_preview_redacts_sequence_environments_and_build_args() {
        let base = r#"
services:
  web:
    build:
      context: .
      args:
        - NPM_TOKEN=plaintext
    environment:
      - API_KEY=plaintext
      - INHERITED
"#;

        let preview = render_effective_compose_preview(base, None, &[]).unwrap();

        assert_eq!(preview.redacted_values, 3);
        assert!(!preview.yaml.contains("plaintext"));
        assert!(preview.yaml.contains("NPM_TOKEN: <redacted>"));
        assert!(preview.yaml.contains("INHERITED: <redacted>"));
    }

    #[test]
    fn effective_preview_redacts_credentials_outside_environment_blocks() {
        let base = r#"
services:
  web:
    image: example/web:latest
    command: ["sh", "-c", "curl -H 'Authorization: Bearer plaintext-command'"]
    healthcheck:
      test: ["CMD-SHELL", "curl -u user:plaintext-health http://localhost"]
    labels:
      proxy.example.test/api-token: plaintext-label
      proxy.example.test/enabled: "true"
    x-provider:
      client_secret: plaintext-extension
configs:
  application:
    content: |
      password=plaintext-config
"#;

        let preview = render_effective_compose_preview(base, None, &[]).unwrap();

        for secret in [
            "plaintext-command",
            "plaintext-health",
            "plaintext-label",
            "plaintext-extension",
            "plaintext-config",
        ] {
            assert!(
                !preview.yaml.contains(secret),
                "preview leaked secret marker {secret}:\n{}",
                preview.yaml
            );
        }
        assert!(preview.yaml.contains("command: <redacted>"));
        assert!(preview.yaml.contains("test: <redacted>"));
        assert!(preview.yaml.contains("client_secret: <redacted>"));
        assert!(preview.yaml.contains("content: <redacted>"));
    }

    #[test]
    fn errors_on_missing_services_key() {
        let yaml = "version: '3'\n";
        assert!(matches!(
            list_compose_services(yaml),
            Err(ComposeParseError::MissingServices)
        ));
    }

    #[test]
    fn errors_on_invalid_yaml() {
        let yaml = "services: [this is not a mapping";
        assert!(matches!(
            list_compose_services(yaml),
            Err(ComposeParseError::InvalidYaml { .. })
        ));
    }
}
