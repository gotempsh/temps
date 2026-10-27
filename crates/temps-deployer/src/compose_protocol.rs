// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire protocol for running a Docker Compose stack on a worker node.
//!
//! The control plane keeps everything that needs no Docker daemon (config
//! checks, the security policy decision, the deployment's database records)
//! and asks the worker's agent to do the rest with the same
//! [`ComposeExecutor`](crate::compose::ComposeExecutor) the control plane
//! uses locally. The split follows the executor's own two phases:
//!
//! 1. `POST /agent/compose/prepare` — multipart: a JSON [`ComposePrepareSpec`]
//!    first, then (when the stack comes from a repository) a `stack` tar of
//!    the checkout. The worker writes the stack, builds and pulls, and keeps
//!    the prepared generation in memory under its generation name. The old
//!    stack keeps running.
//! 2. `POST /agent/compose/deploy` — starts a prepared generation.
//!
//! Both answer with NDJSON [`ComposeHostEvent`]s: output batches while
//! `docker compose` runs, then exactly one terminal event. The remaining
//! operations (teardown, stop, secret-generation cleanup, verified removal)
//! are plain JSON requests.
//!
//! A stack's named volumes and persistent bind directories live on the node
//! that ran it, which is why the control plane pins each Compose environment
//! to one node instead of scheduling every deploy afresh.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use temps_entities::compose_security::ComposeSecurityPolicy;
use temps_presets::compose_check::ComposeCheckErrorClass;

use crate::compose::{
    ComposeDeployFailure, ComposeDeployRequest, ComposeError, ComposeOutputSink,
    ComposeServiceResult,
};

/// Bumped whenever a request or event changes shape incompatibly. A worker
/// refuses a version it does not know rather than guessing.
pub const COMPOSE_PROTOCOL_VERSION: u32 = 1;

/// Largest prepare spec a worker accepts. Holds the resolved Compose file,
/// its override, the repository `.env`, every variable and every secret
/// (each secret is at most 1 MiB, see `DeployRequest::secrets`).
pub const MAX_COMPOSE_SPEC_BYTES: usize = 16 * 1024 * 1024;

/// Largest stack archive (the repository checkout) a worker accepts.
pub const MAX_COMPOSE_STACK_BYTES: u64 = 1024 * 1024 * 1024;

/// Most entries a stack archive may hold.
pub const MAX_COMPOSE_STACK_ENTRIES: usize = 200_000;

/// Largest single NDJSON event either side reads. Output batches are capped
/// at 256 KiB by the executor; a deploy result lists every service.
pub const MAX_COMPOSE_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// Everything a worker needs to prepare one generation of a stack.
///
/// Mirrors [`ComposeDeployRequest`] minus what only makes sense on the host
/// that builds it: the checkout directory (the `stack` archive replaces it)
/// and the in-process output sink (output comes back as events).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposePrepareSpec {
    pub version: u32,
    pub project_name: String,
    pub generation: String,
    pub compose_content: String,
    pub env_content: Option<String>,
    pub compose_path: Option<String>,
    pub environment_vars: HashMap<String, String>,
    /// Decrypted secrets, materialized on the worker under `/run/secrets`
    /// exactly as on the control plane. Protected in transit by the agent
    /// connection (mutual TLS), the same as a single-container deploy's.
    pub secrets: HashMap<String, String>,
    pub secret_compose_services: HashMap<String, Vec<String>>,
    pub build_args: HashMap<String, String>,
    pub labels: HashMap<String, String>,
    pub compose_override: Option<String>,
    pub relaxed_capability_services: Vec<String>,
    pub unsandboxed_services: Vec<String>,
    pub ready_timeout_secs: Option<u64>,
    pub routed_services: Vec<String>,
    /// The administrator's Compose security policy for this project. The
    /// worker enforces it with the same checks the control plane would.
    pub policy: ComposeSecurityPolicy,
    /// Whether a `stack` archive follows. Without one the stack runs from
    /// the inline Compose content, as on the control plane.
    pub has_stack: bool,
}

