// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preset resolution, merging and validation.

use super::*;

pub(super) fn compose_public_ports(
    config: Option<&temps_entities::preset::PresetConfig>,
) -> Vec<temps_entities::preset::ComposePublicPort> {
    match config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(compose)) => {
            compose.public_ports.clone()
        }
        _ => Vec::new(),
    }
}

/// Derive the durable project lifecycle from the resolved runtime preset.
///
/// Several framework presets (Vite, Create React App, Docusaurus and Rsbuild)
/// deploy static output even though their stored preset is not the literal
/// `static` variant. Nixpacks can also switch between server and static based
/// on its provider configuration, so callers must classify the complete
/// resolved preset rather than matching only its enum discriminator.
pub(super) fn project_type_for_resolved_preset(
    resolved: &temps_presets::StoredPreset,
) -> Result<ProjectType, ProjectError> {
    let runtime_preset =
        temps_presets::get_preset_for_storage(resolved.preset, resolved.config.as_ref())
            .map_err(|error| {
                ProjectError::InvalidInput(format!(
                    "Could not resolve project lifecycle for preset '{}': {error}",
                    resolved.preset.as_str()
                ))
            })?
            .ok_or_else(|| {
                ProjectError::InvalidInput(format!(
                    "Preset '{}' does not provide a deployable runtime",
                    resolved.preset.as_str()
                ))
            })?;

    Ok(match runtime_preset.project_type() {
        temps_presets::ProjectType::Static => ProjectType::Static,
        temps_presets::ProjectType::Server => ProjectType::Server,
    })
}

/// Apply a canonical preset selection to a project update.
pub(super) fn apply_resolved_preset(
    active: &mut projects::ActiveModel,
    resolved: temps_presets::StoredPreset,
) -> Result<(), ProjectError> {
    if active.project_type.as_ref() != &ProjectType::Service {
        active.project_type = Set(project_type_for_resolved_preset(&resolved)?);
    }
    active.preset = Set(resolved.preset);
    active.preset_config = Set(resolved.config);
    Ok(())
}

