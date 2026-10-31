// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Filesystem confinement: every host path a compose document uses must stay
//! inside the project directory.

use std::path::{Component, Path, PathBuf};
use temps_entities::compose_security::{
    ComposeSecurityCheck as PolicyCheck, ComposeSecurityPolicy,
};

use super::{ComposeError, ComposeExecutor};
use yaml_serde::Mapping;
use yaml_serde::Value as YamlValue;

impl ComposeExecutor {
    /// Lexically normalize a path: collapse `.` and resolve `..` without
    /// touching the filesystem. Relative `..` that escapes the base is kept as
    /// a leading `..` so callers can detect project-directory escape.
    pub(crate) fn lexically_normalize(source: &str) -> String {
        let is_absolute = source.starts_with('/');
        let mut stack: Vec<&str> = Vec::new();
        for comp in source.split('/') {
            match comp {
                "" | "." => {}
                ".." => match stack.last() {
                    Some(&last) if last != ".." => {
                        stack.pop();
                    }
                    _ => {
                        // For absolute paths, `..` at the root is a no-op.
                        if !is_absolute {
                            stack.push("..");
                        }
                    }
                },
                other => stack.push(other),
            }
        }
        let joined = stack.join("/");
        if is_absolute {
            format!("/{joined}")
        } else if joined.is_empty() {
            ".".to_string()
        } else {
            joined
        }
    }

    /// Resolve every repository-backed path used by Compose and verify the
    /// canonical target remains inside the checked-out project. Relative-path
    /// string checks alone cannot detect a committed symlink such as
    /// `data -> /`, which Docker follows when it opens a bind source.
    #[cfg(test)]
    fn validate_compose_filesystem_confinement(
        project_dir: &Path,
        compose_file: &str,
        source: &str,
        compose_content: &str,
    ) -> Result<(), ComposeError> {
        Self::validate_compose_filesystem_confinement_with_policy(
            project_dir,
            compose_file,
            source,
            compose_content,
            &ComposeSecurityPolicy::default(),
        )
    }

    pub(super) fn validate_compose_filesystem_confinement_with_policy(
        project_dir: &Path,
        compose_file: &str,
        source: &str,
        compose_content: &str,
        policy: &ComposeSecurityPolicy,
    ) -> Result<(), ComposeError> {
        if compose_content.trim().is_empty() {
            return Ok(());
        }
        Self::validate_relative_path(compose_file, "compose_path")?;

        let canonical_root =
            std::fs::canonicalize(project_dir).map_err(|e| ComposeError::InvalidComposePath {
                field: "compose project directory".to_string(),
                path: project_dir.display().to_string(),
                reason: format!("failed to canonicalize project directory: {e}"),
            })?;
        let compose_path = canonical_root.join(compose_file);
        let compose_base =
            compose_path
                .parent()
                .ok_or_else(|| ComposeError::InvalidComposePath {
                    field: "compose_path".to_string(),
                    path: compose_file.to_string(),
                    reason: "compose path has no parent directory".to_string(),
                })?;
        let compose_base =
            std::fs::canonicalize(compose_base).map_err(|e| ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.to_string(),
                reason: format!("failed to canonicalize compose base directory: {e}"),
            })?;
        if !compose_base.starts_with(&canonical_root) {
            return Err(ComposeError::InvalidComposePath {
                field: "compose_path".to_string(),
                path: compose_file.to_string(),
                reason: "compose base directory resolves outside the project directory".to_string(),
            });
        }