impl ComposePrepareSpec {
    pub fn from_request(
        request: &ComposeDeployRequest,
        generation: &str,
        policy: ComposeSecurityPolicy,
    ) -> Self {
        Self {
            version: COMPOSE_PROTOCOL_VERSION,
            project_name: request.project_name.clone(),
            generation: generation.to_string(),
            compose_content: request.compose_content.clone(),
            env_content: request.env_content.clone(),
            compose_path: request.compose_path.clone(),
            environment_vars: request.environment_vars.clone(),
            secrets: request.secrets.clone(),
            secret_compose_services: request.secret_compose_services.clone(),
            build_args: request.build_args.clone(),
            labels: request.labels.clone(),
            compose_override: request.compose_override.clone(),
            relaxed_capability_services: request.relaxed_capability_services.clone(),
            unsandboxed_services: request.unsandboxed_services.clone(),
            ready_timeout_secs: request.ready_timeout.map(|timeout| timeout.as_secs()),
            routed_services: request.routed_services.clone(),
            policy,
            has_stack: request.repo_dir.is_some(),
        }
    }

    /// The request the worker's executor runs, with the stack extracted at
    /// `repo_dir`.
    pub fn to_request(
        &self,
        repo_dir: Option<PathBuf>,
        output_sink: Option<ComposeOutputSink>,
    ) -> ComposeDeployRequest {
        ComposeDeployRequest {
            project_name: self.project_name.clone(),
            compose_content: self.compose_content.clone(),
            env_content: self.env_content.clone(),
            work_dir: std::env::temp_dir(),
            compose_path: self.compose_path.clone(),
            environment_vars: self.environment_vars.clone(),
            secrets: self.secrets.clone(),
            secret_compose_services: self.secret_compose_services.clone(),
            build_args: self.build_args.clone(),
            labels: self.labels.clone(),
            repo_dir,
            compose_override: self.compose_override.clone(),
            relaxed_capability_services: self.relaxed_capability_services.clone(),
            unsandboxed_services: self.unsandboxed_services.clone(),
            ready_timeout: self.ready_timeout_secs.map(std::time::Duration::from_secs),
            routed_services: self.routed_services.clone(),
            output_sink,
        }
    }

    /// Reject a spec the worker cannot safely act on before touching disk.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != COMPOSE_PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported worker Compose protocol version {} (this agent speaks {}); run the \
                 same Temps version on the control plane and the worker",
                self.version, COMPOSE_PROTOCOL_VERSION
            ));
        }
        validate_stack_name(&self.project_name, "project_name")?;
        validate_stack_name(&self.generation, "generation")?;
        Ok(())
    }
}

/// A Compose project or generation name, used as a directory name on the
/// worker. Temps generates both (`temps-{project}-{environment}`,
/// `deployment-{id}-{uuid}`), so anything outside that alphabet is refused.
pub fn validate_stack_name(value: &str, field: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        && !value.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Compose {field} '{value}' must be 1-128 ASCII letters, digits, '-' or '_' and must \
             not start with '-'"
        ))
    }
}

/// Start a prepared generation, or forget one (`/agent/compose/discard`).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposeGenerationRequest {
    pub project_name: String,
    pub generation: String,
}

/// `docker compose down` (volumes are always kept) or `docker compose stop`
/// for a project. The worker runs it from `generation`'s files when that
/// generation is still on disk, otherwise from the stack that last started.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposeStackRequest {
    pub project_name: String,
    pub generation: Option<String>,
    pub compose_path: Option<String>,
    /// For teardown: also remove the project's materialized secrets.
    #[serde(default)]
    pub remove_secrets: bool,
}

/// Remove every secret generation except `keep_generation`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposePruneRequest {
    pub project_name: String,
    pub keep_generation: Option<String>,
}

