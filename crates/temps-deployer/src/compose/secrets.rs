// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Materialising per-service secret files and the override that mounts them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{ComposeError, ComposeExecutor, CONTAINER_SECRETS_DIR};
use tracing::warn;
use yaml_serde::Value as YamlValue;
use yaml_serde::{Mapping, Value};

impl ComposeExecutor {
    /// Root directory holding every stack's materialized secret files.
    ///
    /// Created `0700` so no other local user can traverse into it — that
    /// directory bit is the entire host-side protection for these files, since
    /// the per-stack directory below it has to stay traversable by whatever
    /// uid the container's image happens to run as.
    ///
    /// Deliberately *not* under [`Self::project_dir`]: the per-stack work
    /// directory is created `0755` by `write_compose_files` and shares its
    /// parent with every other stack, so it cannot be the confidentiality
    /// boundary. Mirrors `DockerRuntime`'s `$TEMPS_DATA_DIR/secrets` root,
    /// kept separate so a container name and a Compose project name can never
    /// collide on the same directory.
    fn secrets_root(&self) -> PathBuf {
        self.data_dir.join("compose-secrets")
    }

    /// Host directory bind-mounted at `/run/secrets` for every service in a
    /// stack. Lives under the Temps data dir rather than the repository
    /// checkout because git-backed deployments run Compose from an ephemeral
    /// checkout that is deleted as soon as the deploy job finishes — a mount
    /// source inside it would be gone by the first container restart.
    fn secrets_dir(&self, project_name: &str) -> PathBuf {
        self.secrets_root().join(project_name)
    }

    pub(super) fn secret_generation_dir(&self, project_name: &str, generation: &str) -> PathBuf {
        self.secrets_dir(project_name).join(generation)
    }