/// Preserve discriminator-like fields when a partial config patch omits them.
///
/// An explicit empty Nixpacks provider list still resets to auto, and an
/// explicit Dockerfile variant is still honored. Catalog preset selection is
/// normalized separately by the selected preset's resolver.
pub(super) fn merge_preset_config(
    existing: Option<&temps_entities::preset::PresetConfig>,
    parsed: temps_entities::preset::PresetConfig,
    config_value: &serde_json::Value,
    preserve_omitted_providers: bool,
) -> temps_entities::preset::PresetConfig {
    use temps_entities::preset::PresetConfig;

    let omits_providers = config_value
        .as_object()
        .map(|map| !map.contains_key("providers"))
        .unwrap_or(true);
    let omits_dockerfile_variant = config_value
        .as_object()
        .map(|map| !map.contains_key("variant"))
        .unwrap_or(true);

    match (existing, parsed) {
        (Some(PresetConfig::Nixpacks(existing_cfg)), PresetConfig::Nixpacks(mut parsed_cfg)) => {
            if preserve_omitted_providers
                && omits_providers
                && parsed_cfg.providers.is_empty()
                && !existing_cfg.providers.is_empty()
            {
                parsed_cfg.providers = existing_cfg.providers.clone();
            }
            PresetConfig::Nixpacks(parsed_cfg)
        }
        (
            Some(PresetConfig::Dockerfile(existing_cfg)),
            PresetConfig::Dockerfile(mut parsed_cfg),
        ) => {
            if omits_dockerfile_variant {
                parsed_cfg.variant = existing_cfg.variant;
            }
            if config_value
                .as_object()
                .map(|map| !map.contains_key("imageRuntime"))
                .unwrap_or(true)
            {
                parsed_cfg.image_runtime = existing_cfg.image_runtime.clone();
            }
            let obj = config_value.as_object();
            let omits = |key: &str| obj.map(|map| !map.contains_key(key)).unwrap_or(true);
            if omits("dockerfilePath") {
                parsed_cfg.dockerfile_path = existing_cfg.dockerfile_path.clone();
            }
            if omits("buildContext") {
                parsed_cfg.build_context = existing_cfg.build_context.clone();
            }
            if omits("target") {
                parsed_cfg.target = existing_cfg.target.clone();
            }
            if omits("command") {
                parsed_cfg.runtime.command = existing_cfg.runtime.command.clone();
            }
            if omits("entrypoint") {
                parsed_cfg.runtime.entrypoint = existing_cfg.runtime.entrypoint.clone();
            }
            PresetConfig::Dockerfile(parsed_cfg)
        }
        (
            Some(PresetConfig::DockerCompose(existing_cfg)),
            PresetConfig::DockerCompose(mut parsed_cfg),
        ) => {
            // A partial PATCH (e.g. the settings-page exclusion toggle sends
            // only `excludedServices`) parses into a config where every
            // omitted field is its zero value, not "leave unchanged" — so
            // without this, a one-field patch would silently wipe
            // composePath/composeOverride/publicPorts/composeServices.
            let obj = config_value.as_object();
            let omits = |key: &str| obj.map(|map| !map.contains_key(key)).unwrap_or(true);
            if omits("composePath") {
                parsed_cfg.compose_path = existing_cfg.compose_path.clone();
            }
            if omits("composeOverride") {
                parsed_cfg.compose_override = existing_cfg.compose_override.clone();
            }
            if omits("publicPorts") {
                parsed_cfg.public_ports = existing_cfg.public_ports.clone();
            }
            if omits("excludedServices") {
                parsed_cfg.excluded_services = existing_cfg.excluded_services.clone();
            }
            if omits("composeServices") {
                parsed_cfg.compose_services = existing_cfg.compose_services.clone();
            }
            if omits("relaxedCapabilityServices") {
                parsed_cfg.relaxed_capability_services =
                    existing_cfg.relaxed_capability_services.clone();
            }
            if omits("unsandboxedServices") {
                parsed_cfg.unsandboxed_services = existing_cfg.unsandboxed_services.clone();
            }
            PresetConfig::DockerCompose(parsed_cfg)
        }
        (_, other) => other,
    }
}

/// Resolve a settings patch against the caller's current project row. Writers
/// must call this under the row lock; a preflight result must never be saved.
pub(super) fn resolve_preset_patch(
    current_preset: temps_entities::preset::Preset,
    current_config: Option<&temps_entities::preset::PresetConfig>,
    requested_preset: Option<&str>,
    config_value: Option<&serde_json::Value>,
) -> Result<Option<temps_presets::StoredPreset>, ProjectError> {
    if let Some(slug) = requested_preset {
        return resolve_preset_selection(slug, config_value, current_config).map(Some);
    }
    let Some(value) = config_value else {
        return Ok(None);
    };
    let parsed = temps_entities::preset::PresetConfig::parse_for_preset(&current_preset, value)
        .map_err(|error| ProjectError::InvalidInput(format!("Invalid preset config: {error}")))?;
    let merged = merge_preset_config(current_config, parsed, value, true);
    let config = validate_preset_config(current_preset, merged, Some(value))?;
    Ok(Some(temps_presets::StoredPreset {
        preset: current_preset,
        config: Some(config),
    }))
}