        let mut root: YamlValue = yaml_serde::from_str(compose_content).map_err(|e| {
            ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: e.to_string(),
            }
        })?;
        root.apply_merge()
            .map_err(|e| ComposeError::InvalidComposeYaml {
                compose_source: source.to_string(),
                reason: format!("failed to expand YAML merge keys: {e}"),
            })?;

        for key in ["configs", "secrets"] {
            if !policy.enforced(if key == "configs" {
                PolicyCheck::ConfigPaths
            } else {
                PolicyCheck::SecretPaths
            }) {
                continue;
            }
            let Some(files) = root.get(key).and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, definition) in files {
                let name = name.as_str().unwrap_or("<unknown>");
                let Some(file) = definition
                    .as_mapping()
                    .and_then(|mapping| mapping.get(YamlValue::String("file".to_string())))
                    .and_then(YamlValue::as_str)
                else {
                    continue;
                };
                Self::canonicalize_confined_existing_path(
                    &canonical_root,
                    &compose_base,
                    file,
                    &format!("{key}.{name}"),
                    &format!("{key}.file"),
                )?;
            }
        }

        // Local-driver bind volumes can hide a repository symlink in
        // `driver_opts.device` and then expose it through an apparently named
        // volume. Validate relative bind devices against the same compose base.
        if let Some(volumes) = root.get("volumes").and_then(YamlValue::as_mapping) {
            for (name, definition) in volumes {
                let name = name.as_str().unwrap_or("<unknown>");
                let Some(options) = definition
                    .as_mapping()
                    .and_then(|mapping| mapping.get(YamlValue::String("driver_opts".to_string())))
                    .and_then(YamlValue::as_mapping)
                else {
                    continue;
                };
                let is_bind = options
                    .get(YamlValue::String("type".to_string()))
                    .and_then(YamlValue::as_str)
                    == Some("none")
                    || options
                        .get(YamlValue::String("o".to_string()))
                        .and_then(YamlValue::as_str)
                        .is_some_and(|value| value.split(',').any(|option| option == "bind"));
                if !is_bind {
                    continue;
                }
                if let Some(device) = options
                    .get(YamlValue::String("device".to_string()))
                    .and_then(YamlValue::as_str)
                {
                    Self::validate_socket_mount(
                        &compose_base,
                        device,
                        &format!("volumes.{name}"),
                        policy,
                    )?;
                    if !policy.enforced(PolicyCheck::VolumeHostPaths) {
                        continue;
                    }
                    Self::canonicalize_confined_existing_path(
                        &canonical_root,
                        &compose_base,
                        device,
                        &format!("volumes.{name}"),
                        "volumes.driver_opts.device",
                    )?;
                }
            }
        }

        let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
            return Ok(());
        };
        for (service_name, definition) in services {
            let service_name = service_name.as_str().unwrap_or("<unknown>");
            let Some(service) = definition.as_mapping() else {
                continue;
            };

            if let Some(entries) = service
                .get(YamlValue::String("volumes".to_string()))
                .and_then(YamlValue::as_sequence)
            {
                for entry in entries {
                    let Some(bind_source) = Self::volume_source(entry) else {
                        continue;
                    };
                    if Self::is_named_volume_ref(&bind_source)
                        && !Self::is_explicit_bind_mount(entry)
                    {
                        continue;
                    }
                    Self::validate_socket_mount(&compose_base, &bind_source, service_name, policy)?;
                    if !policy.enforced(if Self::socket_source(&bind_source) {
                        PolicyCheck::DockerSocket
                    } else {
                        PolicyCheck::BindMounts
                    }) {
                        continue;
                    }
                    Self::validate_confined_bind_path(
                        &canonical_root,
                        &compose_base,
                        &bind_source,
                        service_name,
                        "volumes",
                    )?;
                }
            }

            Self::validate_build_filesystem_paths_with_policy(
                &canonical_root,
                &compose_base,
                service,
                service_name,
                policy,
            )?;
        }

        Ok(())
    }

    fn canonicalize_confined_existing_path(
        canonical_root: &Path,
        base_dir: &Path,
        raw_path: &str,
        service: &str,
        field: &str,
    ) -> Result<PathBuf, ComposeError> {
        if Self::is_dangerous_host_path(raw_path) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' must be a confined, non-interpolated relative path"
                ),
            });
        }

        let candidate = base_dir.join(raw_path);
        let canonical = std::fs::canonicalize(&candidate).map_err(|e| {
            ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' must already exist and be canonicalizable before deployment: {e}"
                ),
            }
        })?;
        if !canonical.starts_with(canonical_root) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' resolves outside the compose project directory"
                ),
            });
        }
        Ok(canonical)
    }

    /// Validate a service bind source against the Compose project without
    /// requiring its final directory to exist yet.
    ///
    /// Docker Compose short volume syntax creates a missing relative host
    /// directory. Rejecting that case breaks common upstream stacks such as
    /// Paperless (`./export` and `./consume`). We still inspect and canonicalize
    /// every existing prefix, including dangling symlinks, so an absent leaf
    /// cannot disguise a path that escapes through a committed symlink.
    pub(super) fn validate_confined_bind_path(
        canonical_root: &Path,
        base_dir: &Path,
        raw_path: &str,
        service: &str,
        field: &str,
    ) -> Result<PathBuf, ComposeError> {
        if Self::is_dangerous_host_path(raw_path) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: service.to_string(),
                field: field.to_string(),
                reason: format!(
                    "host path '{raw_path}' must be a confined, non-interpolated relative path"
                ),
            });
        }

        let normalized = Self::lexically_normalize(raw_path);
        let mut cursor = base_dir.to_path_buf();
        for component in Path::new(&normalized).components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(name) => cursor.push(name),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service.to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "host path '{raw_path}' must remain inside the compose project directory"
                        ),
                    });
                }
            }

            match std::fs::symlink_metadata(&cursor) {
                Ok(_) => {
                    cursor = std::fs::canonicalize(&cursor).map_err(|error| {
                        ComposeError::SecurityPolicyViolation {
                            service: service.to_string(),
                            field: field.to_string(),
                            reason: format!(
                                "host path '{raw_path}' contains an existing path that cannot be canonicalized: {error}"
                            ),
                        }
                    })?;
                    if !cursor.starts_with(canonical_root) {
                        return Err(ComposeError::SecurityPolicyViolation {
                            service: service.to_string(),
                            field: field.to_string(),
                            reason: format!(
                                "host path '{raw_path}' resolves outside the compose project directory"
                            ),
                        });
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Once a prefix is absent, no deeper path can exist. Compose
                    // may safely create the remaining confined path at `up`.
                    return Ok(base_dir.join(normalized));
                }
                Err(error) => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: service.to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "host path '{raw_path}' could not be inspected before deployment: {error}"
                        ),
                    });
                }
            }
        }

        Ok(cursor)
    }

    fn validate_build_filesystem_paths_with_policy(
        canonical_root: &Path,
        compose_base: &Path,
        service: &Mapping,
        service_name: &str,
        policy: &ComposeSecurityPolicy,
    ) -> Result<(), ComposeError> {
        let Some(build) = service.get(YamlValue::String("build".to_string())) else {
            return Ok(());
        };

        let (context, dockerfile, has_inline_dockerfile) = if let Some(context) = build.as_str() {
            (context, None, false)
        } else if let Some(build_map) = build.as_mapping() {
            let context = build_map
                .get(YamlValue::String("context".to_string()))
                .and_then(YamlValue::as_str)
                .unwrap_or(".");
            let dockerfile = build_map
                .get(YamlValue::String("dockerfile".to_string()))
                .and_then(YamlValue::as_str);
            let has_inline =
                build_map.contains_key(YamlValue::String("dockerfile_inline".to_string()));
            (context, dockerfile, has_inline)
        } else {
            return Ok(());
        };

        // Remote Git/HTTP build contexts do not resolve against the host
        // checkout. Local/file schemes are intentionally not exempted.
        if Self::is_remote_build_context(context) {
            return Ok(());
        }

        let canonical_context = if !policy.enforced(PolicyCheck::BuildContext) {
            compose_base.join(context)
        } else {
            Self::canonicalize_confined_existing_path(
                canonical_root,
                compose_base,
                context,
                service_name,
                "build.context",
            )?
        };
        if !policy.enforced(PolicyCheck::Dockerfile) {
            return Ok(());
        }
        if let Some(dockerfile) = dockerfile {
            Self::canonicalize_confined_existing_path(
                canonical_root,
                &canonical_context,
                dockerfile,
                service_name,
                "build.dockerfile",
            )?;
        } else if !has_inline_dockerfile {
            let default_dockerfile = canonical_context.join("Dockerfile");
            if default_dockerfile.exists() {
                Self::canonicalize_confined_existing_path(
                    canonical_root,
                    &canonical_context,
                    "Dockerfile",
                    service_name,
                    "build.dockerfile",
                )?;
            }
        }

        Ok(())
    }

    pub(super) fn is_remote_build_context(context: &str) -> bool {
        ["https://", "http://", "git://", "ssh://"]
            .iter()
            .any(|prefix| context.starts_with(prefix))
            || context.starts_with("git@")
    }

    /// Confine a user-supplied path (e.g. `compose_path`) to the project
    /// checkout directory: reject empty values, absolute paths, and any `..`
    /// / root / prefix component that would escape the project tree.
    pub(super) fn validate_relative_path(path: &str, field: &str) -> Result<(), ComposeError> {
        let candidate = Path::new(path);
        if candidate.as_os_str().is_empty() || candidate.is_absolute() || path.starts_with('~') {
            return Err(ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: path.to_string(),
                reason: "must be a non-empty relative path without home-directory expansion"
                    .to_string(),
            });
        }
        if candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: path.to_string(),
                reason: "must not contain '..' or absolute/root path components".to_string(),
            });
        }
        Ok(())
    }

    /// Return a canonical destination for a file Temps writes into the checked
    /// out repository, rejecting committed symlinks in either the destination
    /// or any parent directory. Git preserves symlinks, so a repository could
    /// otherwise make `.env.temps -> /host/file` and turn a normal deployment
    /// into an arbitrary host-file overwrite.
    pub(super) fn confined_write_path(
        project_dir: &Path,
        relative_path: &Path,
        field: &str,
    ) -> Result<PathBuf, ComposeError> {
        let relative = relative_path
            .to_str()
            .ok_or_else(|| ComposeError::InvalidComposePath {
                field: field.to_string(),
                path: relative_path.display().to_string(),
                reason: "path must be valid UTF-8".to_string(),
            })?;
        Self::validate_relative_path(relative, field)?;

        let canonical_root =
            std::fs::canonicalize(project_dir).map_err(|e| ComposeError::FileWriteFailed {
                path: project_dir.display().to_string(),
                reason: format!("failed to canonicalize compose project directory: {e}"),
            })?;

        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        let mut canonical_parent = canonical_root.clone();
        for component in parent.components() {
            match component {
                Component::CurDir => continue,
                Component::Normal(name) => canonical_parent.push(name),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ComposeError::InvalidComposePath {
                        field: field.to_string(),
                        path: relative_path.display().to_string(),
                        reason: "write destination must remain inside the compose project"
                            .to_string(),
                    });
                }
            }

            match std::fs::symlink_metadata(&canonical_parent) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ComposeError::SecurityPolicyViolation {
                        service: "<compose-files>".to_string(),
                        field: field.to_string(),
                        reason: format!(
                            "refusing to write through repository symlink '{}'",
                            canonical_parent.display()
                        ),
                    });
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(ComposeError::FileWriteFailed {
                        path: canonical_parent.display().to_string(),
                        reason: "compose write parent exists but is not a directory".to_string(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&canonical_parent).map_err(|e| {
                        ComposeError::FileWriteFailed {
                            path: canonical_parent.display().to_string(),
                            reason: format!("failed to create confined compose directory: {e}"),
                        }
                    })?;
                }
                Err(error) => {
                    return Err(ComposeError::FileWriteFailed {
                        path: canonical_parent.display().to_string(),
                        reason: format!("failed to inspect compose write parent: {error}"),
                    });
                }
            }
        }

        let canonical_parent = std::fs::canonicalize(&canonical_parent).map_err(|e| {
            ComposeError::FileWriteFailed {
                path: canonical_parent.display().to_string(),
                reason: format!("failed to canonicalize compose write parent: {e}"),
            }
        })?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(ComposeError::SecurityPolicyViolation {
                service: "<compose-files>".to_string(),
                field: field.to_string(),
                reason: "compose write destination resolves outside the project directory"
                    .to_string(),
            });
        }

        let file_name =
            relative_path
                .file_name()
                .ok_or_else(|| ComposeError::InvalidComposePath {
                    field: field.to_string(),
                    path: relative_path.display().to_string(),
                    reason: "write destination must name a file".to_string(),
                })?;
        let destination = canonical_parent.join(file_name);
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ComposeError::SecurityPolicyViolation {
                    service: "<compose-files>".to_string(),
                    field: field.to_string(),
                    reason: format!(
                        "refusing to overwrite repository symlink '{}'",
                        destination.display()
                    ),
                });
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: "compose write destination is a directory".to_string(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ComposeError::FileWriteFailed {
                    path: destination.display().to_string(),
                    reason: format!("failed to inspect compose write destination: {error}"),
                });
            }
        }

        Ok(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::test_support::violation_field;
    use crate::compose::{ComposeError, ComposeExecutor};

    #[test]
    fn test_validate_relative_path_confines_to_project_dir() {
        // Valid relative paths are accepted.
        assert!(
            ComposeExecutor::validate_relative_path("docker-compose.yml", "compose_path").is_ok()
        );
        assert!(
            ComposeExecutor::validate_relative_path("apps/web/compose.yml", "compose_path").is_ok()
        );
        assert!(ComposeExecutor::validate_relative_path("./compose.yml", "compose_path").is_ok());

        // Empty, absolute, and traversing paths are rejected.
        for bad in [
            "",
            "/tmp/compose.yml",
            "/etc/passwd",
            "../compose.yml",
            "apps/../../compose.yml",
        ] {
            let err = ComposeExecutor::validate_relative_path(bad, "compose_path").unwrap_err();
            assert!(matches!(
                err,
                ComposeError::InvalidComposePath { ref field, .. } if field == "compose_path"
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_symlink_host_paths() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        symlink("/", root.path().join("escape")).unwrap();

        let bind =
            "services:\n  app:\n    image: alpine\n    volumes:\n      - ./escape:/host:ro\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            bind,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        let long_bind = "services:\n  app:\n    image: alpine\n    volumes:\n      - type: bind\n        source: escape\n        target: /host\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            long_bind,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "volumes");

        let config = "services:\n  app:\n    image: alpine\nconfigs:\n  host:\n    file: ./escape/etc/passwd\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            config,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "configs.file");

        let build = "services:\n  app:\n    build: ./escape\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            build,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "build.context");
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_symlink_dockerfile_and_write_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("app")).unwrap();
        symlink("/etc/passwd", root.path().join("app/Dockerfile")).unwrap();

        let compose = "services:\n  app:\n    build: ./app\n";
        let err = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            compose,
        )
        .unwrap_err();
        assert_eq!(violation_field(err), "build.dockerfile");

        symlink(
            "/tmp/temps-compose-write-target",
            root.path().join(".env.temps"),
        )
        .unwrap();
        let err = ComposeExecutor::confined_write_path(
            root.path(),
            Path::new(".env.temps"),
            ".env.temps",
        )
        .unwrap_err();
        assert_eq!(violation_field(err), ".env.temps");
    }

    #[test]
    fn test_filesystem_confinement_allows_existing_paths_inside_nested_compose_base() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("apps/app");
        std::fs::create_dir_all(app.join("data")).unwrap();
        std::fs::write(app.join("Dockerfile"), "FROM scratch\n").unwrap();
        std::fs::write(root.path().join("apps/config.txt"), "safe\n").unwrap();

        let compose = r#"
services:
  app:
    build: ./app
    volumes:
      - ./app/data:/data
configs:
  app-config:
    file: ./config.txt
"#;
        ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "apps/compose.yml",
            "compose file",
            compose,
        )
        .unwrap();
    }

    #[test]
    fn test_filesystem_confinement_allows_missing_relative_bind_directories() {
        let root = tempfile::tempdir().unwrap();
        let compose_dir = root.path().join("paperless");
        std::fs::create_dir_all(&compose_dir).unwrap();

        let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    volumes:
      - ./export:/usr/src/paperless/export
      - ./consume:/usr/src/paperless/consume
"#;

        ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "paperless/compose.yml",
            "compose file",
            compose,
        )
        .unwrap();

        // Validation remains read-only. Docker Compose creates short-syntax
        // bind directories when it starts the stack.
        assert!(!compose_dir.join("export").exists());
        assert!(!compose_dir.join("consume").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_filesystem_confinement_rejects_missing_bind_below_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        symlink("/tmp", root.path().join("escape")).unwrap();
        let compose = r#"
services:
  app:
    image: alpine
    volumes:
      - ./escape/not-created-yet:/data
"#;

        let error = ComposeExecutor::validate_compose_filesystem_confinement(
            root.path(),
            "compose.yml",
            "compose file",
            compose,
        )
        .unwrap_err();

        assert_eq!(violation_field(error), "volumes");
    }

    #[test]
    fn filesystem_exceptions_do_not_authorize_generated_file_writes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("compose.yml"), "services: {}").unwrap();
        let policy = ComposeSecurityPolicy {
            disabled_checks: [PolicyCheck::BindMounts].into(),
        };
        let content = "services: {app: {image: nginx, volumes: ['/srv/app:/data']}}";
        assert!(
            ComposeExecutor::validate_compose_filesystem_confinement_with_policy(
                root.path(),
                "compose.yml",
                "test",
                content,
                &policy
            )
            .is_ok()
        );
        assert!(
            ComposeExecutor::confined_write_path(root.path(), Path::new("../outside"), "test")
                .is_err()
        );
        assert!(
            ComposeExecutor::validate_compose_filesystem_confinement_with_policy(
                root.path(),
                "compose.yml",
                "test",
                content,
                &ComposeSecurityPolicy::default()
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn mount_exceptions_still_reject_docker_socket_aliases_and_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("docker.sock");
        std::fs::write(&socket, "test socket placeholder").unwrap();
        std::os::unix::fs::symlink(&socket, root.path().join("engine")).unwrap();
        let policy = ComposeSecurityPolicy {
            disabled_checks: [PolicyCheck::BindMounts, PolicyCheck::VolumeHostPaths].into(),
        };
        for content in [
            "services: {app: {image: nginx, volumes: ['./engine:/socket']}}",
            "services: {app: {image: nginx, volumes: ['/var/run:/host-run']}}",
            "services: {app: {image: nginx}}\nvolumes: {data: {driver_opts: {type: none, o: bind, device: ./engine}}}",
        ] {
            assert!(matches!(ComposeExecutor::validate_compose_filesystem_confinement_with_policy(root.path(), "compose.yml", "test", content, &policy), Err(ComposeError::SecurityPolicyViolation { .. })), "socket reachable through {content}");
        }
        let policy = ComposeSecurityPolicy {
            disabled_checks: [PolicyCheck::BindMounts, PolicyCheck::DockerSocket].into(),
        };
        assert!(
            ComposeExecutor::validate_compose_filesystem_confinement_with_policy(
                root.path(),
                "compose.yml",
                "test",
                "services: {app: {image: nginx, volumes: ['./engine:/socket']}}",
                &policy
            )
            .is_ok()
        );
    }
}
