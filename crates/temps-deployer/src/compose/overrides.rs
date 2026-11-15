// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The override files Temps generates: env, network, security, healthcheck
//! and label overrides, and published-port stripping.

use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use temps_entities::compose_security::ComposeSecurityCheck as PolicyCheck;
use tokio::io::AsyncReadExt;

use super::diagnostics::sanitize_compose_diagnostic;
use super::exec::isolated_docker_command;
use super::{
    ComposeError, ComposeExecutor, COMPOSE_CONFIG_TIMEOUT, COMPOSE_SERVICE_MEMORY_LIMIT,
    DOCKER_IMAGE_INSPECT_TIMEOUT, MAX_COMPOSE_DIAGNOSTIC_BYTES, MAX_CONCURRENT_IMAGE_INSPECTIONS,
    MAX_RESOLVED_COMPOSE_CONFIG_BYTES, MAX_RESOLVED_COMPOSE_SERVICES,
};
use tracing::{info, warn};
use yaml_serde::Value as YamlValue;
use yaml_serde::{Mapping, Value};

impl ComposeExecutor {
    pub(super) async fn detect_image_owned_init_services(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
    ) -> Result<HashSet<String>, ComposeError> {
        let mut cmd = isolated_docker_command();
        cmd.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut cmd, project_dir, compose_file);
        cmd.arg("config")
            .current_dir(project_dir)
            .env("PWD", project_dir.to_string_lossy().to_string());
        cmd.kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(ComposeError::Io)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "docker compose config stdout was unavailable".to_string(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: "docker compose config stderr was unavailable".to_string(),
            })?;
        let stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take((MAX_RESOLVED_COMPOSE_CONFIG_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr
                .take((MAX_COMPOSE_DIAGNOSTIC_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let collect_output = async {
            let status = child.wait().await?;
            let stdout = stdout_task.await.map_err(|error| {
                std::io::Error::other(format!("stdout reader failed: {error}"))
            })??;
            let stderr = stderr_task.await.map_err(|error| {
                std::io::Error::other(format!("stderr reader failed: {error}"))
            })??;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };
        let (status, stdout, stderr) = tokio::time::timeout(COMPOSE_CONFIG_TIMEOUT, collect_output)
            .await
            .map_err(|_| ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "docker compose config timed out after {} seconds while resolving image entrypoints",
                    COMPOSE_CONFIG_TIMEOUT.as_secs()
                ),
            })??;
        if stdout.len() > MAX_RESOLVED_COMPOSE_CONFIG_BYTES {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "resolved Compose config exceeds the {} MiB safety limit",
                    MAX_RESOLVED_COMPOSE_CONFIG_BYTES / 1024 / 1024
                ),
            });
        }
        if !status.success() {
            let stderr =
                sanitize_compose_diagnostic(&String::from_utf8_lossy(&stderr), redact_values);
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "docker compose config failed while resolving image entrypoints: {stderr}"
                ),
            });
        }

        let resolved: YamlValue =
            yaml_serde::from_slice(&stdout).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: format!("resolved Compose config for project '{project_name}'"),
                reason: error.to_string(),
            })?;
        let services = resolved
            .get("services")
            .and_then(YamlValue::as_mapping)
            .ok_or_else(|| ComposeError::InvalidComposeYaml {
                compose_source: format!("resolved Compose config for project '{project_name}'"),
                reason: "missing top-level services mapping".to_string(),
            })?;
        if services.len() > MAX_RESOLVED_COMPOSE_SERVICES {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "resolved Compose config declares {} services; the safety limit is {}",
                    services.len(),
                    MAX_RESOLVED_COMPOSE_SERVICES
                ),
            });
        }

        let mut services_by_image = HashMap::<String, Vec<String>>::new();
        for (name, definition) in services {
            let (Some(service), Some(image)) = (
                name.as_str(),
                definition.get("image").and_then(YamlValue::as_str),
            ) else {
                continue;
            };
            services_by_image
                .entry(image.to_string())
                .or_default()
                .push(service.to_string());
        }
        let docker = self.require_docker()?;
        let inspections = futures::stream::iter(services_by_image)
            .map(|(image, services)| {
                let docker = docker.clone();
                async move {
                    let inspection = tokio::time::timeout(
                        DOCKER_IMAGE_INSPECT_TIMEOUT,
                        docker.inspect_image(&image),
                    )
                    .await;
                    (services, inspection)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_IMAGE_INSPECTIONS)
            .collect::<Vec<_>>()
            .await;
        let mut detected = HashSet::new();
        for (services, inspection) in inspections {
            match inspection {
                Ok(inspect) => match inspect {
                    Ok(inspect) => {
                        let entrypoint = inspect
                            .config
                            .and_then(|config| config.entrypoint)
                            .unwrap_or_default();
                        if Self::entrypoint_owns_pid_one(&entrypoint) {
                            for service in services {
                                info!(
                                    project = %project_name,
                                    service = %service,
                                    "Detected image-owned init process; preserving it as PID 1"
                                );
                                detected.insert(service);
                            }
                        }
                    }
                    Err(error) => {
                        let error = sanitize_compose_diagnostic(&error.to_string(), redact_values);
                        for service in services {
                            warn!(
                                project = %project_name,
                                service = %service,
                                error = %error,
                                "Could not inspect image entrypoint; keeping Docker's init wrapper"
                            );
                        }
                    }
                },
                Err(_) => {
                    for service in services {
                        warn!(
                            project = %project_name,
                            service = %service,
                            timeout_secs = DOCKER_IMAGE_INSPECT_TIMEOUT.as_secs(),
                            "Image entrypoint inspection timed out; keeping Docker's init wrapper"
                        );
                    }
                }
            }
        }
        Ok(detected)
    }

    /// True only for entrypoints that are themselves well-known process
    /// supervisors. Do not inspect arbitrary command arguments or image names:
    /// a shell script may or may not eventually exec an init process, and
    /// guessing there would silently remove zombie reaping from ordinary apps.
    fn entrypoint_owns_pid_one(entrypoint: &[String]) -> bool {
        let Some(executable) = entrypoint.first().map(|value| value.trim()) else {
            return false;
        };
        let normalized = executable.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "/init" | "/sbin/init" | "/usr/sbin/init"
        ) {
            return true;
        }
        Path::new(&normalized)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                matches!(
                    name,
                    "tini"
                        | "tini-static"
                        | "dumb-init"
                        | "docker-init"
                        | "catatonit"
                        | "s6-svscan"
                        | "s6-overlay-suexec"
                        | "runsvdir"
                        | "runsvdir-start"
                        | "my_init"
                )
            })
    }

    /// Generate a docker-compose.temps-override.yml that adds env_file to every
    /// service, plus an `environment:` override for any key a service already
    /// sets inline AND the project has configured. `env_file:` alone can never
    /// beat a service's own inline `environment:` value in Compose's
    /// precedence rules (inline `environment:` always wins over `env_file:`,
    /// regardless of `-f` file order) — an inline override for the *same* key
    /// beats it instead, since Compose merges `environment:` maps across `-f`
    /// files with later files winning. Without this, a compose file that
    /// hardcodes e.g. `DATABASE_URL: postgres://...@postgres:5432/db` inline
    /// makes any Temps-configured `DATABASE_URL` silently ineffective.
    pub(super) fn generate_env_override(
        &self,
        compose_content: &str,
        env_file: &str,
        project_env_vars: &HashMap<String, String>,
    ) -> String {
        let services = self.parse_service_names_yaml(compose_content);
        if services.is_empty() {
            return String::new();
        }
        let inline_keys = self.service_inline_environment_keys(compose_content);
        let build_arg_keys = Self::service_build_arg_keys(compose_content);

        let mut services_map = Mapping::new();
        for service in &services {
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("env_file".to_string()),
                Value::Sequence(vec![Value::String(env_file.to_string())]),
            );

            if let Some(service_inline_keys) = inline_keys.get(service) {
                let mut keys: Vec<&String> = service_inline_keys
                    .iter()
                    .filter(|key| project_env_vars.contains_key(*key))
                    .collect();
                keys.sort();
                if !keys.is_empty() {
                    let mut overrides = Mapping::new();
                    for key in keys {
                        if let Some(value) = project_env_vars.get(key) {
                            overrides
                                .insert(Value::String(key.clone()), Value::String(value.clone()));
                        }
                    }
                    service_map.insert(
                        Value::String("environment".to_string()),
                        Value::Mapping(overrides),
                    );
                }
            }

            // Project variables reach a `build:` service's Dockerfile `ARG`s,
            // the same way they do for a Dockerfile deployment. Only keys the
            // service's own `build.args` leaves undeclared are added: an
            // explicit value in the Compose file wins, and it can still opt
            // into the project value with `KEY: ${KEY}`. Image-only services
            // are skipped, since adding `build:` would turn them into builds.
            if let Some(declared) = build_arg_keys.get(service) {
                let mut keys: Vec<&String> = project_env_vars
                    .keys()
                    .filter(|key| !declared.contains(*key))
                    .collect();
                keys.sort();
                if !keys.is_empty() {
                    let mut args = Mapping::new();
                    for key in keys {
                        if let Some(value) = project_env_vars.get(key) {
                            // `$$` keeps the value literal: Compose interpolates
                            // every `-f` file, generated overrides included.
                            args.insert(
                                Value::String(key.clone()),
                                Value::String(value.replace('$', "$$")),
                            );
                        }
                    }
                    let mut build = Mapping::new();
                    build.insert(Value::String("args".to_string()), Value::Mapping(args));
                    service_map.insert(Value::String("build".to_string()), Value::Mapping(build));
                }
            }

            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );

        yaml_serde::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// For each service, the set of env var keys its own inline `environment:`
    /// block defines (values are ignored — only used to know which keys are
    /// safe to override via a later `-f` file). Handles both Compose shapes:
    /// the sequence form (`- KEY=value` / bare `- KEY`) and the mapping form
    /// (`KEY: value`). Falls back to an empty map if the content isn't valid
    /// YAML — this only feeds a best-effort override, never blocks the deploy.
    fn service_inline_environment_keys(
        &self,
        compose_content: &str,
    ) -> HashMap<String, HashSet<String>> {
        let mut result = HashMap::new();
        let Ok(mut root) = yaml_serde::from_str::<YamlValue>(compose_content) else {
            return result;
        };
        if root.apply_merge().is_err() {
            return result;
        }
        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return result;
        };

        for (name, service_value) in services {
            let Some(service_name) = name.as_str() else {
                continue;
            };
            let Some(environment) = service_value
                .as_mapping()
                .and_then(|m| m.get("environment"))
            else {
                continue;
            };

            let mut keys = HashSet::new();
            if let Some(seq) = environment.as_sequence() {
                for entry in seq {
                    if let Some(s) = entry.as_str() {
                        let key = s.split('=').next().unwrap_or(s).trim();
                        if !key.is_empty() {
                            keys.insert(key.to_string());
                        }
                    }
                }
            } else if let Some(map) = environment.as_mapping() {
                for (k, _) in map {
                    if let Some(k) = k.as_str() {
                        keys.insert(k.to_string());
                    }
                }
            }

            if !keys.is_empty() {
                result.insert(service_name.to_string(), keys);
            }
        }

        result
    }

    /// For each service with a `build:` directive, the build-arg keys its own
    /// `build.args` declares (empty when it declares none, including the
    /// `build: ./dir` short form). Services without `build:` are absent.
    /// Handles both `args:` shapes: the sequence form (`- KEY=value` / bare
    /// `- KEY`) and the mapping form (`KEY: value`). Unparsable YAML yields
    /// an empty map, so no build args are injected rather than failing here.
    fn service_build_arg_keys(compose_content: &str) -> HashMap<String, HashSet<String>> {
        let mut result = HashMap::new();
        let Ok(mut root) = yaml_serde::from_str::<YamlValue>(compose_content) else {
            return result;
        };
        if root.apply_merge().is_err() {
            return result;
        }
        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return result;
        };

        for (name, service_value) in services {
            let Some(service_name) = name.as_str() else {
                continue;
            };
            let Some(build) = service_value.as_mapping().and_then(|m| m.get("build")) else {
                continue;
            };

            let mut keys = HashSet::new();
            match build.get("args") {
                Some(YamlValue::Sequence(seq)) => {
                    for entry in seq.iter().filter_map(YamlValue::as_str) {
                        let key = entry.split('=').next().unwrap_or(entry).trim();
                        if !key.is_empty() {
                            keys.insert(key.to_string());
                        }
                    }
                }
                Some(YamlValue::Mapping(map)) => {
                    keys.extend(map.keys().filter_map(YamlValue::as_str).map(str::to_string));
                }
                _ => {}
            }
            result.insert(service_name.to_string(), keys);
        }

        result
    }

    /// Generate a docker-compose override that attaches every service to the
    /// shared Temps network (`temps_core::NETWORK_NAME`), *in addition to*
    /// the project's own default network — never instead of, so same-stack
    /// DNS between compose services keeps working. This is what lets a
    /// compose service reach a Temps-managed external service (database,
    /// cache) or a single-container app deployment by name, the same way
    /// those already reach each other. Both `default` and the Temps network
    /// are declared explicitly (top level and per-service) rather than
    /// relying on an unreferenced network still being auto-created once a
    /// top-level `networks:` block exists — Compose's merge behavior for
    /// per-service `networks:` lists across `-f` files is easy to get subtly
    /// wrong, so this stays maximally explicit.
    pub(super) fn generate_network_override(&self, compose_content: &str) -> String {
        let services = self.parse_service_names_yaml(compose_content);
        if services.is_empty() {
            return String::new();
        }
        let network_name = temps_core::NETWORK_NAME.as_str();

        let mut networks_declared = Mapping::new();
        networks_declared.insert(
            Value::String("default".to_string()),
            Value::Mapping(Mapping::new()),
        );
        let mut external = Mapping::new();
        external.insert(Value::String("external".to_string()), Value::Bool(true));
        networks_declared.insert(
            Value::String(network_name.to_string()),
            Value::Mapping(external),
        );

        let mut parsed: Value = yaml_serde::from_str(compose_content).unwrap_or(Value::Null);
        // Security validation has already rejected malformed merge keys.
        let _ = parsed.apply_merge();
        let mut services_map = Mapping::new();
        for service in &services {
            if parsed
                .get("services")
                .and_then(|services| services.get(service))
                .and_then(|service| service.get("network_mode"))
                .is_some()
            {
                continue;
            }
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("networks".to_string()),
                Value::Sequence(vec![
                    Value::String("default".to_string()),
                    Value::String(network_name.to_string()),
                ]),
            );
            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("networks".to_string()),
            Value::Mapping(networks_declared),
        );
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );

        yaml_serde::to_string(&Value::Mapping(root)).unwrap_or_default()
    }

    /// Minimal Linux capabilities official image entrypoints commonly need to
    /// fix ownership on a data/socket/cache directory at container start
    /// (`chown`/`chmod` as root, then drop to a service user via
    /// `gosu`/`su-exec`, or nginx's own non-root cache setup) — not just
    /// databases (postgres/mysql/mariadb/mongo), but the vast majority of
    /// official images, including plain web servers like nginx. None of
    /// these grant anything beyond what the container already controls
    /// (its own filesystem layers and process UID/GID), so they're part of
    /// every sandboxed service's baseline rather than an opt-in — a service
    /// still gets the full `cap_drop: ALL` below, just with this minimal set
    /// added back. Genuinely risky capabilities are never granted here; an
    /// image that needs one of those must be exempted from the sandbox
    /// entirely via `unsandboxed_services`.
    pub(super) const RELAXED_CAPABILITIES: [&'static str; 5] =
        ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"];

    pub(super) fn validate_security_exemptions(
        relaxed_capability_services: &[String],
        unsandboxed_services: &[String],
    ) -> Result<(), ComposeError> {
        if let Some(service) = unsandboxed_services
            .iter()
            .find(|service| relaxed_capability_services.contains(service))
        {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.clone(),
                field: "sandbox".to_string(),
                reason:
                    "a service cannot use both elevated capabilities and a disabled Temps sandbox"
                        .to_string(),
            });
        }
        Ok(())
    }

    /// Generate a docker-compose override that applies the same baseline
    /// sandboxing used by the single-container Docker runtime plus narrowly
    /// scoped health/PID-1 compatibility corrections.
    #[cfg(test)]
    fn generate_security_override(
        &self,
        compose_content: &str,
        unsandboxed_services: &[String],
    ) -> String {
        self.generate_security_override_with_image_init(
            compose_content,
            unsandboxed_services,
            &HashSet::new(),
            &Self::healthcheck_loopback_overrides(compose_content, None),
        )
    }

    pub(super) fn generate_security_override_with_image_init(
        &self,
        compose_content: &str,
        unsandboxed_services: &[String],
        detected_image_owned_init_services: &HashSet<String>,
        healthcheck_loopback_overrides: &HashMap<String, String>,
    ) -> String {
        // Enumerate service names from the parsed YAML mapping so inline
        // mappings (`web: {image: nginx}`), anchors (`web: &app`), and merge
        // keys are all hardened, not just lines that end in `:`.
        let services = self.parse_service_names_yaml(compose_content);
        let built_services = Self::services_with_build(compose_content);
        let explicitly_disabled_init_services =
            Self::services_with_image_owned_init(compose_content);

        // Every service receives platform-owned resource and logging bounds,
        // including services with an explicit runtime-sandbox exemption.
        let affected_services = services.iter().collect::<Vec<_>>();

        if affected_services.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in affected_services {
            override_yaml.push_str(&format!("  {}:\n", service));
            if self.enforced(PolicyCheck::Memory) {
                override_yaml.push_str(&format!("    mem_limit: {COMPOSE_SERVICE_MEMORY_LIMIT}\n"));
            }
            if self.enforced(PolicyCheck::Logging) {
                override_yaml.push_str("    logging:\n");
                override_yaml.push_str("      driver: json-file\n");
                override_yaml.push_str("      options:\n");
                override_yaml.push_str("        max-size: 50m\n");
                override_yaml.push_str("        max-file: \"3\"\n");
            }
            if self.enforced(PolicyCheck::Pids) {
                override_yaml.push_str("    pids_limit: 512\n");
            }
            if self.enforced(PolicyCheck::PullPolicy) && !built_services.contains(service.as_str())
            {
                override_yaml.push_str("    pull_policy: always\n");
            }
            let sandboxed = self.policy_authoritative || !unsandboxed_services.contains(service);
            // Applied last in the `-f` order, so `privileged: false` here wins
            // over anything that smuggled `privileged: true` past validation
            // (e.g. via runtime interpolation) as a last line of defense.
            if sandboxed {
                if self.enforced(PolicyCheck::Privileged) {
                    override_yaml.push_str("    privileged: false\n");
                }
                if self.enforced(PolicyCheck::DropCapabilities) {
                    override_yaml.push_str("    cap_drop:\n");
                    override_yaml.push_str("      - ALL\n");
                    override_yaml.push_str("    cap_add:\n");
                    for cap in Self::RELAXED_CAPABILITIES {
                        override_yaml.push_str(&format!("      - {}\n", cap));
                    }
                }
                if self.enforced(PolicyCheck::NoNewPrivileges) {
                    override_yaml.push_str("    security_opt:\n");
                    // Prevents exec-based privilege re-escalation (SUID binaries,
                    // capability gains on exec) after the entrypoint drops to the
                    // service user. Does NOT suppress a relaxed service's entrypoint
                    // from using capabilities it was already granted above (e.g.
                    // `gosu` calling `setuid()` directly) — that's the intended
                    // behavior, not a gap.
                    override_yaml.push_str("      - no-new-privileges:true\n");
                }
            }
            // An explicit `init: false` is a compatibility contract: the
            // image owns PID 1 (commonly s6-overlay) and Docker's init wrapper
            // would make that entrypoint fail. Keep every other sandbox guard
            // instead of forcing the user to disable the entire sandbox.
            if self.enforced(PolicyCheck::Init)
                && detected_image_owned_init_services.contains(service.as_str())
            {
                // This must be explicit rather than merely omitting `init`:
                // the user's base/override may contain `init: true`, and this
                // trusted final override has to win that scalar merge.
                override_yaml.push_str("    init: false\n");
            } else if self.enforced(PolicyCheck::Init)
                && sandboxed
                && !explicitly_disabled_init_services.contains(service.as_str())
            {
                override_yaml.push_str("    init: true\n");
            }
            if let Some(test) = healthcheck_loopback_overrides.get(service.as_str()) {
                override_yaml.push_str("    healthcheck:\n");
                override_yaml.push_str(&format!("      test: {test}\n"));
            }
        }

        override_yaml
    }

    fn services_with_build(compose_content: &str) -> HashSet<String> {
        let Ok(mut root) = yaml_serde::from_str::<Value>(compose_content) else {
            return HashSet::new();
        };
        if root.apply_merge().is_err() {
            return HashSet::new();
        }
        root.get("services")
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(Mapping::iter)
            .filter_map(|(name, definition)| {
                definition
                    .as_mapping()
                    .is_some_and(|service| service.contains_key("build"))
                    .then(|| name.as_str().map(ToOwned::to_owned))
                    .flatten()
            })
            .collect()
    }

    fn services_with_image_owned_init(compose_content: &str) -> std::collections::HashSet<String> {
        let Ok(mut root) = yaml_serde::from_str::<Value>(compose_content) else {
            return std::collections::HashSet::new();
        };
        if root.apply_merge().is_err() {
            return std::collections::HashSet::new();
        }
        root.get("services")
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(Mapping::iter)
            .filter_map(|(name, definition)| {
                let owns_init = definition
                    .as_mapping()
                    .and_then(|service| service.get("init"))
                    .and_then(Value::as_bool)
                    == Some(false);
                owns_init
                    .then(|| name.as_str().map(str::to_string))
                    .flatten()
            })
            .collect()
    }

    /// Return only healthcheck tests that need an IPv4 loopback override.
    /// Read the raw Compose documents rather than `docker compose config`
    /// output so interpolation expressions remain expressions: a health probe
    /// containing `${TOKEN}` must never cause its resolved secret to be copied
    /// into a generated Compose file.
    pub(super) fn healthcheck_loopback_overrides(
        compose_content: &str,
        compose_override: Option<&str>,
    ) -> HashMap<String, String> {
        let mut overrides = HashMap::new();
        for document in [Some(compose_content), compose_override]
            .into_iter()
            .flatten()
        {
            let Ok(mut root) = yaml_serde::from_str::<YamlValue>(document) else {
                continue;
            };
            if root.apply_merge().is_err() {
                continue;
            }
            let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, definition) in services {
                let Some(service_name) = name.as_str() else {
                    continue;
                };
                let Some(healthcheck) = definition.get("healthcheck") else {
                    continue;
                };
                let Some(healthcheck) = healthcheck.as_mapping() else {
                    overrides.remove(service_name);
                    continue;
                };
                if healthcheck.get("disable").and_then(YamlValue::as_bool) == Some(true) {
                    overrides.remove(service_name);
                    continue;
                }
                let Some(mut test) = healthcheck.get("test").cloned() else {
                    continue;
                };
                if Self::rewrite_healthcheck_http_localhost(&mut test) {
                    if let Ok(rendered) = serde_json::to_string(&test) {
                        overrides.insert(service_name.to_string(), rendered);
                    }
                } else {
                    // A later Compose override can replace a base probe with a
                    // probe that no longer needs normalization.
                    overrides.remove(service_name);
                }
            }
        }
        overrides
    }

    pub(crate) fn rewrite_healthcheck_http_localhost(value: &mut YamlValue) -> bool {
        match value {
            YamlValue::String(text) => {
                let normalized = Self::replace_http_localhost(text);
                if normalized == *text {
                    false
                } else {
                    *text = normalized;
                    true
                }
            }
            YamlValue::Sequence(values) => {
                let mut changed = false;
                for value in values {
                    changed |= Self::rewrite_healthcheck_http_localhost(value);
                }
                changed
            }
            YamlValue::Mapping(values) => {
                let mut changed = false;
                for value in values.values_mut() {
                    changed |= Self::rewrite_healthcheck_http_localhost(value);
                }
                changed
            }
            YamlValue::Tagged(value) => Self::rewrite_healthcheck_http_localhost(&mut value.value),
            _ => false,
        }
    }

    fn replace_http_localhost(input: &str) -> String {
        const NEEDLE: &str = "http://localhost";
        const REPLACEMENT: &str = "http://127.0.0.1";

        let mut remainder = input;
        let mut output = String::with_capacity(input.len());
        while let Some(index) = remainder.to_ascii_lowercase().find(NEEDLE) {
            output.push_str(&remainder[..index]);
            let after = &remainder[index + NEEDLE.len()..];
            if after
                .chars()
                .next()
                .is_none_or(|character| matches!(character, ':' | '/' | '?' | '#'))
            {
                output.push_str(REPLACEMENT);
            } else {
                output.push_str(&remainder[index..index + NEEDLE.len()]);
            }
            remainder = after;
        }
        output.push_str(remainder);
        output
    }

    /// Generate a docker-compose override that adds Temps labels to every service.
    /// These labels are required for log collection, monitoring, and container discovery.
    pub(super) fn generate_labels_override(
        &self,
        compose_content: &str,
        labels: &HashMap<String, String>,
    ) -> String {
        // Reuse the same service parsing logic
        let services = self.parse_service_names_yaml(compose_content);

        if services.is_empty() || labels.is_empty() {
            return String::new();
        }

        let mut override_yaml = String::from("services:\n");
        for service in &services {
            override_yaml.push_str(&format!("  {}:\n", service));
            override_yaml.push_str("    labels:\n");
            for (key, value) in labels {
                override_yaml.push_str(&format!("      {}: \"{}\"\n", key, value));
            }
            // Per-service label: the compose service name
            override_yaml.push_str(&format!("      sh.temps.service: \"{}\"\n", service));
        }

        override_yaml
    }

    pub(super) fn parse_service_names_yaml(&self, compose_content: &str) -> Vec<String> {
        let mut root: YamlValue = match yaml_serde::from_str(compose_content) {
            Ok(value) => value,
            Err(_) => return self.parse_service_names(compose_content),
        };
        let _ = root.apply_merge();
        match root.get("services").and_then(YamlValue::as_mapping) {
            Some(services) => {
                let names: Vec<String> = services
                    .keys()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
                if names.is_empty() {
                    self.parse_service_names(compose_content)
                } else {
                    names
                }
            }
            None => self.parse_service_names(compose_content),
        }
    }

    /// Parse service names from compose YAML content.
    fn parse_service_names(&self, compose_content: &str) -> Vec<String> {
        let mut services = Vec::new();
        let mut in_services = false;
        let mut services_indent: usize = 0;
        let mut service_indent: Option<usize> = None;

        for line in compose_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
                service_indent = None;
                continue;
            }

            if in_services {
                if indent <= services_indent {
                    in_services = false;
                    continue;
                }

                if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                    match service_indent {
                        None => {
                            service_indent = Some(indent);
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        Some(si) if indent == si => {
                            services.push(trimmed.trim_end_matches(':').to_string());
                        }
                        _ => {}
                    }
                }
            }
        }

        services
    }

    /// Parse a user override YAML and return the names of services that define `ports:`.
    pub(super) fn services_with_ports_in_override(&self, override_content: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut in_services = false;
        let mut services_indent: usize = 0;
        let mut current_service: Option<(String, usize)> = None; // (name, indent)

        for line in override_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
                current_service = None;
                continue;
            }

            if !in_services {
                continue;
            }

            // Left of services block
            if indent <= services_indent && !trimmed.is_empty() {
                in_services = false;
                continue;
            }

            // Inside a service — check for ports: before checking service names
            if let Some((ref svc_name, svc_indent)) = current_service {
                if indent > svc_indent && (trimmed == "ports:" || trimmed.starts_with("ports:")) {
                    if !result.contains(svc_name) {
                        result.push(svc_name.clone());
                    }
                    continue;
                }
            }

            // Service-level key (direct child of services:)
            if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                let svc_name = trimmed.trim_end_matches(':').to_string();
                match &current_service {
                    None => {
                        current_service = Some((svc_name, indent));
                    }
                    Some((_, si)) if indent == *si => {
                        current_service = Some((svc_name, indent));
                    }
                    _ => {}
                }
            }
        }

        result
    }

    /// Strip `ports:` sections from the base compose content for the given services only.
    /// Other services keep their ports untouched.
    pub(super) fn strip_ports_for_services(
        &self,
        compose_content: &str,
        services: &[String],
    ) -> String {
        let mut output = String::new();
        let mut in_services_block = false;
        let mut services_indent: usize = 0;
        let mut current_service: Option<(String, usize)> = None;
        let mut service_indent: Option<usize> = None;
        let mut skipping_ports = false;
        let mut ports_indent: usize = 0;

        for line in compose_content.lines() {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();

            // Track services: block
            if trimmed == "services:" || trimmed.starts_with("services:") {
                in_services_block = true;
                services_indent = indent;
                service_indent = None;
                current_service = None;
                skipping_ports = false;
                output.push_str(line);
                output.push('\n');
                continue;
            }

            // If currently skipping a ports block, check if we've exited it
            if skipping_ports {
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    // Skip blank lines and comments inside ports block
                    continue;
                }
                if indent > ports_indent {
                    // Still inside ports block (port entries are indented further)
                    continue;
                }
                // We've exited the ports block
                skipping_ports = false;
            }

            if in_services_block && !trimmed.is_empty() && indent <= services_indent {
                in_services_block = false;
                current_service = None;
                service_indent = None;
            }

            if in_services_block && !trimmed.is_empty() && !trimmed.starts_with('#') {
                // Detect service names
                if trimmed.ends_with(':') && !trimmed.contains(' ') && !trimmed.starts_with('-') {
                    match service_indent {
                        None => {
                            service_indent = Some(indent);
                            let name = trimmed.trim_end_matches(':').to_string();
                            current_service = Some((name, indent));
                        }
                        Some(si) if indent == si => {
                            let name = trimmed.trim_end_matches(':').to_string();
                            current_service = Some((name, indent));
                        }
                        _ => {}
                    }
                }

                // Check if this line is `ports:` inside a service we need to strip
                if let Some((ref svc_name, svc_indent)) = current_service {
                    if indent > svc_indent
                        && (trimmed == "ports:" || trimmed.starts_with("ports:"))
                        && services.contains(svc_name)
                    {
                        // If it's `ports:` with inline value like `ports: ["80:80"]`
                        if trimmed.starts_with("ports:") && trimmed != "ports:" {
                            // Single-line ports — just skip this line
                            continue;
                        }
                        // Block-style ports: — skip this line and subsequent indented lines
                        skipping_ports = true;
                        ports_indent = indent;
                        continue;
                    }
                }
            }

            output.push_str(line);
            output.push('\n');
        }

        output
    }
}

#[cfg(test)]
mod tests;