pub(super) fn validate_preset_config(
    preset: temps_entities::preset::Preset,
    config: temps_entities::preset::PresetConfig,
    config_value: Option<&serde_json::Value>,
) -> Result<temps_entities::preset::PresetConfig, ProjectError> {
    temps_presets::validate_preset_config(preset, &config)
        .map_err(|error| ProjectError::InvalidInput(format!("Invalid preset config: {}", error)))?;
    // Only re-validate when this call's patch explicitly touched
    // relaxedCapabilityServices. A value merged forward unchanged from the
    // existing config (e.g. because a later, unrelated patch replaced
    // composeServices and the previously-relaxed service name is no longer
    // in the new snapshot) must not retroactively fail every subsequent
    // save — that would permanently wedge the project's settings until the
    // user manually clears a field they never touched.
    let touches_relaxed_capability_services = config_value
        .and_then(|v| v.as_object())
        .is_some_and(|map| map.contains_key("relaxedCapabilityServices"));
    let touches_unsandboxed_services = config_value
        .and_then(|v| v.as_object())
        .is_some_and(|map| map.contains_key("unsandboxedServices"));
    if touches_relaxed_capability_services || touches_unsandboxed_services {
        if let temps_entities::preset::PresetConfig::DockerCompose(ref cfg) = config {
            validate_relaxed_capability_services(cfg)?;
            validate_unsandboxed_services(cfg)?;
        }
    }
    let touches_public_ports = config_value
        .and_then(|value| value.as_object())
        .is_some_and(|map| map.contains_key("publicPorts"));
    if touches_public_ports {
        if let temps_entities::preset::PresetConfig::DockerCompose(ref cfg) = config {
            validate_compose_public_ports(cfg)?;
        }
    }
    Ok(config)
}

fn validate_compose_public_ports(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    let mut routes = std::collections::HashSet::new();
    for route in &cfg.public_ports {
        if route.service.trim().is_empty() {
            return Err(ProjectError::InvalidInput(
                "Compose public route service cannot be empty".to_string(),
            ));
        }
        if route.port == 0 || route.published == Some(0) {
            return Err(ProjectError::InvalidInput(format!(
                "Compose public route for service '{}' must use ports between 1 and 65535",
                route.service
            )));
        }
        if let Some(path) = route.health_check_path.as_deref() {
            if path.len() > 2048
                || !path.starts_with('/')
                || path.contains('@')
                || path.contains("://")
                || path.chars().any(char::is_control)
            {
                return Err(ProjectError::InvalidInput(format!(
                    "Compose public route health path '{}' for service '{}' must be a safe relative HTTP path starting with '/'",
                    path, route.service
                )));
            }
        }
        if !routes.insert((route.service.as_str(), route.port)) {
            return Err(ProjectError::InvalidInput(format!(
                "Compose service '{}' already has a public URL for port {}",
                route.service, route.port
            )));
        }
        if cfg
            .excluded_services
            .iter()
            .any(|excluded| excluded == &route.service)
        {
            return Err(ProjectError::InvalidInput(format!(
                "Compose service '{}' cannot be both disabled and public",
                route.service
            )));
        }
        if !cfg.compose_services.is_empty()
            && !cfg
                .compose_services
                .iter()
                .any(|service| service.name == route.service)
        {
            return Err(ProjectError::InvalidInput(format!(
                "Compose public route references unknown service '{}'",
                route.service
            )));
        }
    }

    // Additional ports on a service get a `{service}-{port}` hostname label.
    // Reject configs where that label would be claimed by another service, or
    // one of the two URLs would silently shadow the other.
    let labels = temps_entities::preset::compose_public_route_labels(&cfg.public_ports);
    let mut seen_labels = std::collections::HashSet::new();
    for (route, label) in cfg.public_ports.iter().zip(&labels) {
        let collides_with_service = *label != route.service
            && cfg
                .compose_services
                .iter()
                .any(|service| service.name == *label);
        if collides_with_service || !seen_labels.insert(label.as_str()) {
            return Err(ProjectError::InvalidInput(format!(
                "Public URL for port {} of compose service '{}' would use the hostname label '{}', which is already used by another compose service",
                route.port, route.service, label
            )));
        }
    }
    Ok(())
}