/// Force-remove containers after re-verifying their Temps labels.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposeRemoveVerifiedRequest {
    pub containers: Vec<ComposeServiceResult>,
    pub expected_labels: HashMap<String, String>,
}

/// Acknowledgement for the JSON operations, naming what was acted on.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComposeAck {
    pub project_name: String,
}

/// An error raised on the worker, carried back with its class so the
/// deployment records the same failure kind it would locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteComposeError {
    pub message: String,
    pub class: ComposeCheckErrorClass,
}

impl From<&ComposeError> for RemoteComposeError {
    fn from(error: &ComposeError) -> Self {
        Self {
            message: error.to_string(),
            class: error.class(),
        }
    }
}

impl RemoteComposeError {
    pub fn into_compose_error(self, node_name: &str) -> ComposeError {
        ComposeError::Remote {
            node_name: node_name.to_string(),
            message: self.message,
            class: self.class,
        }
    }
}

/// A failed start, with the containers it created (see
/// [`ComposeDeployFailure`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeHostFailure {
    pub error: RemoteComposeError,
    #[serde(default)]
    pub containers: Vec<ComposeServiceResult>,
    #[serde(default)]
    pub cleanup_containers: Vec<ComposeServiceResult>,
    #[serde(default)]
    pub retention_error: Option<RemoteComposeError>,
}

impl From<&ComposeDeployFailure> for ComposeHostFailure {
    fn from(failure: &ComposeDeployFailure) -> Self {
        Self {
            error: RemoteComposeError::from(&failure.error),
            containers: failure.containers.clone(),
            cleanup_containers: failure.cleanup_containers.clone(),
            retention_error: failure
                .retention_error
                .as_ref()
                .map(RemoteComposeError::from),
        }
    }
}

impl ComposeHostFailure {
    pub fn into_deploy_failure(self, node_name: &str) -> ComposeDeployFailure {
        ComposeDeployFailure {
            error: self.error.into_compose_error(node_name),
            containers: self.containers,
            cleanup_containers: self.cleanup_containers,
            retention_error: self
                .retention_error
                .map(|error| error.into_compose_error(node_name)),
        }
    }
}

/// One line of the NDJSON stream from `/agent/compose/prepare` and
/// `/agent/compose/deploy`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ComposeHostEvent {
    /// A batch of redacted `docker compose` output lines.
    Output(Vec<String>),
    /// Terminal for prepare: the generation is ready to start.
    Prepared,
    /// Terminal for deploy: every service is running.
    Deployed(Vec<ComposeServiceResult>),
    /// Terminal for either.
    Failed(ComposeHostFailure),
}

/// Pack a checkout into a tar for the worker.
///
/// Everything Compose may read is included (bind sources, `env_file`s,
/// configs, build contexts), so unlike an image build no `.dockerignore`
/// applies. `.git` is skipped. Symbolic links are kept only when they are
/// relative and stay inside the checkout; any other link, or a special file,
/// refuses the deploy with its path, since the worker could not reproduce it
/// safely.
pub fn pack_compose_stack(repo_dir: &Path) -> Result<tempfile::NamedTempFile, ComposeError> {
    let archive =
        tempfile::NamedTempFile::new().map_err(|error| ComposeError::FileWriteFailed {
            path: std::env::temp_dir().display().to_string(),
            reason: format!("cannot create the worker stack archive: {error}"),
        })?;
    let mut builder = tar::Builder::new(archive.reopen().map_err(ComposeError::Io)?);
    builder.follow_symlinks(false);
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    append_stack_dir(
        &mut builder,
        repo_dir,
        Path::new(""),
        &mut entries,
        &mut bytes,
    )?;
    builder.finish().map_err(ComposeError::Io)?;
    Ok(archive)
}

