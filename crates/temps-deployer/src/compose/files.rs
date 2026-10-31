// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Writing the compose files and env files Temps generates for a project.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use temps_entities::compose_security::ComposeSecurityCheck as PolicyCheck;

use super::{
    ComposeDeployRequest, ComposeError, ComposeExecutor, CONTAINER_SECRETS_DIR,
    TEMPS_SECRETS_OVERRIDE,
};
use tracing::{debug, warn};
use yaml_serde::Value as YamlValue;
use yaml_serde::Value;

/// Compose filenames Temps generates itself inside the stack directory.
///
/// `compose_up` passes every one of these to `docker compose -f` purely
/// because it exists on disk, and only the *user-supplied* override is run
/// through `validate_compose_override`. A tenant compose file that names one
/// of them as an `env_file:` target would therefore have Temps write
/// attacker-influenced bytes to a path the daemon later parses as trusted
/// Compose YAML — dotenv syntax and YAML syntax overlap enough (`#` comments,
/// bare `key: value` lines) to smuggle a whole `services:` mapping past the
/// dotenv reader. That is a sandbox escape, so referencing these names is
/// rejected outright rather than silently skipped.
///
/// This is the *single* list. `validate_compose_path_not_generated` consumed a
/// second, longer copy of it, and the two drifted: `TEMPS_SECRETS_OVERRIDE` was
/// on that one but not here, even though it is passed to `docker compose -f` on
/// sight exactly like the rest. Only write ordering kept that from being
/// exploitable — the secrets override is written or deleted unconditionally
/// after the `env_file` loop, unlike the labels override, which is written only
/// when labels exist and was therefore reachable. One refactor making the
/// secrets override conditional would have reopened the escape, so the lists
/// are now the same list.
pub(crate) const RESERVED_GENERATED_COMPOSE_FILES: &[&str] = &[
    "docker-compose.temps-env.yml",
    "docker-compose.temps-network.yml",
    "docker-compose.temps-override.yml",
    "docker-compose.temps-labels.yml",
    "docker-compose.temps-security.yml",
    TEMPS_SECRETS_OVERRIDE,
];

/// Whether `path` names one of the Compose files Temps generates.
///
/// Compares the final component only: `./docker-compose.temps-override.yml`
/// and `docker-compose.temps-override.yml` resolve to the same file in the
/// stack root, and a same-named file in a subdirectory is not passed to
/// `docker compose -f`, so it is harmless.
pub(crate) fn is_reserved_generated_compose_file(path: &Path) -> bool {
    // Only the stack root is dangerous: `compose_input_args` looks for
    // these names directly under the project directory.
    if path.parent().is_some_and(|parent| {
        !parent.as_os_str().is_empty() && parent != Path::new(".") && parent != Path::new("")
    }) {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            RESERVED_GENERATED_COMPOSE_FILES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        })
}

/// Where the contents of a referenced `env_file` come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFileSource {
    /// The repository ships the file; it is copied verbatim.
    Repository(PathBuf),
    /// The repository does not ship it, so it is synthesized from the
    /// project's Temps environment variables.
    ProjectEnvironment,
}

/// One `env_file:` reference resolved against the repository checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFilePlan {
    /// Path exactly as written in the compose file, relative to the project.
    pub path: String,
    pub source: EnvFileSource,
}

/// Render environment variables as `KEY=value` lines for an env file.
///
/// Keys are emitted in sorted order so redeploying with an unchanged set
/// produces an identical file (`HashMap` iteration order is not stable).
pub(super) fn render_env_file(vars: &HashMap<String, String>) -> Result<String, ComposeError> {
    let mut entries: Vec<(&String, &String)> = vars.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut rendered = String::new();
    for (key, value) in entries {
        let mut key_chars = key.chars();
        let valid_key = key_chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && key_chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !valid_key {
            return Err(ComposeError::InvalidEnvironmentVariable {
                key: key.clone(),
                reason: "keys must match [A-Za-z_][A-Za-z0-9_]*".to_string(),
            });
        }

        // Compose applies interpolation to unquoted and double-quoted dotenv
        // values. Encode a literal value in double quotes, escaping `$` as
        // `$$` so tenant data cannot introduce a second interpolation step or
        // inject another dotenv assignment through a newline.
        let mut encoded = String::with_capacity(value.len() + 2);
        encoded.push('"');
        for ch in value.chars() {
            match ch {
                '\\' => encoded.push_str("\\\\"),
                '"' => encoded.push_str("\\\""),
                '\n' => encoded.push_str("\\n"),
                '\r' => encoded.push_str("\\r"),
                '\t' => encoded.push_str("\\t"),
                '$' => encoded.push_str("$$"),
                ch if ch.is_control() => {
                    return Err(ComposeError::InvalidEnvironmentVariable {
                        key: key.clone(),
                        reason: format!(
                            "value contains unsupported control character U+{:04X}",
                            ch as u32
                        ),
                    });
                }
                _ => encoded.push(ch),
            }
        }
        encoded.push('"');
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&encoded);
        rendered.push('\n');
    }
    Ok(rendered)
}