/// `relaxed_capability_services` grants a compose service back the Linux
/// capabilities (CHOWN, DAC_OVERRIDE, FOWNER, SETUID, SETGID) many official
/// images' entrypoints need to fix ownership on a data directory and drop
/// from root to a service user at startup — this is not unique to database
/// images (confirmed live: Gitea's own official image hits the identical
/// `chown: ... Operation not permitted` / `su-exec: setgroups: Operation not
/// permitted` failure), so the settings UI offers this toggle for every
/// compose service, not just ones flagged `looks_like_database`. The
/// server-side check mirrors that: any name is accepted as long as it
/// matches a real service in the persisted snapshot, which rejects typos or
/// phantom names without narrowing eligibility to a specific image family.
/// If the snapshot is empty (e.g. before the first deploy has captured one),
/// allow the list through rather than block a legitimate first-time setup,
/// since there is nothing yet to validate against.
fn validate_relaxed_capability_services(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    if cfg.relaxed_capability_services.is_empty() || cfg.compose_services.is_empty() {
        return Ok(());
    }
    for service_name in &cfg.relaxed_capability_services {
        let matches_known_service = cfg.compose_services.iter().any(|s| &s.name == service_name);
        if !matches_known_service {
            return Err(ProjectError::InvalidInput(format!(
                "Cannot grant elevated capabilities to service '{}': it is not a recognized \
                 service in this compose file.",
                service_name
            )));
        }
    }
    Ok(())
}

fn validate_unsandboxed_services(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    if cfg.unsandboxed_services.is_empty() {
        return Ok(());
    }
    if cfg.compose_services.is_empty() {
        return Err(ProjectError::InvalidInput(
            "Cannot disable the Temps sandbox before Compose services have been recognized. Sync the Compose services from the repository first."
                .to_string(),
        ));
    }
    for service_name in &cfg.unsandboxed_services {
        if !cfg.compose_services.iter().any(|s| &s.name == service_name) {
            return Err(ProjectError::InvalidInput(format!(
                "Cannot disable the Temps sandbox for service '{}': it is not a recognized service in this compose file.",
                service_name
            )));
        }
        if cfg.relaxed_capability_services.contains(service_name) {
            return Err(ProjectError::InvalidInput(format!(
                "Service '{}' cannot use both elevated permissions and a disabled sandbox. Remove one of these settings.",
                service_name
            )));
        }
    }
    Ok(())
}