fn append_stack_dir(
    builder: &mut tar::Builder<std::fs::File>,
    root: &Path,
    relative: &Path,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<(), ComposeError> {
    let directory = root.join(relative);
    let mut children = std::fs::read_dir(&directory)
        .map_err(|error| stack_error(&directory, format!("cannot list directory: {error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| stack_error(&directory, format!("cannot list directory: {error}")))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let name = child.file_name();
        if relative.as_os_str().is_empty() && name == ".git" {
            continue;
        }
        let child_relative = relative.join(&name);
        let path = root.join(&child_relative);
        *entries += 1;
        if *entries > MAX_COMPOSE_STACK_ENTRIES {
            return Err(stack_error(
                root,
                format!(
                    "the checkout has more than {MAX_COMPOSE_STACK_ENTRIES} files and \
                     directories, more than a worker accepts"
                ),
            ));
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| stack_error(&path, format!("cannot read file metadata: {error}")))?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path)
                .map_err(|error| stack_error(&path, format!("cannot read link: {error}")))?;
            validate_link_target(&child_relative, &target)
                .map_err(|reason| stack_error(&path, reason))?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder
                .append_link(&mut header, &child_relative, &target)
                .map_err(ComposeError::Io)?;
        } else if file_type.is_dir() {
            builder
                .append_dir(&child_relative, &path)
                .map_err(ComposeError::Io)?;
            append_stack_dir(builder, root, &child_relative, entries, bytes)?;
        } else if file_type.is_file() {
            *bytes = bytes.saturating_add(metadata.len());
            if *bytes > MAX_COMPOSE_STACK_BYTES {
                return Err(stack_error(
                    root,
                    format!(
                        "the checkout is larger than the {} MiB a worker accepts",
                        MAX_COMPOSE_STACK_BYTES / (1024 * 1024)
                    ),
                ));
            }
            builder
                .append_path_with_name(&path, &child_relative)
                .map_err(ComposeError::Io)?;
        } else {
            return Err(stack_error(
                &path,
                "is not a regular file, directory or link, so it cannot be sent to a worker"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn stack_error(path: &Path, reason: String) -> ComposeError {
    ComposeError::InvalidComposePath {
        path: path.display().to_string(),
        field: "repository checkout".to_string(),
        reason: format!("cannot send the stack to the worker node: {reason}"),
    }
}

/// A link at `link` (relative to the stack root) may point only at a
/// relative path that resolves, lexically, inside the stack root.
fn validate_link_target(link: &Path, target: &Path) -> Result<(), String> {
    if target.is_absolute() {
        return Err(format!(
            "links to '{}', an absolute path; only relative links inside the repository can be \
             sent to a worker",
            target.display()
        ));
    }
    let mut depth = link.components().count().saturating_sub(1) as isize;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(format!(
                        "links to '{}', outside the repository",
                        target.display()
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("links to '{}', an absolute path", target.display()));
            }
        }
    }
    Ok(())
}

/// Extract a stack archive into an empty `destination`, enforcing the same
/// limits and link rule the control plane packed it with. A file is never
/// written through a link, so a link cannot redirect a later entry outside
/// the destination.
pub fn unpack_compose_stack(archive: &Path, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("Cannot open the uploaded stack archive: {error}"))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|error| format!("Cannot read the stack archive: {error}"))?;
    let mut count = 0_usize;
    let mut total = 0_u64;
    for entry in entries {
        count += 1;
        if count > MAX_COMPOSE_STACK_ENTRIES {
            return Err(format!(
                "The stack archive has more than {MAX_COMPOSE_STACK_ENTRIES} entries"
            ));
        }
        let mut entry = entry.map_err(|error| format!("Invalid stack archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("Invalid stack archive path: {error}"))?
            .into_owned();
        crate::build_protocol::validate_archive_path(&path)?;
        if path.components().next() == Some(Component::Normal(".git".as_ref())) {
            return Err("The stack archive must not contain .git".to_string());
        }
        let target = destination.join(&path);
        ensure_no_link_ancestor(destination, &path)?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target).map_err(|error| {
                    format!("Cannot create directory '{}': {error}", path.display())
                })?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                total = total.saturating_add(entry.header().size().unwrap_or(0));
                if total > MAX_COMPOSE_STACK_BYTES {
                    return Err(format!(
                        "The stack archive is larger than {MAX_COMPOSE_STACK_BYTES} bytes"
                    ));
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        format!("Cannot create directory for '{}': {error}", path.display())
                    })?;
                }
                let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
                let mut contents = Vec::new();
                entry
                    .by_ref()
                    .take(MAX_COMPOSE_STACK_BYTES)
                    .read_to_end(&mut contents)
                    .map_err(|error| format!("Cannot read '{}': {error}", path.display()))?;
                write_new_file(&target, &contents, mode)
                    .map_err(|error| format!("Cannot write '{}': {error}", path.display()))?;
            }
            tar::EntryType::Symlink => {
                let link_target = entry
                    .link_name()
                    .map_err(|error| format!("Invalid link '{}': {error}", path.display()))?
                    .ok_or_else(|| format!("Link '{}' has no target", path.display()))?
                    .into_owned();
                validate_link_target(&path, &link_target)
                    .map_err(|reason| format!("Link '{}' {reason}", path.display()))?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        format!("Cannot create directory for '{}': {error}", path.display())
                    })?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&link_target, &target)
                    .map_err(|error| format!("Cannot create link '{}': {error}", path.display()))?;
                #[cfg(not(unix))]
                return Err(format!(
                    "Link '{}' cannot be created on this platform",
                    path.display()
                ));
            }
            other => {
                return Err(format!(
                    "Stack archive entry '{}' has unsupported type {other:?}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn ensure_no_link_ancestor(destination: &Path, relative: &Path) -> Result<(), String> {
    let mut current = destination.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(format!(
                "Stack archive entry '{}' would be written through a link",
                relative.display()
            ));
        }
    }
    Ok(())
}