impl ComposeExecutor {
    /// Every override filename Temps writes into the stack directory itself.
    /// Reject a `compose_path` that names one of Temps' own generated
    /// overrides. Those files are written unconditionally, so pointing the
    /// project at one would have Temps overwrite the user's compose document
    /// with generated content and then pass the same file to `-f` twice.
    pub(super) fn validate_compose_path_not_generated(
        compose_path: &str,
    ) -> Result<(), ComposeError> {
        let name = Path::new(compose_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(compose_path);
        if RESERVED_GENERATED_COMPOSE_FILES.contains(&name) {
            return Err(ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_path.to_string(),
                reason: format!(
                    "'{name}' is reserved for a Temps-generated override; \
                     rename the compose file in your repository"
                ),
            });
        }
        Ok(())
    }

    pub(crate) async fn write_compose_files(
        &self,
        project_dir: &Path,
        request: &ComposeDeployRequest,
        secret_generation: &str,
    ) -> Result<(), ComposeError> {
        tokio::fs::create_dir_all(project_dir).await.map_err(|e| {
            ComposeError::FileWriteFailed {
                path: project_dir.display().to_string(),
                reason: e.to_string(),
            }
        })?;

        let compose_file = request
            .compose_path
            .as_deref()
            .unwrap_or("docker-compose.yml");
        Self::validate_relative_path(compose_file, "compose_path")?;
        // Same reasoning as the `env_file` guard below: these names are handed
        // to `docker compose -f` on sight, so nothing user-selected may land on
        // them. Here it would also mean the base compose file and a generated
        // override silently clobber each other depending on write order.
        if is_reserved_generated_compose_file(Path::new(compose_file)) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<compose-files>".to_string(),
                field: "compose_path".to_string(),
                reason: format!(
                    "'{compose_file}' is a Compose file name Temps generates; \
                     choose a different compose file path."
                ),
            });
        }
        let compose_path =
            Self::confined_write_path(project_dir, Path::new(compose_file), "compose_path")?;

        // If the user override defines ports for specific services, strip those
        // ports from the base compose file. Docker Compose merges (appends) port
        // arrays from override files, so without stripping, the original ports
        // remain alongside the override ports, causing conflicts.
        let compose_to_write = if let Some(ref user_override) = request.compose_override {
            let services_with_port_overrides = self.services_with_ports_in_override(user_override);
            if services_with_port_overrides.is_empty() {
                request.compose_content.clone()
            } else {
                self.strip_ports_for_services(
                    &request.compose_content,
                    &services_with_port_overrides,
                )
            }
        } else {
            request.compose_content.clone()
        };
        let compose_to_write = Self::rewrite_missing_relative_bind_mounts(
            &compose_to_write,
            compose_path.parent().unwrap_or(project_dir),
            &self.project_dir(&request.project_name).join("binds"),
        )?;
        let compose_to_write = Self::rebind_loopback_published_ports(
            &compose_to_write,
            self.published_host_ip,
            "compose file",
        )?;

        tokio::fs::write(&compose_path, &compose_to_write)
            .await
            .map_err(|e| ComposeError::FileWriteFailed {
                path: compose_path.display().to_string(),
                reason: e.to_string(),
            })?;

        // Write .env file (repo's original .env content if any)
        if let Some(ref env_content) = request.env_content {
            if !env_content.trim().is_empty() {
                let env_path = Self::confined_write_path(project_dir, Path::new(".env"), ".env")?;
                tokio::fs::write(&env_path, env_content.trim())
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: env_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        // Satisfy every `env_file:` the compose file declares. The stack
        // directory is not a repo checkout — only the files Temps writes here
        // exist — so a referenced env file must either be copied out of the
        // repository or synthesized, otherwise `docker compose` aborts with
        // "env file ... not found" before a single container starts. Operators
        // deploying somebody else's repository cannot fix that by editing it.
        let already_written_env = request
            .env_content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty());
        for plan in Self::plan_env_files_for_compose_file(
            &request.compose_content,
            compose_file,
            request.repo_dir.as_deref(),
        ) {
            if already_written_env && plan.path == ".env" {
                continue;
            }
            // Fail closed rather than skipping: a stack that only works because
            // Temps quietly declined to satisfy one of its `env_file:` entries
            // is worse to debug than an explicit "rename this file" error.
            if is_reserved_generated_compose_file(Path::new(&plan.path)) {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<compose-files>".to_string(),
                    field: "env_file".to_string(),
                    reason: format!(
                        "'{}' is a Compose file name Temps generates and passes to the Docker \
                         daemon; an env_file may not target it. Rename the env file in your \
                         compose configuration.",
                        plan.path
                    ),
                });
            }
            if !self.enforced(PolicyCheck::EnvFiles) && Self::is_dangerous_host_path(&plan.path) {
                // Docker reads this existing administrator-authorized file. Never synthesize or overwrite it.
                continue;
            }
            let destination =
                Self::confined_write_path(project_dir, Path::new(&plan.path), "env_file")?;
            let contents = match &plan.source {
                EnvFileSource::Repository(repo_path) => tokio::fs::read_to_string(repo_path)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: repo_path.display().to_string(),
                        reason: format!("failed to read referenced env file from repository: {e}"),
                    })?,
                EnvFileSource::ProjectEnvironment => render_env_file(&request.environment_vars)?,
            };
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    ComposeError::FileWriteFailed {
                        path: parent.display().to_string(),
                        reason: e.to_string(),
                    }
                })?;
            }
            tokio::fs::write(&destination, contents)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps system env vars to .env.temps
        // These include SENTRY_DSN, TEMPS_API_URL, TEMPS_API_TOKEN, OTEL vars, etc.
        if !request.environment_vars.is_empty() {
            let temps_env = render_env_file(&request.environment_vars)?;
            let temps_env_path =
                Self::confined_write_path(project_dir, Path::new(".env.temps"), ".env.temps")?;
            tokio::fs::write(&temps_env_path, &temps_env)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_env_path.display().to_string(),
                    reason: e.to_string(),
                })?;

            // Write Temps env override (auto-generated, injects .env.temps into every service)
            let temps_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-env.yml"),
                "docker-compose.temps-env.yml",
            )?;
            // Absolute: Compose resolves relative paths in every `-f` file
            // against the directory of the *first* one, so with the compose
            // file in `deploy/` a relative `.env.temps` would point at
            // `deploy/.env.temps`, which does not exist.
            let override_content = self.generate_env_override(
                &request.compose_content,
                &temps_env_path.to_string_lossy(),
                &request.environment_vars,
            );
            tokio::fs::write(&temps_override_path, &override_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: temps_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps network override (attaches every service to the shared
        // temps-app-network, in addition to the project's own default
        // network, so compose services can reach Temps-managed external
        // services). Unconditional — unlike the env override above, this
        // doesn't depend on the project having any configured env vars.
        let network_content = self.generate_network_override(&request.compose_content);
        if !network_content.is_empty() {
            let network_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-network.yml"),
                "docker-compose.temps-network.yml",
            )?;
            tokio::fs::write(&network_override_path, &network_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: network_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps' runtime policy/compatibility override. Sandboxing applies
        // only to opted-in services; safe health/PID-1 corrections also apply
        // to unsandboxed services without adding any isolation controls.
        self.write_security_override(project_dir, request, &HashSet::new())
            .await?;

        // Materialize project secrets and mount them at /run/secrets in every
        // service. Values never enter the compose documents, the env files or
        // the build args — only the host path of the directory does.
        let documents = [
            request.compose_content.as_str(),
            request.compose_override.as_deref().unwrap_or_default(),
        ];
        let services = self.all_service_names(&documents);
        let secret_mounts = self
            .materialize_secrets(
                &request.project_name,
                secret_generation,
                &request.secrets,
                &request.secret_compose_services,
                &services,
            )
            .await?;
        let secrets_override_path = Self::confined_write_path(
            project_dir,
            Path::new(TEMPS_SECRETS_OVERRIDE),
            TEMPS_SECRETS_OVERRIDE,
        )?;
        let secrets_content = if secret_mounts.is_empty() {
            String::new()
        } else {
            let skip = Self::services_managing_own_secrets(&documents);
            for service in &skip {
                warn!(
                    project = %request.project_name,
                    service = %service,
                    "Service already mounts {CONTAINER_SECRETS_DIR}; \
                     skipping Temps secret injection for it"
                );
            }
            let (content, mounted) = self.generate_secrets_override(&secret_mounts, &skip);
            debug!(
                project = %request.project_name,
                services = %mounted.join(", "),
                "Mounted project secrets into compose services"
            );
            content
        };
        if secrets_content.is_empty() {
            // A stale override from a previous deploy would keep mounting a
            // directory this deploy just emptied.
            if let Err(error) = tokio::fs::remove_file(&secrets_override_path).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(ComposeError::FileWriteFailed {
                        path: secrets_override_path.display().to_string(),
                        reason: format!("failed to remove stale secrets override: {error}"),
                    });
                }
            }
        } else {
            tokio::fs::write(&secrets_override_path, &secrets_content)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: secrets_override_path.display().to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Write Temps labels override (injects sh.temps.* labels into every service for log collection)
        if !request.labels.is_empty() {
            let labels_override_path = Self::confined_write_path(
                project_dir,
                Path::new("docker-compose.temps-labels.yml"),
                "docker-compose.temps-labels.yml",
            )?;
            let labels_content =
                self.generate_labels_override(&request.compose_content, &request.labels);
            if !labels_content.is_empty() {
                tokio::fs::write(&labels_override_path, &labels_content)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: labels_override_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        // Write user-provided override if present. Inline overrides come from project
        // settings, so validate them (structural allow-list) before handing them to the
        // host Docker daemon — defense-in-depth alongside the value-level policy above.
        if let Some(ref user_override) = request.compose_override {
            if !user_override.trim().is_empty() {
                Self::validate_compose_override_with_policy(
                    &request.project_name,
                    &request.compose_content,
                    user_override,
                    &self.policy,
                )?;

                let override_path = Self::confined_write_path(
                    project_dir,
                    Path::new("docker-compose.temps-override.yml"),
                    "docker-compose.temps-override.yml",
                )?;
                // Validated above as written; only the bind address changes.
                let user_override = Self::rebind_loopback_published_ports(
                    user_override,
                    self.published_host_ip,
                    "compose override",
                )?;
                tokio::fs::write(&override_path, user_override)
                    .await
                    .map_err(|e| ComposeError::FileWriteFailed {
                        path: override_path.display().to_string(),
                        reason: e.to_string(),
                    })?;
            }
        }

        debug!(
            path = %compose_path.display(),
            "Wrote compose files"
        );

        Ok(())
    }

    pub(super) async fn write_security_override(
        &self,
        project_dir: &Path,
        request: &ComposeDeployRequest,
        detected_image_owned_init_services: &HashSet<String>,
    ) -> Result<(), ComposeError> {
        let healthcheck_loopback_overrides = Self::healthcheck_loopback_overrides(
            &request.compose_content,
            request.compose_override.as_deref(),
        );
        let security_content = self.generate_security_override_with_image_init(
            &request.compose_content,
            &request.unsandboxed_services,
            detected_image_owned_init_services,
            &healthcheck_loopback_overrides,
        );
        let security_override_path = Self::confined_write_path(
            project_dir,
            Path::new("docker-compose.temps-security.yml"),
            "docker-compose.temps-security.yml",
        )?;
        if !security_content.is_empty() {
            tokio::fs::write(&security_override_path, &security_content)
                .await
                .map_err(|error| ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: error.to_string(),
                })?;
        } else if let Err(error) = tokio::fs::remove_file(&security_override_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ComposeError::FileWriteFailed {
                    path: security_override_path.display().to_string(),
                    reason: format!("failed to remove stale security override: {error}"),
                });
            }
        }
        Ok(())
    }

    /// Rebind every loopback-published port to `host_ip`.
    ///
    /// Runs after the security policy has checked the bindings, so on the
    /// control plane (`host_ip` is loopback) this returns the content
    /// unchanged. On a worker it keeps every entry the author wrote, with the
    /// same host and container ports and protocol, and changes only the
    /// interface from `127.0.0.1` to the node's private address, which is
    /// where the control-plane proxy reaches the node. Entries that are not
    /// loopback bindings (an administrator relaxed the published-ports check)
    /// are left exactly as written.
    fn rebind_loopback_published_ports(
        compose_content: &str,
        host_ip: std::net::IpAddr,
        compose_source: &str,
    ) -> Result<String, ComposeError> {
        if host_ip.is_loopback() {
            return Ok(compose_content.to_string());
        }
        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|error| {
            ComposeError::InvalidComposeYaml {
                compose_source: compose_source.to_string(),
                reason: error.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: compose_source.to_string(),
                reason: format!("failed to expand YAML merge keys: {error}"),
            })?;
        let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
            return Ok(compose_content.to_string());
        };
        let short_host = match host_ip {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        };
        let mut changed = false;
        for definition in services.values_mut() {
            let Some(entries) = definition
                .as_mapping_mut()
                .and_then(|service| service.get_mut(YamlValue::String("ports".to_string())))
                .and_then(YamlValue::as_sequence_mut)
            else {
                continue;
            };
            for entry in entries {
                match entry {
                    YamlValue::String(binding) => {
                        if let Some(rest) = binding.strip_prefix("127.0.0.1:") {
                            *binding = format!("{short_host}:{rest}");
                            changed = true;
                        }
                    }
                    YamlValue::Mapping(binding) => {
                        let key = YamlValue::String("host_ip".to_string());
                        if binding.get(&key).and_then(YamlValue::as_str) == Some("127.0.0.1") {
                            binding.insert(key, YamlValue::String(host_ip.to_string()));
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            return Ok(compose_content.to_string());
        }
        yaml_serde::to_string(&root).map_err(|error| ComposeError::InvalidComposeYaml {
            compose_source: compose_source.to_string(),
            reason: format!("failed to render ports rebound to {host_ip}: {error}"),
        })
    }

    /// Rewrite absent relative bind sources to stable project-scoped host
    /// directories. Git checkouts are deployment-temporary, so allowing Docker
    /// to create `./data` there would silently move the mount to a fresh empty
    /// directory on every redeploy. Existing repository paths are left alone
    /// because they may intentionally provide checked-in files or build data.
    fn rewrite_missing_relative_bind_mounts(
        compose_content: &str,
        compose_base: &Path,
        persistent_bind_root: &Path,
    ) -> Result<String, ComposeError> {
        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|error| {
            ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: error.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to expand YAML merge keys: {error}"),
            })?;

        let Some(services) = root.get_mut("services").and_then(YamlValue::as_mapping_mut) else {
            return Ok(compose_content.to_string());
        };
        let mut changed = false;

        for definition in services.values_mut() {
            let Some(entries) = definition
                .as_mapping_mut()
                .and_then(|service| service.get_mut(YamlValue::String("volumes".to_string())))
                .and_then(YamlValue::as_sequence_mut)
            else {
                continue;
            };

            for entry in entries {
                let Some(source) = Self::volume_source(entry) else {
                    continue;
                };
                if (Self::is_named_volume_ref(&source) && !Self::is_explicit_bind_mount(entry))
                    || Self::is_dangerous_host_path(&source)
                {
                    continue;
                }

                let normalized = Self::lexically_normalize(&source);
                let repository_source = compose_base.join(&normalized);
                let stable_name = Self::stable_bind_name(&normalized);
                let stable_source = persistent_bind_root.join(stable_name);

                // Once a source has been assigned stable storage, keep using it
                // even if a later repository revision happens to add an empty
                // directory at the same relative path.
                if repository_source.exists() && !stable_source.exists() {
                    continue;
                }
                std::fs::create_dir_all(&stable_source).map_err(|error| {
                    ComposeError::FileWriteFailed {
                        path: stable_source.display().to_string(),
                        reason: format!(
                            "failed to create persistent directory for relative bind '{source}': {error}"
                        ),
                    }
                })?;
                let stable_source = stable_source.to_string_lossy().to_string();

                if let Some(short) = entry.as_str() {
                    let mut parts = short.splitn(3, ':');
                    let _ = parts.next();
                    let Some(target) = parts.next() else {
                        continue;
                    };
                    let mode = parts.next();
                    *entry = YamlValue::String(match mode {
                        Some(mode) => format!("{stable_source}:{target}:{mode}"),
                        None => format!("{stable_source}:{target}"),
                    });
                    changed = true;
                } else if let Some(mapping) = entry.as_mapping_mut() {
                    mapping.insert(
                        YamlValue::String("source".to_string()),
                        YamlValue::String(stable_source),
                    );
                    changed = true;
                }
            }
        }

        if changed {
            yaml_serde::to_string(&root).map_err(|error| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to render persistent relative bind mounts: {error}"),
            })
        } else {
            Ok(compose_content.to_string())
        }
    }

    fn stable_bind_name(relative_source: &str) -> String {
        // FNV-1a is used only for a compact deterministic directory suffix,
        // not for a security boundary. The readable prefix helps operators
        // identify storage on self-hosted installations.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in relative_source.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let readable = relative_source
            .rsplit('/')
            .next()
            .unwrap_or("bind")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        format!("{readable}-{hash:016x}")
    }

    /// Removes excluded services — and references to them in other services'
    /// `depends_on` — from a compose file's `services:` map. Compose's own
    /// `-f` override-file merging can only add/replace fields on a service,
    /// never unset a whole service (the same constraint the port-stripping
    /// logic above hits at a narrower scope), so exclusion has to rewrite the
    /// base document before it's written to disk or validated.
    ///
    /// Re-serializes the whole file, which normalizes formatting (comments,
    /// quoting, anchor definitions) rather than preserving it byte-for-byte —
    /// an accepted tradeoff, since `depends_on` can be either a list or a map
    /// of `{condition: ...}` entries, and correctly stripping a name out of
    /// either shape needs structural understanding, not line-oriented text
    /// editing.
    pub fn strip_excluded_services(
        &self,
        compose_content: &str,
        excluded: &[String],
    ) -> Result<String, ComposeError> {
        if excluded.is_empty() || compose_content.trim().is_empty() {
            return Ok(compose_content.to_string());
        }

        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: e.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: "compose file".to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        let Some(services) = root
            .as_mapping_mut()
            .and_then(|m| m.get_mut("services"))
            .and_then(Value::as_mapping_mut)
        else {
            return Ok(compose_content.to_string());
        };

        for name in excluded {
            services.remove(name.as_str());
        }

        for (_, service_value) in services.iter_mut() {
            let Some(depends_on) = service_value
                .as_mapping_mut()
                .and_then(|m| m.get_mut("depends_on"))
            else {
                continue;
            };
            if let Some(seq) = depends_on.as_sequence_mut() {
                seq.retain(|entry| {
                    entry
                        .as_str()
                        .is_none_or(|name| !excluded.iter().any(|e| e == name))
                });
            } else if let Some(map) = depends_on.as_mapping_mut() {
                for name in excluded {
                    map.remove(name.as_str());
                }
            }
        }

        yaml_serde::to_string(&root).map_err(|e| ComposeError::InvalidComposeYaml {
            compose_source: "compose file".to_string(),
            reason: format!("failed to re-serialize compose file after excluding services: {e}"),
        })
    }

    /// Enumerate service names from parsed compose YAML (with merge keys
    /// expanded). Falls back to the line-based parser if the content is not
    /// valid YAML or has no `services:` mapping.
    /// Collect every path referenced by an `env_file:` key across all services.
    ///
    /// Handles all three shapes the Compose spec allows:
    /// `env_file: .env`, `env_file: [a.env, b.env]`, and the long form
    /// `env_file: [{path: .env, required: false}]`. Paths are returned exactly
    /// as written, de-duplicated, in first-seen order.
    ///
    /// Paths that cannot be confined to the project (absolute, `..`) are
    /// dropped here rather than propagated: the compose file may come from a
    /// repository the operator does not control, so an `env_file` entry must
    /// never be able to name a location outside the stack directory.
    pub fn collect_env_file_refs(compose_content: &str) -> Vec<String> {
        Self::raw_env_file_refs(compose_content)
            .into_iter()
            .filter(|path| Self::validate_relative_path(path, "env_file").is_ok())
            .collect()
    }

    /// Every `env_file` path exactly as written, de-duplicated, unvalidated.
    fn raw_env_file_refs(compose_content: &str) -> Vec<String> {
        let mut root: YamlValue = match yaml_serde::from_str(compose_content) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        let _ = root.apply_merge();
        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Vec::new();
        };

        let mut refs: Vec<String> = Vec::new();
        let push = |candidate: Option<&str>, refs: &mut Vec<String>| {
            let Some(path) = candidate else { return };
            if !refs.iter().any(|existing| existing == path) {
                refs.push(path.to_string());
            }
        };

        for service in services.values() {
            match service.get("env_file") {
                Some(YamlValue::String(single)) => push(Some(single.as_str()), &mut refs),
                Some(YamlValue::Sequence(entries)) => {
                    for entry in entries {
                        match entry {
                            YamlValue::String(path) => push(Some(path.as_str()), &mut refs),
                            // Long form: {path: .env, required: false}. `required`
                            // is Compose's own concern — Temps satisfies the
                            // reference either way, so the file exists whichever
                            // the author chose.
                            YamlValue::Mapping(_) => {
                                push(entry.get("path").and_then(YamlValue::as_str), &mut refs)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        refs
    }

    /// Resolve each referenced env file against the repository checkout,
    /// deciding whether it is copied from the repo or synthesized.
    ///
    /// Pure and side-effect free so the deployment job can log the same plan it
    /// is about to execute — an operator must be able to see that Temps created
    /// a file the repository never contained.
    pub fn plan_env_files(compose_content: &str, repo_dir: Option<&Path>) -> Vec<EnvFilePlan> {
        Self::plan_env_files_for_compose_file(compose_content, "docker-compose.yml", repo_dir)
    }

    /// `env_file` paths resolved the way Docker Compose resolves them:
    /// relative to the directory of the compose file, then expressed relative
    /// to the stack root. A compose file at `deploy/compose.yml` that names
    /// `env_file: .env.app` therefore reads `deploy/.env.app`, and
    /// `../.env` reads the root `.env`. A reference that leaves the stack
    /// root after resolution is dropped, as before.
    pub fn env_file_refs_for_compose_file(
        compose_content: &str,
        compose_file: &str,
    ) -> Vec<String> {
        let compose_dir = Path::new(compose_file)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut resolved_refs: Vec<String> = Vec::new();
        for path in Self::raw_env_file_refs(compose_content) {
            if path.starts_with('/') || path.starts_with('~') {
                continue;
            }
            let joined = if compose_dir.is_empty() {
                path
            } else {
                format!("{compose_dir}/{path}")
            };
            let resolved = Self::lexically_normalize(&joined);
            if resolved == "." || Self::validate_relative_path(&resolved, "env_file").is_err() {
                continue;
            }
            if !resolved_refs.contains(&resolved) {
                resolved_refs.push(resolved);
            }
        }
        resolved_refs
    }

    /// [`Self::plan_env_files`] for a compose file that may live in a
    /// subdirectory of the stack root. Plan paths are stack-root relative.
    pub fn plan_env_files_for_compose_file(
        compose_content: &str,
        compose_file: &str,
        repo_dir: Option<&Path>,
    ) -> Vec<EnvFilePlan> {
        Self::env_file_refs_for_compose_file(compose_content, compose_file)
            .into_iter()
            .map(|path| {
                let from_repo = repo_dir.and_then(|dir| {
                    // Confine the read for the same reason as the write: the
                    // path comes from the compose file, not from Temps.
                    let candidate = dir.join(&path);
                    let canonical_dir = std::fs::canonicalize(dir).ok()?;
                    let canonical = std::fs::canonicalize(&candidate).ok()?;
                    (canonical.starts_with(&canonical_dir) && canonical.is_file())
                        .then_some(canonical)
                });
                let source = match from_repo {
                    Some(repo_path) => EnvFileSource::Repository(repo_path),
                    None => EnvFileSource::ProjectEnvironment,
                };
                EnvFilePlan { path, source }
            })
            .collect()
    }

    pub(super) fn find_compose_file(&self, project_dir: &Path) -> String {
        // Docker Compose's own precedence, matching the deployment default.
        for name in &[
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ] {
            if project_dir.join(name).exists() {
                return name.to_string();
            }
        }
        "docker-compose.yml".to_string()
    }
}

#[cfg(test)]
mod tests;