/// Resolve an explicit catalog selection for create/update.
///
/// Existing config is retained when it belongs to the same canonical preset.
/// Selecting base `nixpacks` is authoritative: omitted providers reset to
/// auto-detection while other Nixpacks settings remain intact.
pub(super) fn resolve_preset_selection(
    slug: &str,
    config_value: Option<&serde_json::Value>,
    existing: Option<&temps_entities::preset::PresetConfig>,
) -> Result<temps_presets::StoredPreset, ProjectError> {
    use temps_entities::preset::PresetConfig;

    let base_selection = resolve_preset_slug(slug, None)?;
    let compatible_existing =
        existing.filter(|config| config.preset_type() == base_selection.preset);

    let config = match config_value {
        Some(value) => {
            let parsed =
                PresetConfig::parse_for_preset(&base_selection.preset, value).map_err(|error| {
                    ProjectError::InvalidInput(format!("Invalid preset config: {}", error))
                })?;
            Some(merge_preset_config(
                compatible_existing,
                parsed,
                value,
                slug != "nixpacks",
            ))
        }
        None => {
            let mut config = compatible_existing.cloned();
            if slug == "nixpacks" {
                if let Some(PresetConfig::Nixpacks(nixpacks)) = config.as_mut() {
                    nixpacks.providers.clear();
                }
            }
            config
        }
    };

    let resolved = if config.is_some() {
        resolve_preset_slug(slug, config)?
    } else {
        base_selection
    };
    let config = match resolved.config {
        Some(config) => Some(validate_preset_config(
            resolved.preset,
            config,
            config_value,
        )?),
        None => None,
    };
    Ok(temps_presets::StoredPreset {
        preset: resolved.preset,
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::preset::Preset;

    #[test]
    fn resolved_framework_and_nixpacks_presets_get_their_runtime_project_type() {
        let vite = temps_presets::StoredPreset {
            preset: temps_entities::preset::Preset::Vite,
            config: Some(temps_entities::preset::PresetConfig::Vite(
                temps_entities::preset::ViteConfig::default(),
            )),
        };
        assert_eq!(
            project_type_for_resolved_preset(&vite).expect("Vite is deployable"),
            ProjectType::Static
        );

        let nixpacks_static = temps_presets::StoredPreset {
            preset: temps_entities::preset::Preset::Nixpacks,
            config: Some(temps_entities::preset::PresetConfig::Nixpacks(
                temps_entities::preset::NixpacksConfig {
                    providers: vec![temps_entities::preset::NixpacksProvider::Static],
                    ..Default::default()
                },
            )),
        };
        assert_eq!(
            project_type_for_resolved_preset(&nixpacks_static)
                .expect("static Nixpacks is deployable"),
            ProjectType::Static
        );

        let nixpacks_auto = temps_presets::StoredPreset {
            preset: temps_entities::preset::Preset::Nixpacks,
            config: Some(temps_entities::preset::PresetConfig::Nixpacks(
                temps_entities::preset::NixpacksConfig::default(),
            )),
        };
        assert_eq!(
            project_type_for_resolved_preset(&nixpacks_auto)
                .expect("automatic Nixpacks is deployable"),
            ProjectType::Server
        );
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_matching_database_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["db".to_string()],
            compose_services: vec![
                ComposeServiceSnapshot {
                    name: "db".to_string(),
                    image: Some("postgres:18".to_string()),
                    looks_like_database: true,
                    ..Default::default()
                },
                ComposeServiceSnapshot {
                    name: "web".to_string(),
                    image: Some("nginx".to_string()),
                    looks_like_database: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_non_database_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        // The fix isn't database-specific — e.g. Gitea's own official image
        // hits the identical `chown: ... Operation not permitted` failure at
        // startup, confirmed live. The toggle (and this validation) is
        // available for any real service in the compose file, not just ones
        // flagged looks_like_database.
        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["web".to_string()],
            compose_services: vec![
                ComposeServiceSnapshot {
                    name: "db".to_string(),
                    image: Some("postgres:18".to_string()),
                    looks_like_database: true,
                    ..Default::default()
                },
                ComposeServiceSnapshot {
                    name: "web".to_string(),
                    image: Some("gitea/gitea:latest".to_string()),
                    looks_like_database: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_relaxed_capability_services_rejects_unknown_service_name() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["nonexistent".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "db".to_string(),
                image: Some("postgres:18".to_string()),
                looks_like_database: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_err());
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_when_snapshot_empty() {
        use temps_entities::preset::DockerComposeConfig;

        // No compose_services snapshot yet (e.g. before the first deploy) —
        // nothing to validate against, so don't block a legitimate first-time
        // setup.
        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["db".to_string()],
            compose_services: vec![],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_unsandboxed_services_allows_known_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["webserver".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                image: Some("ghcr.io/paperless-ngx/paperless-ngx:latest".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(validate_unsandboxed_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_unsandboxed_services_rejects_unknown_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["unknown".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error.to_string().contains("not a recognized service"));
    }

    #[test]
    fn test_validate_unsandboxed_services_rejects_elevated_overlap() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["webserver".to_string()],
            unsandboxed_services: vec!["webserver".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot use both elevated permissions and a disabled sandbox"));
    }

    #[test]
    fn test_validate_unsandboxed_services_requires_recognized_snapshot() {
        use temps_entities::preset::DockerComposeConfig;

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["webserver".to_string()],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error
            .to_string()
            .contains("before Compose services have been recognized"));
    }

    #[test]
    fn test_partial_compose_patch_preserves_unsandboxed_services() {
        use temps_entities::preset::{DockerComposeConfig, PresetConfig};

        let existing = PresetConfig::DockerCompose(DockerComposeConfig {
            compose_path: Some("compose.yml".to_string()),
            unsandboxed_services: vec!["webserver".to_string()],
            ..Default::default()
        });
        let parsed = PresetConfig::DockerCompose(DockerComposeConfig {
            excluded_services: vec!["db".to_string()],
            ..Default::default()
        });

        let merged = merge_preset_config(
            Some(&existing),
            parsed,
            &serde_json::json!({ "excludedServices": ["db"] }),
            true,
        );

        match merged {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.compose_path.as_deref(), Some("compose.yml"));
                assert_eq!(cfg.excluded_services, ["db"]);
                assert_eq!(cfg.unsandboxed_services, ["webserver"]);
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }
    }

    #[test]
    fn partial_dockerfile_patch_preserves_and_resets_startup_overrides() {
        use temps_entities::preset::{ContainerRuntimeOverrides, DockerfileConfig, PresetConfig};
        let existing = PresetConfig::Dockerfile(DockerfileConfig {
            dockerfile_path: Some("docker/worker.Dockerfile".into()),
            runtime: ContainerRuntimeOverrides {
                command: Some(vec!["python".into(), "-m".into(), "worker".into()]),
                entrypoint: Some(vec!["/app/start".into()]),
            },
            ..Default::default()
        });
        for patch in [
            serde_json::json!({"buildContext": "api"}),
            serde_json::json!({"command": null, "entrypoint": []}),
            serde_json::json!({"entrypoint": null}),
        ] {
            let parsed = PresetConfig::parse_for_preset(&Preset::Dockerfile, &patch).unwrap();
            let PresetConfig::Dockerfile(merged) =
                merge_preset_config(Some(&existing), parsed, &patch, true)
            else {
                panic!("expected Dockerfile configuration");
            };
            assert_eq!(
                merged.dockerfile_path.as_deref(),
                Some("docker/worker.Dockerfile")
            );
            let PresetConfig::Dockerfile(original) = &existing else {
                unreachable!()
            };
            if patch.get("command").is_some() {
                assert_eq!(merged.runtime.command, None);
            } else {
                assert_eq!(merged.runtime.command, original.runtime.command);
            }
            assert_eq!(
                merged.runtime.entrypoint,
                match patch.get("entrypoint") {
                    Some(serde_json::Value::Null) => None,
                    Some(_) => Some(vec![]),
                    None => original.runtime.entrypoint.clone(),
                }
            );
        }
    }

    #[test]
    fn partial_dockerfile_patch_preserves_service_image_runtime() {
        use temps_entities::preset::{DockerfileConfig, ImageRuntimeConfig, PresetConfig};

        let runtime = ImageRuntimeConfig {
            image_ref: "registry.example.test/app:1".to_string(),
            command: Some(vec!["serve".to_string()]),
            health_check_path: Some("/ready".to_string()),
        };
        let existing = PresetConfig::Dockerfile(DockerfileConfig {
            image_runtime: Some(runtime.clone()),
            ..Default::default()
        });
        let parsed = PresetConfig::Dockerfile(DockerfileConfig {
            target: Some("runner".to_string()),
            ..Default::default()
        });

        let merged = merge_preset_config(
            Some(&existing),
            parsed,
            &serde_json::json!({ "target": "runner" }),
            true,
        );

        match merged {
            PresetConfig::Dockerfile(config) => {
                assert_eq!(config.target.as_deref(), Some("runner"));
                assert_eq!(config.image_runtime, Some(runtime));
            }
            other => panic!("expected Dockerfile config, got {other:?}"),
        }
    }

    #[test]
    fn test_preset_selection_rejects_unknown_unsandboxed_service() {
        let config = serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [{ "name": "webserver", "image": "paperless:latest" }],
            "unsandboxedServices": ["unknown"]
        });

        let error = resolve_preset_selection("docker-compose", Some(&config), None).unwrap_err();

        assert!(error.to_string().contains("not a recognized service"));
    }

    #[test]
    fn test_preset_selection_rejects_overlapping_security_modes() {
        let config = serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [{ "name": "webserver", "image": "paperless:latest" }],
            "relaxedCapabilityServices": ["webserver"],
            "unsandboxedServices": ["webserver"]
        });

        let error = resolve_preset_selection("docker-compose", Some(&config), None).unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot use both elevated permissions and a disabled sandbox"));
    }

    #[test]
    fn validate_compose_public_ports_rejects_duplicate_unknown_and_disabled_services() {
        use temps_entities::preset::{
            ComposePublicPort, ComposeServiceSnapshot, DockerComposeConfig,
        };

        let route = |service: &str| ComposePublicPort {
            service: service.to_string(),
            port: 80,
            published: Some(15_455),
            health_check_path: None,
        };
        let base = DockerComposeConfig {
            compose_services: vec![ComposeServiceSnapshot {
                name: "web".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let duplicate = DockerComposeConfig {
            public_ports: vec![route("web"), route("web")],
            ..base.clone()
        };
        assert!(validate_compose_public_ports(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("already has a public URL for port 80"));

        let unknown = DockerComposeConfig {
            public_ports: vec![route("missing")],
            ..base.clone()
        };
        assert!(validate_compose_public_ports(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown service"));

        let disabled = DockerComposeConfig {
            public_ports: vec![route("web")],
            excluded_services: vec!["web".to_string()],
            ..base
        };
        assert!(validate_compose_public_ports(&disabled)
            .unwrap_err()
            .to_string()
            .contains("disabled and public"));
    }

    #[test]
    fn validate_compose_public_ports_accepts_target_and_published_mapping() {
        use temps_entities::preset::{
            ComposePublicPort, ComposeServiceSnapshot, DockerComposeConfig,
        };
        let cfg = DockerComposeConfig {
            public_ports: vec![ComposePublicPort {
                service: "web".to_string(),
                port: 80,
                published: Some(15_455),
                health_check_path: None,
            }],
            compose_services: vec![ComposeServiceSnapshot {
                name: "web".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(validate_compose_public_ports(&cfg).is_ok());
    }

    #[test]
    fn validate_compose_public_ports_accepts_several_ports_on_one_service() {
        use temps_entities::preset::{
            ComposePublicPort, ComposeServiceSnapshot, DockerComposeConfig,
        };
        let route = |service: &str, port: u16| ComposePublicPort {
            service: service.to_string(),
            port,
            ..Default::default()
        };
        let snapshot = |name: &str| ComposeServiceSnapshot {
            name: name.to_string(),
            ..Default::default()
        };
        let cfg = DockerComposeConfig {
            public_ports: vec![route("trawl", 3000), route("trawl", 9222)],
            compose_services: vec![snapshot("trawl")],
            ..Default::default()
        };
        assert!(validate_compose_public_ports(&cfg).is_ok());

        // `trawl:9222` would be served at the `trawl-9222` label, which a
        // service literally named `trawl-9222` already owns.
        let colliding = DockerComposeConfig {
            public_ports: vec![route("trawl", 3000), route("trawl", 9222)],
            compose_services: vec![snapshot("trawl"), snapshot("trawl-9222")],
            ..Default::default()
        };
        assert!(validate_compose_public_ports(&colliding)
            .unwrap_err()
            .to_string()
            .contains("hostname label 'trawl-9222'"));
    }

    #[test]
    fn validate_compose_public_ports_rejects_absolute_health_urls() {
        use temps_entities::preset::{ComposePublicPort, DockerComposeConfig};
        let cfg = DockerComposeConfig {
            public_ports: vec![ComposePublicPort {
                service: "web".to_string(),
                port: 80,
                health_check_path: Some("https://attacker.example/ready".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = validate_compose_public_ports(&cfg).expect_err("absolute URL must fail");
        assert!(error.to_string().contains("safe relative HTTP path"));
    }
}