fn write_new_file(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path)?;
    file.write_all(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A checkout crosses to the worker intact (nested files, permissions,
    /// in-tree links), and `.git` stays behind.
    #[test]
    fn a_checkout_round_trips_without_git() {
        let source = tempfile::tempdir().unwrap();
        write(source.path(), "compose.yaml", "services: {}\n");
        write(source.path(), "app/Dockerfile", "FROM alpine\n");
        write(source.path(), ".git/HEAD", "ref: refs/heads/main\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink("app/Dockerfile", source.path().join("Dockerfile")).unwrap();

        let archive = pack_compose_stack(source.path()).unwrap();
        let destination = tempfile::tempdir().unwrap();
        unpack_compose_stack(archive.path(), destination.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.path().join("compose.yaml")).unwrap(),
            "services: {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("app/Dockerfile")).unwrap(),
            "FROM alpine\n"
        );
        assert!(!destination.path().join(".git").exists());
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(destination.path().join("Dockerfile")).unwrap(),
            PathBuf::from("app/Dockerfile")
        );
    }

    /// A link that leaves the checkout is refused on the control plane, with
    /// its path, before anything is sent.
    #[cfg(unix)]
    #[test]
    fn links_outside_the_checkout_are_refused() {
        for target in ["/etc/passwd", "../outside", "app/../../outside"] {
            let source = tempfile::tempdir().unwrap();
            write(source.path(), "app/file", "x");
            std::os::unix::fs::symlink(target, source.path().join("escape")).unwrap();
            let error = pack_compose_stack(source.path()).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("escape"), "{message}");
            assert!(message.contains("worker"), "{message}");
        }
    }

    #[test]
    fn link_targets_are_checked_relative_to_the_link() {
        assert!(validate_link_target(Path::new("a/b/link"), Path::new("../c")).is_ok());
        assert!(validate_link_target(Path::new("a/b/link"), Path::new("../../c")).is_ok());
        assert!(validate_link_target(Path::new("a/b/link"), Path::new("../../../c")).is_err());
        assert!(validate_link_target(Path::new("link"), Path::new("./x/../y")).is_ok());
        assert!(validate_link_target(Path::new("link"), Path::new("..")).is_err());
    }

    /// An archive crafted to write through a link it planted is refused on
    /// the worker, whatever the control plane sent.
    #[cfg(unix)]
    #[test]
    fn a_crafted_archive_cannot_write_through_a_link() {
        let archive = tempfile::NamedTempFile::new().unwrap();
        {
            let mut builder = tar::Builder::new(archive.reopen().unwrap());
            let mut link = tar::Header::new_gnu();
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_size(0);
            builder.append_link(&mut link, "dir", "inside").unwrap();
            let mut file = tar::Header::new_gnu();
            file.set_size(1);
            file.set_mode(0o644);
            file.set_cksum();
            builder
                .append_data(&mut file, "dir/payload", &b"x"[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let destination = tempfile::tempdir().unwrap();
        let error = unpack_compose_stack(archive.path(), destination.path()).unwrap_err();
        assert!(error.contains("through a link"), "{error}");
        assert!(!destination.path().join("inside/payload").exists());
    }

    #[test]
    fn stack_names_are_restricted_to_generated_alphabets() {
        assert!(validate_stack_name("temps-12-34", "project_name").is_ok());
        assert!(validate_stack_name("deployment-7-0f3c_a", "generation").is_ok());
        for bad in ["", "-x", "a/b", "..", "a b", &"x".repeat(129)] {
            assert!(validate_stack_name(bad, "generation").is_err(), "{bad}");
        }
    }

    /// The spec carries every request field, and a newer protocol version
    /// is refused instead of half-understood.
    #[test]
    fn prepare_spec_round_trips_the_request_and_checks_its_version() {
        let request = ComposeDeployRequest {
            project_name: "temps-1-2".into(),
            compose_content: "services: {}\n".into(),
            env_content: Some("A=1".into()),
            work_dir: std::env::temp_dir(),
            compose_path: Some("compose.yaml".into()),
            environment_vars: HashMap::from([("PORT".into(), "3000".into())]),
            secrets: HashMap::from([("TOKEN".into(), "s3cr3t".into())]),
            secret_compose_services: HashMap::from([("TOKEN".into(), vec!["web".into()])]),
            build_args: HashMap::new(),
            labels: HashMap::from([("sh.temps.project_id".into(), "1".into())]),
            repo_dir: Some(PathBuf::from("/tmp/checkout")),
            compose_override: None,
            relaxed_capability_services: vec!["db".into()],
            unsandboxed_services: Vec::new(),
            ready_timeout: Some(std::time::Duration::from_secs(90)),
            routed_services: vec!["web".into()],
            output_sink: None,
        };
        let spec = ComposePrepareSpec::from_request(
            &request,
            "deployment-9-abc",
            ComposeSecurityPolicy::default(),
        );
        assert!(spec.has_stack);
        let decoded: ComposePrepareSpec =
            serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        decoded.validate().unwrap();
        let rebuilt = decoded.to_request(Some(PathBuf::from("/var/lib/temps/stack")), None);
        assert_eq!(rebuilt.secrets["TOKEN"], "s3cr3t");
        assert_eq!(
            rebuilt.secret_compose_services["TOKEN"],
            vec!["web".to_string()]
        );
        assert_eq!(
            rebuilt.ready_timeout,
            Some(std::time::Duration::from_secs(90))
        );
        assert_eq!(
            rebuilt.repo_dir,
            Some(PathBuf::from("/var/lib/temps/stack"))
        );
        assert_eq!(rebuilt.routed_services, vec!["web".to_string()]);

        let mut newer = decoded;
        newer.version = COMPOSE_PROTOCOL_VERSION + 1;
        assert!(newer.validate().unwrap_err().contains("same Temps version"));
    }
}