    /// Which secrets a given Compose service is entitled to read.
    ///
    /// A secret with no scope entry (or an empty one) is readable by every
    /// service. Scoping is opt-in: "not configured" must never mean "withheld",
    /// or enabling this feature would break stacks on upgrade.
    fn secrets_for_service<'a>(
        secrets: &'a HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        service: &str,
    ) -> Vec<(&'a String, &'a String)> {
        secrets
            .iter()
            .filter(|(key, _)| match scopes.get(*key) {
                None => true,
                Some(services) if services.is_empty() => true,
                Some(services) => services.iter().any(|s| s == service),
            })
            .collect()
    }

    /// Secret names a given service is entitled to read. Public so the deploy
    /// job can report the delivery matrix using exactly the same rule the
    /// executor applies when writing the files -- a second, drifting copy of
    /// this predicate would make the log lie.
    pub fn secret_names_for_service(
        secrets: &HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        service: &str,
    ) -> Vec<String> {
        Self::secrets_for_service(secrets, scopes, service)
            .into_iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Scoped service names that do not exist in the deployed stack.
    ///
    /// A Compose service name lives in the user's repository, so a rename
    /// silently strands any secret scoped to the old name. Callers surface
    /// this in the deployment log rather than letting delivery quietly stop.
    pub fn unmatched_secret_scopes(
        scopes: &HashMap<String, Vec<String>>,
        known_services: &[String],
    ) -> Vec<(String, String)> {
        let mut unmatched: Vec<(String, String)> = scopes
            .iter()
            .flat_map(|(key, services)| {
                services
                    .iter()
                    .filter(|service| !known_services.contains(service))
                    .map(move |service| (key.clone(), service.clone()))
            })
            .collect();
        unmatched.sort();
        unmatched
    }

    /// Materialize `secrets` as one directory per Compose service and return
    /// the per-service host directories to bind-mount.
    ///
    /// Per-service directories rather than one shared directory are what make
    /// scoping real: a service's mount can only expose files that were written
    /// into its own directory, so a service outside a secret's scope has no
    /// path to the value at all. Duplicating a value across the services
    /// entitled to it costs at most `SECRET_VALUE_MAX_BYTES` per copy.
    ///
    /// Only the candidate generation is removed and recreated. The generation
    /// mounted by the active stack remains untouched until replacement
    /// containers are healthy, so a slow or failed pull cannot rotate secrets
    /// underneath live containers.
    ///
    /// ### Permissions
    /// `0700` on the root, `0755` on each service directory, `0444` on each
    /// file. The single-container path chowns `0400` files to the uid resolved
    /// from the image's `USER`; that is not available here, because Compose
    /// pulls several of a stack's images during `up` -- after these files have
    /// to exist. World-readable *inside* the container is not a boundary worth
    /// defending (any process there already runs as the app), and on the host
    /// the `0700` root denies every other local user.
    pub(super) async fn materialize_secrets(
        &self,
        project_name: &str,
        generation: &str,
        secrets: &HashMap<String, String>,
        scopes: &HashMap<String, Vec<String>>,
        services: &[String],
    ) -> Result<HashMap<String, PathBuf>, ComposeError> {
        Self::validate_service_dir_name(generation)?;
        let root_for_project = self.secret_generation_dir(project_name, generation);

        // Clear unconditionally, even with no secrets: a project whose last
        // secret was just deleted must not keep serving the old file.
        if let Err(error) = tokio::fs::remove_dir_all(&root_for_project).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ComposeError::FileWriteFailed {
                    path: root_for_project.display().to_string(),
                    reason: format!("failed to clear previous secrets directory: {error}"),
                });
            }
        }

        if secrets.is_empty() {
            return Ok(HashMap::new());
        }

        // Docker splits a short-form bind spec on ':', so a data directory
        // containing one would silently mount the wrong host path (or nothing)
        // rather than fail. Refuse instead of delivering no secrets quietly.
        if root_for_project.to_string_lossy().contains(':') {
            return Err(ComposeError::FileWriteFailed {
                path: root_for_project.display().to_string(),
                reason: "the Temps data directory path contains ':', which Docker treats as a \
                         bind-mount field separator; secrets cannot be mounted from it. \
                         Move TEMPS_DATA_DIR to a path without a colon."
                    .to_string(),
            });
        }

        let root = self.secrets_root();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| ComposeError::FileWriteFailed {
                path: root.display().to_string(),
                reason: format!("failed to create secrets root: {e}"),
            })?;
        Self::set_mode(&root, 0o700).await?;

        let mut mounts = HashMap::new();
        for service in services {
            let entitled = Self::secrets_for_service(secrets, scopes, service);
            if entitled.is_empty() {
                continue;
            }

            // The service name becomes a directory component. Compose service
            // names come from a repository file, so this is validated here and
            // not merely trusted from the API layer.
            Self::validate_service_dir_name(service)?;
            let dir = root_for_project.join(service);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: dir.display().to_string(),
                    reason: format!("failed to create secrets directory: {e}"),
                })?;
            Self::set_mode(&dir, 0o755).await?;

            for (key, value) in entitled {
                // `SecretService::validate_secret_key` already enforces this at
                // the API. Re-checked because this value becomes a path.
                Self::validate_secret_file_name(key)?;
                let path = dir.join(key);
                tokio::fs::write(&path, value).await.map_err(|e| {
                    ComposeError::FileWriteFailed {
                        path: path.display().to_string(),
                        // Never interpolate `value` -- this reaches the log.
                        reason: format!("failed to write secret '{key}': {e}"),
                    }
                })?;
                Self::set_mode(&path, 0o444).await?;
            }

            mounts.insert(service.clone(), dir);
        }

        // `0700` on the per-project root too, so an unscoped service cannot
        // even enumerate the sibling directories it has no mount for.
        Self::set_mode(&root_for_project, 0o700).await?;

        Ok(mounts)
    }

    /// Reject a Compose service name that cannot be used as a single path
    /// component.
    pub(super) fn validate_service_dir_name(service: &str) -> Result<(), ComposeError> {
        let invalid = service.is_empty()
            || service == "."
            || service == ".."
            || service.contains('/')
            || service.contains('\\')
            || service.contains('\0');
        if invalid {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: "service name".to_string(),
                reason: "compose service name cannot be used as a directory name; \
                         it must not be empty, '.', '..', or contain path separators"
                    .to_string(),
            });
        }
        Ok(())
    }

    #[cfg_attr(not(unix), allow(unused_variables))]
    pub(super) async fn set_mode(path: &Path, mode: u32) -> Result<(), ComposeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .await
                .map_err(|e| ComposeError::FileWriteFailed {
                    path: path.display().to_string(),
                    reason: format!("failed to set mode {mode:o}: {e}"),
                })?;
        }
        Ok(())
    }

    /// Reject a secret key that cannot be used as a single filename.
    ///
    /// Mirrors `SecretService::validate_secret_key` rather than merely
    /// excluding path separators: this runs on a value read back from the
    /// database, so it must not assume the API layer was the only writer.
    /// Anything outside `[A-Za-z0-9_]` (leading digit included) is rejected,
    /// which subsumes `.`, `..`, and every path separator.
    fn validate_secret_file_name(key: &str) -> Result<(), ComposeError> {
        let valid = !key.is_empty()
            && key.len() <= 255
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<secrets>".to_string(),
                field: "secret key".to_string(),
                reason: format!(
                    "secret key '{key}' is not a valid filename; keys must start with a \
                     letter or underscore and contain only letters, digits and underscores"
                ),
            });
        }
        Ok(())
    }

    /// Services that already put something at `/run/secrets` themselves —
    /// either an explicit volume mount or Compose's own `secrets:` key, which
    /// mounts each entry at `/run/secrets/<name>`.
    ///
    /// Temps skips injection for these rather than mounting on top: Compose
    /// *appends* volume lists across `-f` files instead of replacing them, so
    /// two mounts at the same target abort `up` for the whole stack. Losing
    /// secret delivery for one service is recoverable and reported; failing
    /// the deployment of an otherwise-valid stack is not.
    pub fn services_managing_own_secrets(compose_documents: &[&str]) -> HashSet<String> {
        let mut conflicting = HashSet::new();
        for document in compose_documents {
            if document.trim().is_empty() {
                continue;
            }
            let Ok(mut root) = yaml_serde::from_str::<YamlValue>(document) else {
                continue;
            };
            let _ = root.apply_merge();
            let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, service) in services {
                let Some(name) = name.as_str() else { continue };
                if service.get("secrets").is_some() {
                    conflicting.insert(name.to_string());
                    continue;
                }
                let Some(volumes) = service.get("volumes").and_then(YamlValue::as_sequence) else {
                    continue;
                };
                if volumes.iter().any(Self::mounts_container_secrets_dir) {
                    conflicting.insert(name.to_string());
                }
            }
        }
        conflicting
    }

    /// Whether a single `volumes:` entry targets `/run/secrets` (or a path
    /// inside it), in either the short `src:dst:opts` form or the long
    /// `{target: ...}` form.
    fn mounts_container_secrets_dir(entry: &YamlValue) -> bool {
        let target = match entry {
            // Short form. The target is the second colon-separated field;
            // a single-field entry (`- /run/secrets`) is an anonymous volume
            // whose target is the whole string.
            YamlValue::String(spec) => {
                let mut parts = spec.split(':');
                let first = parts.next().unwrap_or_default();
                parts.next().unwrap_or(first).to_string()
            }
            YamlValue::Mapping(_) => match entry.get("target").and_then(YamlValue::as_str) {
                Some(target) => target.to_string(),
                None => return false,
            },
            _ => return false,
        };
        let target = target.trim_end_matches('/');
        target == CONTAINER_SECRETS_DIR || target.starts_with(&format!("{CONTAINER_SECRETS_DIR}/"))
    }

    /// Every service name across the base compose document and the user
    /// override, de-duplicated in first-seen order.
    ///
    /// The override is a full compose document, so it can introduce services
    /// the base file never mentions. Enumerating only the base file would
    /// leave those with no secrets while the deployment log claimed every
    /// service got them.
    pub fn all_service_names(&self, compose_documents: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for document in compose_documents {
            if document.trim().is_empty() {
                continue;
            }
            for name in self.parse_service_names_yaml(document) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Generate the override that mounts each service's own secrets directory
    /// read-only at `/run/secrets`. Returns the YAML and the services covered.
    ///
    /// Every service gets a *different* mount source, so a service outside a
    /// secret's scope has no filesystem path to that value at all -- the
    /// scoping is enforced by what was written, not by the container.
    pub(super) fn generate_secrets_override(
        &self,
        mounts: &HashMap<String, PathBuf>,
        skip_services: &HashSet<String>,
    ) -> (String, Vec<String>) {
        let mut mounted: Vec<String> = mounts
            .keys()
            .filter(|service| !skip_services.contains(*service))
            .cloned()
            .collect();
        // Deterministic output so a redeploy with unchanged inputs produces a
        // byte-identical override.
        mounted.sort();

        let mut services_map = Mapping::new();
        for service in &mounted {
            let Some(host_dir) = mounts.get(service) else {
                continue;
            };
            let mount = format!(
                "{}:{}:ro",
                host_dir.to_string_lossy(),
                CONTAINER_SECRETS_DIR
            );
            let mut service_map = Mapping::new();
            service_map.insert(
                Value::String("volumes".to_string()),
                Value::Sequence(vec![Value::String(mount)]),
            );
            services_map.insert(Value::String(service.clone()), Value::Mapping(service_map));
        }

        if services_map.is_empty() {
            return (String::new(), mounted);
        }

        let mut root = Mapping::new();
        root.insert(
            Value::String("services".to_string()),
            Value::Mapping(services_map),
        );
        // Built through yaml_serde rather than string formatting so a service
        // name or host path containing YAML metacharacters is quoted, not
        // injected as structure.
        (
            yaml_serde::to_string(&Value::Mapping(root)).unwrap_or_default(),
            mounted,
        )
    }

    /// Remove a stack's materialized secret files from the host.
    pub(super) async fn remove_secrets_dir(&self, project_name: &str) {
        let dir = self.secrets_dir(project_name);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    project = %project_name,
                    path = %dir.display(),
                    error = %error,
                    "Failed to remove materialized secrets directory"
                );
            }
        }
    }

    /// Remove only one not-yet-promoted secret generation. This is used when
    /// preparation fails or is cancelled while the previous stack is still
    /// serving; deleting the whole project directory would invalidate the
    /// bind mounts of that live stack.
    pub async fn cleanup_secret_generation(
        &self,
        project_name: &str,
        generation: &str,
    ) -> Result<(), ComposeError> {
        Self::validate_service_dir_name(generation)?;
        let dir = self.secret_generation_dir(project_name, generation);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ComposeError::FileWriteFailed {
                path: dir.display().to_string(),
                reason: format!("failed to remove candidate secret generation: {error}"),
            }),
        }
    }

    /// Keep one candidate's secret generation and remove every older one.
    ///
    /// Callers may use this either after the replacement containers become
    /// healthy or after the previous stack has been stopped. In both cases no
    /// live container may still depend on the generations being removed.
    pub async fn prune_secret_generations(
        &self,
        project_name: &str,
        generation: Option<&str>,
    ) -> Result<(), ComposeError> {
        if let Some(generation) = generation {
            Self::validate_service_dir_name(generation)?;
        }
        let project_dir = self.secrets_dir(project_name);
        let mut entries = match tokio::fs::read_dir(&project_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(ComposeError::FileWriteFailed {
                    path: project_dir.display().to_string(),
                    reason: format!("failed to list secret generations: {error}"),
                });
            }
        };
        while let Some(entry) =
            entries
                .next_entry()
                .await
                .map_err(|error| ComposeError::FileWriteFailed {
                    path: project_dir.display().to_string(),
                    reason: format!("failed to inspect secret generations: {error}"),
                })?
        {
            if generation.is_some_and(|current| entry.file_name() == current) {
                continue;
            }
            let path = entry.path();
            tokio::fs::remove_dir_all(&path).await.map_err(|error| {
                ComposeError::FileWriteFailed {
                    path: path.display().to_string(),
                    reason: format!("failed to remove obsolete secret generation: {error}"),
                }
            })?;
        }
        if generation.is_none() {
            match tokio::fs::remove_dir(&project_dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ComposeError::FileWriteFailed {
                        path: project_dir.display().to_string(),
                        reason: format!("failed to remove empty secret project directory: {error}"),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::test_support::{one_secret, secrets_test_request, test_executor};
    use crate::compose::{
        ComposeDeployRequest, ComposeError, ComposeExecutor, TEMPS_SECRETS_OVERRIDE,
    };
    use bollard::Docker;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_compose_secrets_are_materialized_and_mounted_read_only() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  worker:\n    image: nginx\n",
            one_secret("DB_PASSWORD", "hunter2"),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let secret_file = executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("web")
            .join("DB_PASSWORD");
        assert_eq!(
            tokio::fs::read_to_string(&secret_file).await.unwrap(),
            "hunter2"
        );

        let override_yaml =
            tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
                .await
                .unwrap();
        let parsed: YamlValue = yaml_serde::from_str(&override_yaml).unwrap();
        for service in ["web", "worker"] {
            let volumes = parsed["services"][service]["volumes"]
                .as_sequence()
                .unwrap();
            assert_eq!(volumes.len(), 1);
            let mount = volumes[0].as_str().unwrap();
            assert!(mount.ends_with(":/run/secrets:ro"), "mount was {mount}");
            assert!(mount.starts_with(
                &executor
                    .secret_generation_dir("temps-1-2", "test-generation")
                    .to_string_lossy()
                    .to_string()
            ));
        }

        // The plaintext must not leak into any other generated artifact.
        assert!(!override_yaml.contains("hunter2"));
        let mut entries = tokio::fs::read_dir(project_dir.path()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.file_type().await.unwrap().is_file() {
                let body = tokio::fs::read_to_string(entry.path()).await.unwrap();
                assert!(
                    !body.contains("hunter2"),
                    "secret leaked into {}",
                    entry.path().display()
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_compose_secrets_root_is_not_traversable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "s3cr3t"),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        // The 0700 root is the whole host-side boundary: the per-stack
        // directory below it must stay traversable by the container's uid.
        let root_mode = tokio::fs::metadata(executor.secrets_root())
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(root_mode, 0o700);

        let file_mode = tokio::fs::metadata(
            executor
                .secret_generation_dir("temps-1-2", "test-generation")
                .join("web")
                .join("TOKEN"),
        )
        .await
        .unwrap()
        .permissions()
        .mode()
            & 0o222;
        assert_eq!(file_mode, 0, "secret files must not be writable");
    }

    #[tokio::test]
    async fn test_candidate_secret_generation_preserves_active_generation_until_promotion() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", "active")),
                "active-generation",
            )
            .await
            .unwrap();
        executor
            .write_compose_files(
                project_dir.path(),
                &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", "candidate")),
                "candidate-generation",
            )
            .await
            .unwrap();

        let active = executor
            .secret_generation_dir("temps-1-2", "active-generation")
            .join("web/TOKEN");
        let candidate = executor
            .secret_generation_dir("temps-1-2", "candidate-generation")
            .join("web/TOKEN");
        assert_eq!(tokio::fs::read_to_string(&active).await.unwrap(), "active");
        assert_eq!(
            tokio::fs::read_to_string(&candidate).await.unwrap(),
            "candidate"
        );

        executor
            .cleanup_secret_generation("temps-1-2", "candidate-generation")
            .await
            .unwrap();
        assert!(
            active.exists(),
            "cancelling preparation removed active secrets"
        );
        assert!(
            !candidate.exists(),
            "candidate plaintext survived cancellation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_preparation_failure_cleans_candidate_but_preserves_active_secrets() {
        use std::os::unix::fs::symlink;

        let docker = Docker::connect_with_local_defaults()
            .expect("constructing the Docker client should not contact the daemon");
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let active_request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n",
            one_secret("TOKEN", "active"),
        );
        executor
            .write_compose_files(project_dir.path(), &active_request, "active-generation")
            .await
            .unwrap();

        symlink("/", project_dir.path().join("escape")).unwrap();
        let mut candidate_request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n    volumes:\n      - ./escape:/data\n",
            one_secret("TOKEN", "candidate"),
        );
        candidate_request.repo_dir = Some(project_dir.path().to_path_buf());

        let error = match executor
            .prepare_and_pull_for_generation(&candidate_request, "candidate-generation")
            .await
        {
            Ok(_) => panic!("symlink escape should fail before pull"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation { .. }
        ));

        let active = executor
            .secret_generation_dir("temps-1-2", "active-generation")
            .join("web/TOKEN");
        assert_eq!(tokio::fs::read_to_string(active).await.unwrap(), "active");
        assert!(!executor
            .secret_generation_dir("temps-1-2", "candidate-generation")
            .exists());
    }

    #[tokio::test]
    async fn test_pruning_secret_generations_removes_only_obsolete_plaintext() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let compose = "services:\n  web:\n    image: nginx\n";

        for (generation, value) in [("old", "old-secret"), ("current", "new-secret")] {
            executor
                .write_compose_files(
                    project_dir.path(),
                    &secrets_test_request("temps-1-2", compose, one_secret("TOKEN", value)),
                    generation,
                )
                .await
                .unwrap();
        }

        executor
            .prune_secret_generations("temps-1-2", Some("current"))
            .await
            .unwrap();
        assert!(!executor.secret_generation_dir("temps-1-2", "old").exists());
        assert!(executor
            .secret_generation_dir("temps-1-2", "current")
            .join("web/TOKEN")
            .exists());
    }

    fn scoped_request(
        secrets: HashMap<String, String>,
        scopes: HashMap<String, Vec<String>>,
    ) -> ComposeDeployRequest {
        let mut request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres:18\n",
            secrets,
        );
        request.secret_compose_services = scopes;
        request
    }

    #[tokio::test]
    async fn test_scoped_secret_is_only_written_for_entitled_services() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = scoped_request(
            HashMap::from([
                ("APP_ONLY".to_string(), "app-value".to_string()),
                ("SHARED".to_string(), "shared-value".to_string()),
            ]),
            HashMap::from([("APP_ONLY".to_string(), vec!["web".to_string()])]),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let stack = executor.secret_generation_dir("temps-1-2", "test-generation");
        // The scoped secret exists only under the entitled service. This is
        // the whole point: `db` has no filesystem path to the value, so it is
        // not merely "not mounted" -- it was never written for that service.
        assert!(stack.join("web").join("APP_ONLY").exists());
        assert!(!stack.join("db").join("APP_ONLY").exists());
        // The unscoped secret still reaches everyone.
        assert!(stack.join("web").join("SHARED").exists());
        assert!(stack.join("db").join("SHARED").exists());
    }

    #[tokio::test]
    async fn test_service_entitled_to_nothing_gets_no_mount_at_all() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();
        let request = scoped_request(
            one_secret("APP_ONLY", "app-value"),
            HashMap::from([("APP_ONLY".to_string(), vec!["web".to_string()])]),
        );

        executor
            .write_compose_files(project_dir.path(), &request, "test-generation")
            .await
            .unwrap();

        let parsed: YamlValue = yaml_serde::from_str(
            &tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(parsed["services"]["web"].is_mapping());
        // An empty /run/secrets mount would imply "this app has no secrets";
        // no mount at all is the honest representation.
        assert!(parsed["services"]["db"].is_null());
        assert!(!executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("db")
            .exists());
    }

    #[tokio::test]
    async fn test_narrowing_a_scope_removes_the_previous_service_copy() {
        let Some(docker) = Docker::connect_with_defaults().ok() else {
            return;
        };
        let data_dir = tempfile::tempdir().unwrap();
        let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
        let project_dir = tempfile::tempdir().unwrap();

        // Deploy once unscoped, so both services hold a copy.
        executor
            .write_compose_files(
                project_dir.path(),
                &scoped_request(one_secret("TOKEN", "value"), HashMap::new()),
                "test-generation",
            )
            .await
            .unwrap();
        assert!(executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("db")
            .join("TOKEN")
            .exists());

        // Then narrow it to `web` only. Revoking access must actually delete
        // the plaintext the other service already had on disk.
        executor
            .write_compose_files(
                project_dir.path(),
                &scoped_request(
                    one_secret("TOKEN", "value"),
                    HashMap::from([("TOKEN".to_string(), vec!["web".to_string()])]),
                ),
                "test-generation",
            )
            .await
            .unwrap();

        assert!(executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .join("web")
            .join("TOKEN")
            .exists());
        assert!(
            !executor
                .secret_generation_dir("temps-1-2", "test-generation")
                .join("db")
                .exists(),
            "narrowing a scope left the revoked service's plaintext on disk"
        );
    }

    #[test]
    fn test_empty_scope_means_every_service_not_no_service() {
        let secrets = one_secret("TOKEN", "value");
        // Both shapes a caller can produce for "unconfigured".
        for scopes in [
            HashMap::new(),
            HashMap::from([("TOKEN".to_string(), Vec::<String>::new())]),
        ] {
            let names = ComposeExecutor::secret_names_for_service(&secrets, &scopes, "anything");
            assert_eq!(
                names,
                vec!["TOKEN".to_string()],
                "an unconfigured scope must not withhold the secret"
            );
        }
    }

    #[test]
    fn test_unmatched_scopes_are_reported_for_renamed_services() {
        let scopes = HashMap::from([
            ("TOKEN".to_string(), vec!["worker".to_string()]),
            ("OTHER".to_string(), vec!["web".to_string()]),
        ]);
        let known = vec!["web".to_string(), "db".to_string()];

        let unmatched = ComposeExecutor::unmatched_secret_scopes(&scopes, &known);

        assert_eq!(
            unmatched,
            vec![("TOKEN".to_string(), "worker".to_string())],
            "a scope naming a service that no longer exists must be reported"
        );
    }

    #[test]
    fn test_service_names_that_are_not_path_components_are_rejected() {
        for service in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(
                ComposeExecutor::validate_service_dir_name(service).is_err(),
                "service {service:?} should be rejected"
            );
        }
        assert!(ComposeExecutor::validate_service_dir_name("web-1.api_v2").is_ok());
    }

    #[test]
    fn test_all_service_names_merges_both_documents_in_order() {
        let Some(executor) = test_executor() else {
            return;
        };

        let names = executor.all_service_names(&[
            "services:\n  web:\n    image: nginx\n  shared:\n    image: nginx\n",
            "",
            "services:\n  shared:\n    image: nginx\n  extra:\n    image: nginx\n",
        ]);

        assert_eq!(names, vec!["web", "shared", "extra"]);
    }

    #[test]
    fn test_secret_keys_that_are_not_filenames_are_rejected() {
        for key in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            "1LEADING_DIGIT",
            "has space",
            "has-dash",
            "has:colon",
            "..\\..\\etc\\passwd",
        ] {
            assert!(
                ComposeExecutor::validate_secret_file_name(key).is_err(),
                "key {key:?} should be rejected"
            );
        }
        assert!(ComposeExecutor::validate_secret_file_name("DB_PASSWORD").is_ok());
    }

    #[test]
    fn test_services_managing_own_secrets_are_detected() {
        let compose = r#"
services:
  short_form:
    image: nginx
    volumes:
      - ./local:/run/secrets:ro
  long_form:
    image: nginx
    volumes:
      - type: bind
        source: ./local
        target: /run/secrets/nested
  compose_secrets:
    image: nginx
    secrets:
      - db_password
  clean:
    image: nginx
    volumes:
      - ./data:/var/lib/data
"#;
        let detected = ComposeExecutor::services_managing_own_secrets(&[compose]);

        assert!(detected.contains("short_form"));
        assert!(detected.contains("long_form"));
        assert!(detected.contains("compose_secrets"));
        assert!(!detected.contains("clean"));
    }
}
