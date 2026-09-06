// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistence and validation for AI-first multi-project applications.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use serde_json::Value;
use temps_ai::HarnessWorkspace;
use temps_entities::{
    ai_application_projects, ai_application_workspaces, ai_applications, ai_conversations,
    ai_thread_artifacts, deployments, environments, projects, sandboxes,
};

const MAX_PROJECTS: usize = 20;
const MAX_APPLICATIONS_PER_USER: usize = 10;
const MAX_CHAT_ATTACHMENT_FILES_PER_WORKSPACE: usize = 256;
const MAX_CHAT_ATTACHMENT_BYTES_PER_WORKSPACE: u64 = 256 * 1024 * 1024;
const MAX_WORKSPACE_CPU: f64 = 8.0;
const MAX_WORKSPACE_MEMORY_MB: i64 = 16_384;
const MAX_WORKSPACE_PIDS: i64 = 2_048;
const MAX_WORKSPACE_DISK_MB: i64 = 65_536;
const MAX_USER_WORKSPACE_CPU: f64 = 32.0;
const MAX_USER_WORKSPACE_MEMORY_MB: i64 = 65_536;
const MAX_USER_WORKSPACE_PIDS: i64 = 8_192;
const MAX_USER_WORKSPACE_DISK_MB: i64 = 262_144;
pub(crate) const ALLOWED_ARTIFACT_KINDS: &[&str] = &[
    "topology",
    "execution_plan",
    "credential_request",
    "status",
    "form",
    "table",
    // Semantic resource artifacts are renderer-neutral. The console chooses a
    // trusted component from `payload.resource_type`; the model never supplies
    // executable UI or arbitrary component names.
    "resource",
    "collection",
    "operation",
];

fn is_allowed_artifact_kind(kind: &str) -> bool {
    ALLOWED_ARTIFACT_KINDS.contains(&kind)
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error("application '{0}' not found")]
    NotFound(String),
    #[error("application name must be between 1 and 200 characters")]
    InvalidName,
    #[error("an application may contain at most {MAX_PROJECTS} unique projects")]
    InvalidProjects,
    #[error("project {project_id} is already linked to application '{application_id}'")]
    ProjectAlreadyLinked {
        application_id: String,
        project_id: i32,
    },
    #[error("project {project_id} is not linked to application '{application_id}'")]
    ProjectNotLinked {
        application_id: String,
        project_id: i32,
    },
    #[error("project {0} does not exist")]
    ProjectNotFound(i32),
    #[error("conversation '{0}' does not belong to this application")]
    ConversationNotFound(String),
    #[error("artifact kind '{0}' is not supported")]
    InvalidArtifactKind(String),
    #[error("artifact payload contains a secret value at '{0}'; store it in the credential broker and include only a reference")]
    SecretValue(String),
    #[error("failed to prepare the managed application workspace at {path}: {source}")]
    Workspace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("application workspace identifier '{0}' is invalid")]
    InvalidWorkspaceIdentifier(String),
    #[error("chat attachment is invalid: {0}")]
    InvalidAttachment(String),
    #[error("chat attachment '{0}' was not found in this workspace")]
    AttachmentNotFound(String),
    #[error("invalid workspace setting: {0}")]
    InvalidWorkspaceSetting(String),
    #[error("application workspace quota exceeded: {0}")]
    WorkspaceQuota(String),
    #[error("database operation failed: {0}")]
    Database(#[from] sea_orm::DbErr),
}

#[derive(Debug, Clone)]
pub struct ApplicationWithProjects {
    pub application: ai_applications::Model,
    pub projects: Vec<projects::Model>,
    pub primary_project_id: Option<i32>,
    pub environment_statuses: HashMap<i32, Vec<ProjectEnvironmentStatus>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ApplicationProjectScope {
    pub public_id: String,
    pub project_ids: Vec<i32>,
}

#[derive(Debug, Clone)]
pub struct ProjectEnvironmentStatus {
    pub name: String,
    pub slug: String,
    pub sleeping: bool,
    pub deployment_state: Option<String>,
}

/// Creates the only host paths a development harness may receive. Every
/// application gets one durable root with a project subdirectory per linked
/// Temps project:
///
/// `<TEMPS_DATA_DIR>/ai-applications/<application-id>/projects/<project-slug>`
///
/// The root is bind-mounted into the instance sandbox. It is deliberately not
/// a user-selected host path, so an AI thread can never be tricked into
/// inspecting an arbitrary directory on the Temps machine.
#[derive(Clone)]
pub struct ApplicationWorkspaceService {
    root: PathBuf,
    import_lock: Arc<tokio::sync::Mutex<()>>,
}

/// A project tree moved outside the mounted application workspace while its
/// database link is removed. The opaque paths are only produced by
/// [`ApplicationWorkspaceService`] after validating server-owned components.
#[derive(Debug)]
pub struct StagedProjectRemoval {
    original_path: PathBuf,
    staged_path: PathBuf,
}

impl ApplicationWorkspaceService {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            root: data_dir.join("ai-applications"),
            import_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store browser-selected project files without ever resolving a
    /// user-controlled pathname from the host root. Each path component is
    /// opened relative to a trusted directory descriptor with `NOFOLLOW`.
    pub async fn store_project_files_bounded(
        &self,
        application_public_id: &str,
        project_slug: &str,
        files: Vec<(PathBuf, Vec<u8>, Option<u32>)>,
        max_workspace_bytes: u64,
        max_workspace_entries: usize,
    ) -> Result<usize, ApplicationError> {
        validate_workspace_component(application_public_id)?;
        validate_workspace_component(project_slug)?;
        let _import = self.import_lock.lock().await;
        let root = self.root.clone();
        let application_public_id = application_public_id.to_string();
        let project_slug = project_slug.to_string();
        let error_path = root
            .join(&application_public_id)
            .join("projects")
            .join(&project_slug);
        tokio::task::spawn_blocking(move || {
            store_project_files_fd_relative(
                &root,
                &application_public_id,
                &project_slug,
                &files,
                max_workspace_bytes,
                max_workspace_entries,
            )
        })
        .await
        .map_err(|source| ApplicationError::Workspace {
            path: error_path.clone(),
            source: std::io::Error::other(format!("workspace import task failed: {source}")),
        })?
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::FileTooLarge {
                ApplicationError::WorkspaceQuota(source.to_string())
            } else {
                ApplicationError::Workspace {
                    path: error_path,
                    source,
                }
            }
        })
    }

    /// Ensure that the durable workspace exists before a thread is created or
    /// executed. This is idempotent so an interrupted create can safely be
    /// retried without losing generated project files.
    pub async fn ensure(
        &self,
        application_public_id: &str,
        projects: &[projects::Model],
    ) -> Result<HarnessWorkspace, ApplicationError> {
        validate_workspace_component(application_public_id)?;
        // Only the configured data root may be created recursively. Every
        // user-writable descendant is opened one component at a time and must
        // be a real directory. This prevents a sandbox-created `projects`
        // symlink from making the privileged server create paths elsewhere on
        // the host.
        ensure_trusted_directory(&self.root, true).await?;
        for project in projects {
            validate_workspace_component(&project.slug)?;
        }
        let workspace_root = self.root.join(application_public_id);

        #[cfg(unix)]
        {
            let root = self.root.clone();
            let application_public_id = application_public_id.to_string();
            let project_slugs = projects
                .iter()
                .map(|project| project.slug.clone())
                .collect::<Vec<_>>();
            let failure_path = workspace_root.clone();
            tokio::task::spawn_blocking(move || {
                ensure_workspace_tree_fd_relative(&root, &application_public_id, &project_slugs)
            })
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: failure_path.clone(),
                source: std::io::Error::other(format!("workspace directory task failed: {source}")),
            })?
            .map_err(|source| ApplicationError::Workspace {
                path: failure_path,
                source,
            })?;
        }

        #[cfg(not(unix))]
        {
            ensure_trusted_directory(&workspace_root, false).await?;
            let projects_root = workspace_root.join("projects");
            ensure_trusted_directory(&projects_root, false).await?;
            for project in projects {
                ensure_trusted_directory(&projects_root.join(&project.slug), false).await?;
            }
        }

        Ok(HarnessWorkspace {
            // Docker's sandbox name is `temps-sandbox-<label>`; this opaque
            // application id is generated by Temps and passes the strict
            // component validation above.
            sandbox_label: application_public_id.to_string(),
            host_work_dir: workspace_root,
        })
    }

    /// Atomically move a linked project's source tree out of the mounted
    /// workspace before its database link is removed. Callers can restore the
    /// move if the database mutation fails, or finalize it after commit.
    pub async fn stage_project_removal(
        &self,
        application_public_id: &str,
        project_slug: &str,
    ) -> Result<Option<StagedProjectRemoval>, ApplicationError> {
        validate_workspace_component(application_public_id)?;
        validate_workspace_component(project_slug)?;
        let _mutation = self.import_lock.lock().await;
        ensure_trusted_directory(&self.root, true).await?;
        let workspace_root = self.root.join(application_public_id);
        let projects_root = workspace_root.join("projects");
        ensure_trusted_directory(&workspace_root, false).await?;
        ensure_trusted_directory(&projects_root, false).await?;
        let original_path = projects_root.join(project_slug);
        let metadata = match tokio::fs::symlink_metadata(&original_path).await {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ApplicationError::Workspace {
                    path: original_path,
                    source,
                })
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ApplicationError::Workspace {
                path: original_path,
                source: std::io::Error::other(
                    "linked project source path is not a trusted directory",
                ),
            });
        }
        let staging_root = self.root.join(".unlinked-projects");
        ensure_trusted_directory(&staging_root, true).await?;
        let staged_path = staging_root.join(format!(
            "{application_public_id}-{project_slug}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::rename(&original_path, &staged_path)
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: original_path.clone(),
                source,
            })?;
        Ok(Some(StagedProjectRemoval {
            original_path,
            staged_path,
        }))
    }

    pub async fn restore_staged_project(
        &self,
        removal: &StagedProjectRemoval,
    ) -> Result<(), ApplicationError> {
        let _mutation = self.import_lock.lock().await;
        tokio::fs::rename(&removal.staged_path, &removal.original_path)
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: removal.original_path.clone(),
                source,
            })
    }

    pub async fn finalize_staged_project(
        &self,
        removal: StagedProjectRemoval,
    ) -> Result<(), ApplicationError> {
        let _mutation = self.import_lock.lock().await;
        tokio::fs::remove_dir_all(&removal.staged_path)
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: removal.staged_path,
                source,
            })
    }

    /// Persist one uploaded chat file inside the workspace using only
    /// server-generated path components. On Unix every directory and the file
    /// itself are opened relative to trusted descriptors with `NOFOLLOW`, so a
    /// sandbox-created symlink cannot redirect an upload outside the volume.
    pub async fn store_chat_attachment(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        attachment_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, ApplicationError> {
        let _mutation = self.import_lock.lock().await;
        for component in [workspace_id, conversation_id, attachment_id] {
            validate_workspace_component(component)?;
        }
        validate_attachment_file_name(file_name)?;
        ensure_trusted_directory(&self.root, true).await?;
        let workspace_root = self.root.join(workspace_id);
        let result_path = workspace_root
            .join(".temps")
            .join("chat-attachments")
            .join(conversation_id)
            .join(attachment_id)
            .join(file_name);
        let attachment_root = workspace_root.join(".temps").join("chat-attachments");
        let usage_path = attachment_root.clone();
        let (stored_bytes, stored_files) =
            tokio::task::spawn_blocking(move || chat_attachment_usage(&usage_path))
                .await
                .map_err(|source| ApplicationError::Workspace {
                    path: attachment_root.clone(),
                    source: std::io::Error::other(format!(
                        "attachment quota task failed: {source}"
                    )),
                })?
                .map_err(|source| ApplicationError::Workspace {
                    path: attachment_root,
                    source,
                })?;
        if stored_files >= MAX_CHAT_ATTACHMENT_FILES_PER_WORKSPACE
            || stored_bytes.saturating_add(bytes.len() as u64)
                > MAX_CHAT_ATTACHMENT_BYTES_PER_WORKSPACE
        {
            return Err(ApplicationError::InvalidAttachment(format!(
                "workspace attachment quota exceeded (maximum {MAX_CHAT_ATTACHMENT_FILES_PER_WORKSPACE} files and {} MiB)",
                MAX_CHAT_ATTACHMENT_BYTES_PER_WORKSPACE / 1024 / 1024
            )));
        }

        #[cfg(unix)]
        {
            let root = self.root.clone();
            let workspace_id = workspace_id.to_string();
            let conversation_id = conversation_id.to_string();
            let attachment_id = attachment_id.to_string();
            let file_name = file_name.to_string();
            let failure_path = result_path.clone();
            tokio::task::spawn_blocking(move || {
                store_chat_attachment_fd_relative(
                    &root,
                    &workspace_id,
                    &conversation_id,
                    &attachment_id,
                    &file_name,
                    &bytes,
                )
            })
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: failure_path.clone(),
                source: std::io::Error::other(format!("attachment write task failed: {source}")),
            })?
            .map_err(|source| ApplicationError::Workspace {
                path: failure_path,
                source,
            })?;
        }

        #[cfg(not(unix))]
        {
            let attachment_root = result_path.parent().ok_or_else(|| {
                ApplicationError::InvalidAttachment("missing attachment parent".to_string())
            })?;
            tokio::fs::create_dir_all(attachment_root)
                .await
                .map_err(|source| ApplicationError::Workspace {
                    path: attachment_root.to_path_buf(),
                    source,
                })?;
            ensure_trusted_directory(attachment_root, false).await?;
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            let mut file =
                options
                    .open(&result_path)
                    .await
                    .map_err(|source| ApplicationError::Workspace {
                        path: result_path.clone(),
                        source,
                    })?;
            use tokio::io::AsyncWriteExt;
            file.write_all(&bytes)
                .await
                .map_err(|source| ApplicationError::Workspace {
                    path: result_path.clone(),
                    source,
                })?;
        }

        Ok(result_path)
    }

    pub async fn chat_attachment_size(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        attachment_id: &str,
        file_name: &str,
    ) -> Result<u64, ApplicationError> {
        for component in [workspace_id, conversation_id, attachment_id] {
            validate_workspace_component(component)?;
        }
        validate_attachment_file_name(file_name)?;
        let path = self
            .root
            .join(workspace_id)
            .join(".temps")
            .join("chat-attachments")
            .join(conversation_id)
            .join(attachment_id)
            .join(file_name);

        #[cfg(unix)]
        {
            let root = self.root.clone();
            let workspace_id = workspace_id.to_string();
            let conversation_id = conversation_id.to_string();
            let attachment_id = attachment_id.to_string();
            let file_name = file_name.to_string();
            let attachment_label = attachment_id.clone();
            tokio::task::spawn_blocking(move || {
                chat_attachment_size_fd_relative(
                    &root,
                    &workspace_id,
                    &conversation_id,
                    &attachment_id,
                    &file_name,
                )
            })
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: path.clone(),
                source: std::io::Error::other(format!("attachment inspect task failed: {source}")),
            })?
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ApplicationError::AttachmentNotFound(attachment_label)
                } else {
                    ApplicationError::Workspace { path, source }
                }
            })
        }

        #[cfg(not(unix))]
        {
            let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ApplicationError::AttachmentNotFound(attachment_id.to_string())
                } else {
                    ApplicationError::Workspace {
                        path: path.clone(),
                        source,
                    }
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ApplicationError::InvalidAttachment(
                    "attachment path is not a regular file".to_string(),
                ));
            }
            Ok(metadata.len())
        }
    }

    /// Read an attachment through the same descriptor-relative, no-symlink
    /// traversal used for writes. The byte ceiling is enforced while reading,
    /// not only from metadata, because sandbox code can mutate its workspace.
    pub async fn read_chat_attachment(
        &self,
        workspace_id: &str,
        conversation_id: &str,
        attachment_id: &str,
        file_name: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ApplicationError> {
        for component in [workspace_id, conversation_id, attachment_id] {
            validate_workspace_component(component)?;
        }
        validate_attachment_file_name(file_name)?;
        let path = self
            .root
            .join(workspace_id)
            .join(".temps")
            .join("chat-attachments")
            .join(conversation_id)
            .join(attachment_id)
            .join(file_name);

        #[cfg(unix)]
        {
            let root = self.root.clone();
            let workspace_id = workspace_id.to_string();
            let conversation_id = conversation_id.to_string();
            let attachment_id = attachment_id.to_string();
            let attachment_label = attachment_id.clone();
            let file_name = file_name.to_string();
            tokio::task::spawn_blocking(move || {
                read_chat_attachment_fd_relative(
                    &root,
                    &workspace_id,
                    &conversation_id,
                    &attachment_id,
                    &file_name,
                    max_bytes,
                )
            })
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: path.clone(),
                source: std::io::Error::other(format!("attachment read task failed: {source}")),
            })?
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => {
                    ApplicationError::AttachmentNotFound(attachment_label)
                }
                std::io::ErrorKind::InvalidData => ApplicationError::InvalidAttachment(format!(
                    "attachment exceeds the {max_bytes}-byte read limit"
                )),
                _ => ApplicationError::Workspace { path, source },
            })
        }

        #[cfg(not(unix))]
        {
            use tokio::io::AsyncReadExt;

            let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ApplicationError::AttachmentNotFound(attachment_id.to_string())
                } else {
                    ApplicationError::Workspace {
                        path: path.clone(),
                        source,
                    }
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ApplicationError::InvalidAttachment(
                    "attachment path is not a regular file".to_string(),
                ));
            }
            let file = tokio::fs::File::open(&path).await.map_err(|source| {
                ApplicationError::Workspace {
                    path: path.clone(),
                    source,
                }
            })?;
            let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes as u64) as usize);
            file.take(max_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|source| ApplicationError::Workspace {
                    path: path.clone(),
                    source,
                })?;
            if bytes.len() > max_bytes {
                return Err(ApplicationError::InvalidAttachment(format!(
                    "attachment exceeds the {max_bytes}-byte read limit"
                )));
            }
            Ok(bytes)
        }
    }
}

#[cfg(unix)]
fn ensure_workspace_tree_fd_relative(
    root: &Path,
    application_public_id: &str,
    project_slugs: &[String],
) -> std::io::Result<()> {
    use rustix::fs::{mkdirat, open, openat, Mode, OFlags};
    use std::os::fd::{AsFd, OwnedFd};

    fn open_or_create_dir(parent: impl AsFd, component: &str) -> std::io::Result<OwnedFd> {
        if let Err(error) = mkdirat(parent.as_fd(), component, Mode::from_bits_truncate(0o755)) {
            if error != rustix::io::Errno::EXIST {
                return Err(std::io::Error::from_raw_os_error(error.raw_os_error()));
            }
        }
        openat(
            parent.as_fd(),
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))
    }

    let root = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let workspace = open_or_create_dir(&root, application_public_id)?;
    let projects = open_or_create_dir(&workspace, "projects")?;
    for slug in project_slugs {
        open_or_create_dir(&projects, slug)?;
    }
    Ok(())
}

#[cfg(unix)]
fn store_project_files_fd_relative(
    root: &Path,
    application_public_id: &str,
    project_slug: &str,
    files: &[(PathBuf, Vec<u8>, Option<u32>)],
    max_workspace_bytes: u64,
    max_workspace_entries: usize,
) -> std::io::Result<usize> {
    use rustix::fs::{mkdirat, open, openat, Mode, OFlags};
    use std::fs::File;
    use std::os::fd::{AsFd, OwnedFd};

    fn open_dir(parent: impl AsFd, component: &std::ffi::OsStr) -> std::io::Result<OwnedFd> {
        openat(
            parent.as_fd(),
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))
    }

    fn open_or_create_dir(
        parent: impl AsFd,
        component: &std::ffi::OsStr,
    ) -> std::io::Result<OwnedFd> {
        if let Err(error) = mkdirat(parent.as_fd(), component, Mode::from_bits_truncate(0o755)) {
            if error != rustix::io::Errno::EXIST {
                return Err(std::io::Error::from_raw_os_error(error.raw_os_error()));
            }
        }
        open_dir(parent, component)
    }

    let root_fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let application_fd = open_dir(&root_fd, std::ffi::OsStr::new(application_public_id))?;
    let projects_fd = open_dir(&application_fd, std::ffi::OsStr::new("projects"))?;
    let project_fd = open_dir(&projects_fd, std::ffi::OsStr::new(project_slug))?;

    let application_path = root.join(application_public_id);
    let mut existing_bytes = 0_u64;
    let mut existing_entries = 0_usize;
    for entry in walkdir::WalkDir::new(&application_path)
        .min_depth(1)
        .follow_links(false)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        existing_entries = existing_entries
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("workspace entry count overflowed"))?;
        if entry.file_type().is_file() {
            existing_bytes = existing_bytes
                .checked_add(entry.metadata().map_err(std::io::Error::other)?.len())
                .ok_or_else(|| std::io::Error::other("workspace size overflowed"))?;
        }
    }
    let incoming_bytes = files.iter().try_fold(0_u64, |total, (_, bytes, _)| {
        total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("workspace import size overflowed"))
    })?;
    // This deliberately counts every component in every incoming path. It is
    // a conservative upper bound (shared directories may be counted more than
    // once) and therefore cannot undercount new zero-byte files or directories.
    let incoming_entries = files.iter().try_fold(0_usize, |total, (path, _, _)| {
        total
            .checked_add(path.components().count())
            .ok_or_else(|| std::io::Error::other("workspace entry count overflowed"))
    })?;
    if existing_bytes
        .checked_add(incoming_bytes)
        .is_none_or(|total| total > max_workspace_bytes)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("workspace import would exceed the {max_workspace_bytes}-byte aggregate limit"),
        ));
    }
    if existing_entries
        .checked_add(incoming_entries)
        .is_none_or(|total| total > max_workspace_entries)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!(
                "workspace import would exceed the {max_workspace_entries}-entry aggregate limit"
            ),
        ));
    }

    for (relative, contents, mode) in files {
        let components = relative.components().collect::<Vec<_>>();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace import path contains a non-normal component",
            ));
        }
        let mut parent: OwnedFd = project_fd.try_clone()?;
        for component in &components[..components.len() - 1] {
            let std::path::Component::Normal(component) = component else {
                unreachable!("workspace import components were validated")
            };
            parent = open_or_create_dir(&parent, component)?;
        }
        let std::path::Component::Normal(file_name) = components[components.len() - 1] else {
            unreachable!("workspace import components were validated")
        };
        let file = openat(
            &parent,
            file_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            // rustix uses the platform's native mode_t width (u16 on Darwin,
            // u32 on Linux), so let the Mode constructor select that width.
            Mode::from_bits_truncate((mode.unwrap_or(0o644) & 0o777) as _),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let mut file = File::from(file);
        file.write_all(contents)?;
        file.sync_all()?;
    }
    Ok(files.len())
}

#[cfg(not(unix))]
fn store_project_files_fd_relative(
    _root: &Path,
    _application_public_id: &str,
    _project_slug: &str,
    _files: &[(PathBuf, Vec<u8>, Option<u32>)],
    _max_workspace_bytes: u64,
    _max_workspace_entries: usize,
) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace imports require descriptor-relative filesystem support",
    ))
}

fn chat_attachment_usage(root: &Path) -> std::io::Result<(u64, usize)> {
    if !root.exists() {
        return Ok((0, 0));
    }
    let root_metadata = std::fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment storage root is not a regular directory",
        ));
    }
    let mut bytes = 0_u64;
    let mut files = 0_usize;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| std::io::Error::other(error.to_string()))?;
        if entry.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "attachment storage contains a symbolic link",
            ));
        }
        if entry.file_type().is_file() {
            files = files.saturating_add(1);
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok((bytes, files))
}

#[cfg(unix)]
fn store_chat_attachment_fd_relative(
    root: &Path,
    workspace_id: &str,
    conversation_id: &str,
    attachment_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    use rustix::fs::{mkdirat, open, openat, Mode, OFlags};
    use std::fs::File;
    use std::os::fd::{AsFd, OwnedFd};

    fn open_or_create_dir(parent: impl AsFd, component: &str) -> std::io::Result<OwnedFd> {
        if let Err(error) = mkdirat(parent.as_fd(), component, Mode::from_bits_truncate(0o755)) {
            if error != rustix::io::Errno::EXIST {
                return Err(std::io::Error::from_raw_os_error(error.raw_os_error()));
            }
        }
        openat(
            parent.as_fd(),
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))
    }

    let root = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let workspace = open_or_create_dir(&root, workspace_id)?;
    let temps = open_or_create_dir(&workspace, ".temps")?;
    let attachments = open_or_create_dir(&temps, "chat-attachments")?;
    let conversation = open_or_create_dir(&attachments, conversation_id)?;
    let attachment = open_or_create_dir(&conversation, attachment_id)?;
    let file = openat(
        &attachment,
        file_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let mut file = File::from(file);
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn chat_attachment_size_fd_relative(
    root: &Path,
    workspace_id: &str,
    conversation_id: &str,
    attachment_id: &str,
    file_name: &str,
) -> std::io::Result<u64> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use std::fs::File;
    use std::os::fd::{AsFd, OwnedFd};

    fn open_dir(parent: impl AsFd, component: &str) -> std::io::Result<OwnedFd> {
        openat(
            parent.as_fd(),
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))
    }

    let root = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let workspace = open_dir(&root, workspace_id)?;
    let temps = open_dir(&workspace, ".temps")?;
    let attachments = open_dir(&temps, "chat-attachments")?;
    let conversation = open_dir(&attachments, conversation_id)?;
    let attachment = open_dir(&conversation, attachment_id)?;
    let file = openat(
        &attachment,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    File::from(file).metadata().map(|metadata| metadata.len())
}

#[cfg(unix)]
fn read_chat_attachment_fd_relative(
    root: &Path,
    workspace_id: &str,
    conversation_id: &str,
    attachment_id: &str,
    file_name: &str,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use std::fs::File;
    use std::os::fd::{AsFd, OwnedFd};

    fn open_dir(parent: impl AsFd, component: &str) -> std::io::Result<OwnedFd> {
        openat(
            parent.as_fd(),
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))
    }

    let root = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let workspace = open_dir(&root, workspace_id)?;
    let temps = open_dir(&workspace, ".temps")?;
    let attachments = open_dir(&temps, "chat-attachments")?;
    let conversation = open_dir(&attachments, conversation_id)?;
    let attachment = open_dir(&conversation, attachment_id)?;
    let file = openat(
        &attachment,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let mut bytes = Vec::new();
    File::from(file)
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment exceeds read limit",
        ));
    }
    Ok(bytes)
}

async fn ensure_trusted_directory(
    path: &Path,
    allow_missing_parents: bool,
) -> Result<(), ApplicationError> {
    let created = if allow_missing_parents {
        tokio::fs::create_dir_all(path).await
    } else {
        match tokio::fs::create_dir(path).await {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(source) => Err(source),
        }
    };
    created.map_err(|source| ApplicationError::Workspace {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata =
        tokio::fs::symlink_metadata(path)
            .await
            .map_err(|source| ApplicationError::Workspace {
                path: path.to_path_buf(),
                source,
            })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ApplicationError::Workspace {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace path must be a real directory, not a symlink or file",
            ),
        });
    }
    Ok(())
}

fn validate_workspace_component(value: &str) -> Result<(), ApplicationError> {
    if value.is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ApplicationError::InvalidWorkspaceIdentifier(
            value.to_string(),
        ));
    }
    Ok(())
}

fn validate_attachment_file_name(value: &str) -> Result<(), ApplicationError> {
    if value.is_empty()
        || value.len() > 180
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ApplicationError::InvalidAttachment(
            "file name contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

pub struct ApplicationService {
    db: Arc<DatabaseConnection>,
    /// The supported deployment is a single Temps control-plane process.
    /// Serialize topology/resource mutations so quota and primary-project
    /// invariants cannot be defeated by concurrent requests.
    mutation_lock: Arc<tokio::sync::Mutex<()>>,
}

impl ApplicationService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn create(
        &self,
        user_id: i32,
        name: &str,
        description: Option<&str>,
        project_ids: &[i32],
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(ApplicationError::InvalidName);
        }
        let unique = project_ids.iter().copied().collect::<HashSet<_>>();
        if unique.len() != project_ids.len() || unique.len() > MAX_PROJECTS {
            return Err(ApplicationError::InvalidProjects);
        }
        self.ensure_workspace_quota(user_id, None, &WorkspaceQuotaValues::default())
            .await?;

        let mut found_projects = if project_ids.is_empty() {
            Vec::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
                .filter(projects::Column::IsDeleted.eq(false))
                .all(self.db.as_ref())
                .await?
        };
        if found_projects.len() != project_ids.len() {
            let found = found_projects
                .iter()
                .map(|project| project.id)
                .collect::<HashSet<_>>();
            let missing = project_ids
                .iter()
                .find(|project_id| !found.contains(project_id))
                .copied()
                .unwrap_or_default();
            return Err(ApplicationError::ProjectNotFound(missing));
        }
        found_projects.sort_by_key(|project| {
            project_ids
                .iter()
                .position(|project_id| *project_id == project.id)
                .unwrap_or(usize::MAX)
        });

        let txn = self.db.begin().await?;
        let now = Utc::now();
        let application = ai_applications::ActiveModel {
            public_id: Set(format!("app_{}", uuid::Uuid::new_v4().simple())),
            name: Set(name.to_string()),
            description: Set(description
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await?;

        for (index, project_id) in project_ids.iter().enumerate() {
            ai_application_projects::ActiveModel {
                application_id: Set(application.id),
                project_id: Set(*project_id),
                is_primary: Set(index == 0),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
        }
        ai_application_workspaces::ActiveModel {
            application_id: Set(application.id),
            desired_state: Set("running".to_string()),
            runtime: Set("node".to_string()),
            cpu_limit: Set(4.0),
            memory_limit_mb: Set(8192),
            pids_limit: Set(512),
            disk_limit_mb: Set(10_240),
            idle_timeout_secs: Set(900),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        txn.commit().await?;

        Ok(ApplicationWithProjects {
            application,
            projects: found_projects,
            primary_project_id: project_ids.first().copied(),
            environment_statuses: HashMap::new(),
        })
    }

    pub async fn list(
        &self,
        user_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ApplicationWithProjects>, ApplicationError> {
        self.list_with_status(user_id, page, page_size, "active")
            .await
    }

    pub async fn list_with_status(
        &self,
        user_id: i32,
        page: u64,
        page_size: u64,
        status: &str,
    ) -> Result<Vec<ApplicationWithProjects>, ApplicationError> {
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq(status))
            .order_by_desc(ai_applications::Column::UpdatedAt)
            .offset(page.saturating_sub(1).saturating_mul(page_size))
            .limit(page_size)
            .all(self.db.as_ref())
            .await?;
        if applications.is_empty() {
            return Ok(Vec::new());
        }

        // Load the complete topology in two batched queries. Application lists
        // are used by the global chat switcher, so one query per application
        // would become increasingly expensive as a workspace grows.
        let application_ids = applications
            .iter()
            .map(|application| application.id)
            .collect::<Vec<_>>();
        let links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.is_in(application_ids))
            .order_by_asc(ai_application_projects::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let project_ids = links
            .iter()
            .map(|link| link.project_id)
            .collect::<HashSet<_>>();
        let project_by_id = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|project| (project.id, project))
            .collect::<HashMap<_, _>>();
        let environment_statuses = self
            .project_environment_statuses(&project_ids.iter().copied().collect::<Vec<_>>())
            .await?;
        let mut projects_by_application = HashMap::<i64, Vec<projects::Model>>::new();
        let mut primary_by_application = HashMap::<i64, i32>::new();
        for link in links {
            if link.is_primary {
                primary_by_application.insert(link.application_id, link.project_id);
            }
            if let Some(project) = project_by_id.get(&link.project_id) {
                projects_by_application
                    .entry(link.application_id)
                    .or_default()
                    .push(project.clone());
            }
        }

        Ok(applications
            .into_iter()
            .map(|application| {
                let application_projects = projects_by_application
                    .remove(&application.id)
                    .unwrap_or_default();
                let primary_project_id = primary_by_application
                    .remove(&application.id)
                    .or_else(|| application_projects.first().map(|project| project.id));
                let application_environment_statuses = application_projects
                    .iter()
                    .filter_map(|project| {
                        environment_statuses
                            .get(&project.id)
                            .cloned()
                            .map(|statuses| (project.id, statuses))
                    })
                    .collect();
                ApplicationWithProjects {
                    projects: application_projects,
                    primary_project_id,
                    environment_statuses: application_environment_statuses,
                    application,
                }
            })
            .collect())
    }

    /// Compensating action used only while application creation is still in
    /// progress. The application is not returned to the caller until its
    /// persistent workspace exists, so a filesystem failure must not leave a
    /// half-created topology in the database.
    pub async fn rollback_failed_create(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<(), ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::NotFound(public_id.to_string()))?;
        ai_applications::Entity::delete_by_id(application.id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Archive an application without deleting its projects, conversations, or
    /// persistent workspace volume. Archived applications disappear from the
    /// active workspace list and their compute is left in the paused state.
    pub async fn archive(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<Vec<i32>, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = self.get(user_id, public_id).await?;
        let project_ids = application
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();
        let txn = self.db.begin().await?;
        let now = Utc::now();

        let application_id = application.application.id;
        let mut active: ai_applications::ActiveModel = application.application.into();
        active.status = Set("archived".to_string());
        active.updated_at = Set(now);
        active.update(&txn).await?;

        if let Some(workspace) = ai_application_workspaces::Entity::find()
            .filter(ai_application_workspaces::Column::ApplicationId.eq(application_id))
            .one(&txn)
            .await?
        {
            let mut workspace: ai_application_workspaces::ActiveModel = workspace.into();
            workspace.desired_state = Set("paused".to_string());
            workspace.updated_at = Set(now);
            workspace.update(&txn).await?;
        }

        txn.commit().await?;
        Ok(project_ids)
    }

    pub async fn get(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        self.get_with_status(user_id, public_id, "active").await
    }

    pub async fn get_with_status(
        &self,
        user_id: i32,
        public_id: &str,
        status: &str,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let application = ai_applications::Entity::find()
            .filter(ai_applications::Column::PublicId.eq(public_id))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq(status))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::NotFound(public_id.to_string()))?;
        let (projects, primary_project_id, environment_statuses) =
            self.projects(application.id).await?;
        Ok(ApplicationWithProjects {
            application,
            projects,
            primary_project_id,
            environment_statuses,
        })
    }

    /// Restore an archived application and request that its durable workspace
    /// resume on the next access. Projects, conversations, and files were never
    /// deleted by archive, so the active topology becomes readable again.
    pub async fn restore(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = self.get_with_status(user_id, public_id, "archived").await?;
        let application_id = application.application.id;
        let txn = self.db.begin().await?;
        let now = Utc::now();

        let mut active: ai_applications::ActiveModel = application.application.into();
        active.status = Set("active".to_string());
        active.updated_at = Set(now);
        active.update(&txn).await?;

        if let Some(workspace) = ai_application_workspaces::Entity::find()
            .filter(ai_application_workspaces::Column::ApplicationId.eq(application_id))
            .one(&txn)
            .await?
        {
            let mut workspace: ai_application_workspaces::ActiveModel = workspace.into();
            workspace.desired_state = Set("running".to_string());
            workspace.updated_at = Set(now);
            workspace.update(&txn).await?;
        }

        txn.commit().await?;
        self.get(user_id, public_id).await
    }

    /// Batch-load the minimum application topology needed for authorization.
    /// Conversation rows already carry the application FK, so callers should
    /// not parse the public id out of `context_id` or hydrate full projects.
    pub(crate) async fn project_scopes(
        &self,
        user_id: i32,
        application_ids: &[i64],
    ) -> Result<HashMap<i64, ApplicationProjectScope>, ApplicationError> {
        let application_ids = application_ids.iter().copied().collect::<HashSet<_>>();
        if application_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::Id.is_in(application_ids.iter().copied()))
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;
        let visible_ids = applications
            .iter()
            .map(|application| application.id)
            .collect::<Vec<_>>();
        let links = if visible_ids.is_empty() {
            Vec::new()
        } else {
            ai_application_projects::Entity::find()
                .filter(ai_application_projects::Column::ApplicationId.is_in(visible_ids))
                .order_by_asc(ai_application_projects::Column::Id)
                .all(self.db.as_ref())
                .await?
        };
        let mut scopes = applications
            .into_iter()
            .map(|application| {
                (
                    application.id,
                    ApplicationProjectScope {
                        public_id: application.public_id,
                        project_ids: Vec::new(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        for link in links {
            if let Some(scope) = scopes.get_mut(&link.application_id) {
                scope.project_ids.push(link.project_id);
            }
        }
        Ok(scopes)
    }

    pub async fn workspace(
        &self,
        application_id: i64,
    ) -> Result<ai_application_workspaces::Model, ApplicationError> {
        ai_application_workspaces::Entity::find()
            .filter(ai_application_workspaces::Column::ApplicationId.eq(application_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::NotFound(format!("workspace:{application_id}")))
    }

    pub async fn update_workspace(
        &self,
        user_id: i32,
        application_id: i64,
        update: WorkspaceSettingsUpdate,
    ) -> Result<ai_application_workspaces::Model, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        update.validate()?;
        let current = self.workspace(application_id).await?;
        let next_image = update
            .image
            .as_ref()
            .map(|image| image.as_deref())
            .unwrap_or(current.image.as_deref());
        if next_image.is_some() {
            return Err(ApplicationError::InvalidWorkspaceSetting(
                "custom workspace images are disabled; choose a trusted built-in runtime"
                    .to_string(),
            ));
        }
        let prospective = WorkspaceQuotaValues {
            cpu: update.cpu_limit.unwrap_or(current.cpu_limit),
            memory_mb: update.memory_limit_mb.unwrap_or(current.memory_limit_mb),
            pids: update.pids_limit.unwrap_or(current.pids_limit),
            disk_mb: update.disk_limit_mb.unwrap_or(current.disk_limit_mb),
        };
        self.ensure_workspace_quota(user_id, Some(application_id), &prospective)
            .await?;
        let mut active: ai_application_workspaces::ActiveModel = current.into();
        if let Some(value) = update.runtime {
            active.runtime = Set(value);
        }
        if let Some(value) = update.image {
            active.image = Set(value);
        }
        if let Some(value) = update.cpu_limit {
            active.cpu_limit = Set(value);
        }
        if let Some(value) = update.memory_limit_mb {
            active.memory_limit_mb = Set(value);
        }
        if let Some(value) = update.pids_limit {
            active.pids_limit = Set(value);
        }
        if let Some(value) = update.disk_limit_mb {
            active.disk_limit_mb = Set(value);
        }
        if let Some(value) = update.idle_timeout_secs {
            active.idle_timeout_secs = Set(value);
        }
        active.updated_at = Set(Utc::now());
        Ok(active.update(self.db.as_ref()).await?)
    }

    pub async fn record_workspace_sandbox(
        &self,
        application_id: i64,
        sandbox_public_id: Option<String>,
        desired_state: Option<&str>,
        last_error: Option<String>,
    ) -> Result<ai_application_workspaces::Model, ApplicationError> {
        let current = self.workspace(application_id).await?;
        let mut active: ai_application_workspaces::ActiveModel = current.into();
        active.sandbox_public_id = Set(sandbox_public_id);
        if let Some(state) = desired_state {
            active.desired_state = Set(state.to_string());
        }
        active.last_error = Set(last_error);
        active.updated_at = Set(Utc::now());
        Ok(active.update(self.db.as_ref()).await?)
    }

    /// Update desired lifecycle/error state without discarding the durable
    /// sandbox identity. This is used before resume/recovery: clearing the
    /// identity would make a transient provider failure look like a brand-new
    /// workspace and hide the compute that still owns the persistent volume.
    pub async fn update_workspace_runtime_state(
        &self,
        application_id: i64,
        desired_state: Option<&str>,
        last_error: Option<String>,
    ) -> Result<ai_application_workspaces::Model, ApplicationError> {
        let current = self.workspace(application_id).await?;
        let mut active: ai_application_workspaces::ActiveModel = current.into();
        if let Some(state) = desired_state {
            active.desired_state = Set(state.to_string());
        }
        active.last_error = Set(last_error);
        active.updated_at = Set(Utc::now());
        Ok(active.update(self.db.as_ref()).await?)
    }

    pub async fn link_project(
        &self,
        user_id: i32,
        public_id: &str,
        project_id: i32,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = self.get(user_id, public_id).await?;
        if application
            .projects
            .iter()
            .any(|project| project.id == project_id)
        {
            return Err(ApplicationError::ProjectAlreadyLinked {
                application_id: public_id.to_string(),
                project_id,
            });
        }
        if application.projects.len() >= MAX_PROJECTS {
            return Err(ApplicationError::InvalidProjects);
        }
        let exists = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .is_some();
        if !exists {
            return Err(ApplicationError::ProjectNotFound(project_id));
        }
        let now = Utc::now();
        ai_application_projects::ActiveModel {
            application_id: Set(application.application.id),
            project_id: Set(project_id),
            // Self-heal legacy/inconsistent topologies while linking. New
            // applications always have a primary, but imported databases may
            // contain links created before that invariant existed.
            is_primary: Set(application.primary_project_id.is_none()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        self.touch(application.application.id).await?;
        self.get(user_id, public_id).await
    }

    pub async fn unlink_project(
        &self,
        user_id: i32,
        public_id: &str,
        project_id: i32,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = self.get(user_id, public_id).await?;
        let link = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application.application.id))
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::ProjectNotLinked {
                application_id: public_id.to_string(),
                project_id,
            })?;
        let was_primary = link.is_primary;
        let txn = self.db.begin().await?;
        ai_application_projects::Entity::delete_by_id(link.id)
            .exec(&txn)
            .await?;
        if was_primary {
            if let Some(replacement) = ai_application_projects::Entity::find()
                .filter(
                    ai_application_projects::Column::ApplicationId.eq(application.application.id),
                )
                .order_by_asc(ai_application_projects::Column::Id)
                .one(&txn)
                .await?
            {
                let mut active: ai_application_projects::ActiveModel = replacement.into();
                active.is_primary = Set(true);
                active.update(&txn).await?;
            }
        }
        txn.commit().await?;
        self.touch(application.application.id).await?;
        self.get(user_id, public_id).await
    }

    pub async fn set_primary_project(
        &self,
        user_id: i32,
        public_id: &str,
        project_id: i32,
    ) -> Result<ApplicationWithProjects, ApplicationError> {
        let _mutation = self.mutation_lock.lock().await;
        let application = self.get(user_id, public_id).await?;
        if !application
            .projects
            .iter()
            .any(|project| project.id == project_id)
        {
            return Err(ApplicationError::ProjectNotLinked {
                application_id: public_id.to_string(),
                project_id,
            });
        }
        let txn = self.db.begin().await?;
        ai_application_projects::Entity::update_many()
            .col_expr(ai_application_projects::Column::IsPrimary, false.into())
            .filter(ai_application_projects::Column::ApplicationId.eq(application.application.id))
            .exec(&txn)
            .await?;
        ai_application_projects::Entity::update_many()
            .col_expr(ai_application_projects::Column::IsPrimary, true.into())
            .filter(ai_application_projects::Column::ApplicationId.eq(application.application.id))
            .filter(ai_application_projects::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;
        txn.commit().await?;
        self.touch(application.application.id).await?;
        self.get(user_id, public_id).await
    }

    async fn touch(&self, application_id: i64) -> Result<(), ApplicationError> {
        let application = ai_applications::Entity::find_by_id(application_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApplicationError::NotFound(application_id.to_string()))?;
        let mut active: ai_applications::ActiveModel = application.into();
        active.updated_at = Set(Utc::now());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    pub async fn conversations(
        &self,
        application_id: i64,
        user_id: i32,
    ) -> Result<Vec<ai_conversations::Model>, ApplicationError> {
        self.conversations_with_status(application_id, user_id, "active", 1, 20)
            .await
    }

    pub async fn conversations_with_status(
        &self,
        application_id: i64,
        user_id: i32,
        status: &str,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ai_conversations::Model>, ApplicationError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq(status))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .offset(page.saturating_sub(1).saturating_mul(page_size))
            .limit(page_size)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn create_artifact(
        &self,
        application_id: i64,
        conversation_public_id: &str,
        user_id: i32,
        kind: &str,
        title: Option<&str>,
        payload: Value,
    ) -> Result<ai_thread_artifacts::Model, ApplicationError> {
        if !is_allowed_artifact_kind(kind) {
            return Err(ApplicationError::InvalidArtifactKind(kind.to_string()));
        }
        validate_secret_free_payload(&payload, "$")?;
        let conversation = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::PublicId.eq(conversation_public_id))
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApplicationError::ConversationNotFound(conversation_public_id.to_string())
            })?;
        let now = Utc::now();
        Ok(ai_thread_artifacts::ActiveModel {
            public_id: Set(format!("art_{}", uuid::Uuid::new_v4().simple())),
            conversation_id: Set(conversation.id),
            application_id: Set(application_id),
            kind: Set(kind.to_string()),
            schema_version: Set(1),
            title: Set(title
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)),
            payload: Set(payload),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?)
    }

    pub async fn artifacts(
        &self,
        application_id: i64,
        conversation_public_id: &str,
        user_id: i32,
    ) -> Result<Vec<ai_thread_artifacts::Model>, ApplicationError> {
        let conversation = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::PublicId.eq(conversation_public_id))
            .filter(ai_conversations::Column::ApplicationId.eq(application_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApplicationError::ConversationNotFound(conversation_public_id.to_string())
            })?;
        Ok(ai_thread_artifacts::Entity::find()
            .filter(ai_thread_artifacts::Column::ConversationId.eq(conversation.id))
            .filter(ai_thread_artifacts::Column::Status.eq("active"))
            .order_by_asc(ai_thread_artifacts::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?)
    }

    async fn projects(
        &self,
        application_id: i64,
    ) -> Result<
        (
            Vec<projects::Model>,
            Option<i32>,
            HashMap<i32, Vec<ProjectEnvironmentStatus>>,
        ),
        ApplicationError,
    > {
        let links = ai_application_projects::Entity::find()
            .filter(ai_application_projects::Column::ApplicationId.eq(application_id))
            .order_by_asc(ai_application_projects::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let ids = links.iter().map(|link| link.project_id).collect::<Vec<_>>();
        let mut projects = if ids.is_empty() {
            Vec::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(ids.iter().copied()))
                .all(self.db.as_ref())
                .await?
        };
        projects.sort_by_key(|project| {
            ids.iter()
                .position(|id| *id == project.id)
                .unwrap_or(usize::MAX)
        });
        let primary_project_id = links
            .iter()
            .find(|link| link.is_primary)
            .map(|link| link.project_id)
            .or_else(|| links.first().map(|link| link.project_id));
        let environment_statuses = self.project_environment_statuses(&ids).await?;
        Ok((projects, primary_project_id, environment_statuses))
    }

    async fn project_environment_statuses(
        &self,
        project_ids: &[i32],
    ) -> Result<HashMap<i32, Vec<ProjectEnvironmentStatus>>, ApplicationError> {
        if project_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut rows = environments::Entity::find()
            .filter(environments::Column::ProjectId.is_in(project_ids.iter().copied()))
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;
        rows.sort_by(|left, right| left.name.cmp(&right.name));
        let deployment_ids = rows
            .iter()
            .filter_map(|environment| environment.current_deployment_id)
            .collect::<Vec<_>>();
        let deployment_states = if deployment_ids.is_empty() {
            HashMap::new()
        } else {
            deployments::Entity::find()
                .filter(deployments::Column::Id.is_in(deployment_ids))
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .map(|deployment| (deployment.id, deployment.state))
                .collect::<HashMap<_, _>>()
        };
        let mut by_project = HashMap::<i32, Vec<ProjectEnvironmentStatus>>::new();
        for environment in rows {
            by_project
                .entry(environment.project_id)
                .or_default()
                .push(ProjectEnvironmentStatus {
                    name: environment.name,
                    slug: environment.slug,
                    sleeping: environment.sleeping,
                    deployment_state: environment
                        .current_deployment_id
                        .and_then(|id| deployment_states.get(&id).cloned()),
                });
        }
        Ok(by_project)
    }

    async fn ensure_workspace_quota(
        &self,
        user_id: i32,
        replacing_application_id: Option<i64>,
        prospective: &WorkspaceQuotaValues,
    ) -> Result<(), ApplicationError> {
        let applications = ai_applications::Entity::find()
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;
        if replacing_application_id.is_none() && applications.len() >= MAX_APPLICATIONS_PER_USER {
            return Err(ApplicationError::WorkspaceQuota(format!(
                "at most {MAX_APPLICATIONS_PER_USER} active application workspaces are allowed per user"
            )));
        }
        let application_ids = applications
            .iter()
            .map(|application| application.id)
            .collect::<Vec<_>>();
        let workspaces = if application_ids.is_empty() {
            Vec::new()
        } else {
            ai_application_workspaces::Entity::find()
                .filter(ai_application_workspaces::Column::ApplicationId.is_in(application_ids))
                .all(self.db.as_ref())
                .await?
        };
        let mut aggregate = WorkspaceQuotaValues::default_empty();
        let global_workspace_exists = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(format!("ai-application:global-user-{user_id}")))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?
            .is_some();
        if global_workspace_exists {
            aggregate.add(WorkspaceQuotaValues::default());
        }
        for workspace in workspaces {
            if replacing_application_id == Some(workspace.application_id) {
                continue;
            }
            aggregate.cpu += workspace.cpu_limit;
            aggregate.memory_mb += workspace.memory_limit_mb;
            aggregate.pids += workspace.pids_limit;
            aggregate.disk_mb += workspace.disk_limit_mb;
        }
        aggregate.cpu += prospective.cpu;
        aggregate.memory_mb += prospective.memory_mb;
        aggregate.pids += prospective.pids;
        aggregate.disk_mb += prospective.disk_mb;
        aggregate.validate_aggregate()
    }

    /// Reserve aggregate capacity while the user's singleton global workspace
    /// is created. The returned guard must be retained until the sandbox row
    /// exists; otherwise an application create could pass admission between
    /// this check and the global workspace becoming visible in the database.
    pub async fn reserve_global_workspace_quota(
        &self,
        user_id: i32,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, ApplicationError> {
        let guard = self.mutation_lock.clone().lock_owned().await;
        let global_workspace_exists = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(Some(user_id)))
            .filter(sandboxes::Column::Name.eq(format!("ai-application:global-user-{user_id}")))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await?
            .is_some();
        if global_workspace_exists {
            return Ok(guard);
        }
        let application_ids = ai_applications::Entity::find()
            .filter(ai_applications::Column::CreatedBy.eq(user_id))
            .filter(ai_applications::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|application| application.id)
            .collect::<Vec<_>>();
        let workspaces = if application_ids.is_empty() {
            Vec::new()
        } else {
            ai_application_workspaces::Entity::find()
                .filter(ai_application_workspaces::Column::ApplicationId.is_in(application_ids))
                .all(self.db.as_ref())
                .await?
        };
        let mut aggregate = WorkspaceQuotaValues::default();
        for workspace in workspaces {
            aggregate.cpu += workspace.cpu_limit;
            aggregate.memory_mb += workspace.memory_limit_mb;
            aggregate.pids += workspace.pids_limit;
            aggregate.disk_mb += workspace.disk_limit_mb;
        }
        aggregate.validate_aggregate()?;
        Ok(guard)
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceQuotaValues {
    cpu: f64,
    memory_mb: i64,
    pids: i64,
    disk_mb: i64,
}

impl WorkspaceQuotaValues {
    fn default_empty() -> Self {
        Self {
            cpu: 0.0,
            memory_mb: 0,
            pids: 0,
            disk_mb: 0,
        }
    }

    fn validate_aggregate(self) -> Result<(), ApplicationError> {
        if self.cpu > MAX_USER_WORKSPACE_CPU
            || self.memory_mb > MAX_USER_WORKSPACE_MEMORY_MB
            || self.pids > MAX_USER_WORKSPACE_PIDS
            || self.disk_mb > MAX_USER_WORKSPACE_DISK_MB
        {
            return Err(ApplicationError::WorkspaceQuota(format!(
                "requested totals exceed the per-user limits ({MAX_USER_WORKSPACE_CPU} CPU, {MAX_USER_WORKSPACE_MEMORY_MB} MiB memory, {MAX_USER_WORKSPACE_PIDS} PIDs, {MAX_USER_WORKSPACE_DISK_MB} MiB disk)"
            )));
        }
        Ok(())
    }

    fn add(&mut self, other: Self) {
        self.cpu += other.cpu;
        self.memory_mb += other.memory_mb;
        self.pids += other.pids;
        self.disk_mb += other.disk_mb;
    }
}

impl Default for WorkspaceQuotaValues {
    fn default() -> Self {
        Self {
            cpu: 4.0,
            memory_mb: 8_192,
            pids: 512,
            disk_mb: 10_240,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceSettingsUpdate {
    pub runtime: Option<String>,
    /// `Some(None)` clears a custom image; `None` leaves it unchanged.
    pub image: Option<Option<String>>,
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<i64>,
    pub pids_limit: Option<i64>,
    pub disk_limit_mb: Option<i64>,
    pub idle_timeout_secs: Option<i64>,
}

impl WorkspaceSettingsUpdate {
    fn validate(&self) -> Result<(), ApplicationError> {
        if self.image.as_ref().is_some_and(Option::is_some) {
            return Err(ApplicationError::InvalidWorkspaceSetting(
                "custom workspace images are disabled; choose a trusted built-in runtime"
                    .to_string(),
            ));
        }
        if let Some(runtime) = self.runtime.as_deref() {
            if !matches!(runtime, "node" | "bun" | "python" | "rust" | "go" | "full") {
                return Err(ApplicationError::InvalidWorkspaceSetting(format!(
                    "runtime '{runtime}' is not supported"
                )));
            }
        }
        if self
            .cpu_limit
            .is_some_and(|value| !(0.25..=MAX_WORKSPACE_CPU).contains(&value))
        {
            return Err(ApplicationError::InvalidWorkspaceSetting(format!(
                "cpu_limit must be between 0.25 and {MAX_WORKSPACE_CPU} cores"
            )));
        }
        if self
            .memory_limit_mb
            .is_some_and(|value| !(256..=MAX_WORKSPACE_MEMORY_MB).contains(&value))
        {
            return Err(ApplicationError::InvalidWorkspaceSetting(format!(
                "memory_limit_mb must be between 256 and {MAX_WORKSPACE_MEMORY_MB}"
            )));
        }
        if self
            .pids_limit
            .is_some_and(|value| !(64..=MAX_WORKSPACE_PIDS).contains(&value))
        {
            return Err(ApplicationError::InvalidWorkspaceSetting(format!(
                "pids_limit must be between 64 and {MAX_WORKSPACE_PIDS}"
            )));
        }
        if self
            .disk_limit_mb
            .is_some_and(|value| !(512..=MAX_WORKSPACE_DISK_MB).contains(&value))
        {
            return Err(ApplicationError::InvalidWorkspaceSetting(format!(
                "disk_limit_mb must be between 512 and {MAX_WORKSPACE_DISK_MB}"
            )));
        }
        if self
            .idle_timeout_secs
            .is_some_and(|value| !(60..=86_400).contains(&value))
        {
            return Err(ApplicationError::InvalidWorkspaceSetting(
                "idle_timeout_secs must be between 60 and 86400".to_string(),
            ));
        }
        Ok(())
    }
}

fn validate_secret_free_payload(value: &Value, path: &str) -> Result<(), ApplicationError> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                let normalized = key.to_ascii_lowercase();
                let is_reference =
                    normalized.ends_with("_ref") || normalized.ends_with("_reference");
                let is_secret_field = [
                    "secret",
                    "password",
                    "token",
                    "api_key",
                    "private_key",
                    "credential",
                ]
                .iter()
                .any(|marker| normalized == *marker || normalized.ends_with(&format!("_{marker}")));
                // References are not a free-form escape hatch. The only value
                // that may sit under a reference key is a canonical, opaque
                // broker id; accepting arbitrary strings here would persist a
                // caller-supplied secret in an artifact.
                if is_reference {
                    if !child.as_str().is_some_and(is_opaque_credential_reference) {
                        return Err(ApplicationError::SecretValue(child_path));
                    }
                    continue;
                }
                if is_secret_field && !child.is_null() {
                    return Err(ApplicationError::SecretValue(child_path));
                }
                validate_secret_free_payload(child, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_secret_free_payload(child, &format!("{path}[{index}]"))?;
            }
        }
        Value::String(text) if crate::sensitive::contains_likely_credential(text) => {
            return Err(ApplicationError::SecretValue(path.to_string()));
        }
        _ => {}
    }
    Ok(())
}

/// References issued by the credential broker contain an object id only; no
/// provider, token, or secret material can be encoded in them. New broker
/// schemes must be added deliberately here rather than relying on a suffix in
/// an untrusted artifact payload.
fn is_opaque_credential_reference(value: &str) -> bool {
    static REFERENCE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    REFERENCE
        .get_or_init(|| {
            regex::Regex::new(
                r"^(?:vault://connections/conn_|temps://credentials/cred_)[A-Za-z0-9_-]{1,128}$",
            )
            .ok()
        })
        .as_ref()
        .is_some_and(|reference| reference.is_match(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn application_service() -> ApplicationService {
        ApplicationService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ))
    }

    #[tokio::test]
    async fn create_rejects_duplicate_and_excessive_project_ids_before_database_access() {
        let service = application_service();
        assert!(matches!(
            service.create(1, "Workspace", None, &[7, 7]).await,
            Err(ApplicationError::InvalidProjects)
        ));
        let too_many = (1..=(MAX_PROJECTS as i32 + 1)).collect::<Vec<_>>();
        assert!(matches!(
            service.create(1, "Workspace", None, &too_many).await,
            Err(ApplicationError::InvalidProjects)
        ));
    }

    #[tokio::test]
    async fn list_and_get_handle_an_empty_application_store() {
        let list_service = ApplicationService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<ai_applications::Model>::new()])
                .into_connection(),
        ));
        assert!(list_service
            .list(1, 1, 20)
            .await
            .expect("empty list")
            .is_empty());

        let get_service = ApplicationService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<ai_applications::Model>::new()])
                .into_connection(),
        ));
        assert!(matches!(
            get_service.get(1, "app_missing").await,
            Err(ApplicationError::NotFound(id)) if id == "app_missing"
        ));
    }

    #[tokio::test]
    async fn application_and_conversation_lists_apply_lifecycle_and_pagination_in_sql() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_applications::Model>::new()])
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .into_connection(),
        );
        let service = ApplicationService::new(db.clone());

        assert!(service
            .list_with_status(7, 3, 5, "archived")
            .await
            .expect("archived applications")
            .is_empty());
        assert!(service
            .conversations_with_status(11, 7, "archived", 2, 10)
            .await
            .expect("archived conversations")
            .is_empty());

        drop(service);
        let transaction_log = Arc::try_unwrap(db)
            .expect("release mock database")
            .into_transaction_log();
        let statements = transaction_log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.clone())
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("status"));
        assert!(statements[0].contains("LIMIT") && statements[0].contains("OFFSET"));
        assert!(statements[1].contains("status"));
        assert!(statements[1].contains("LIMIT") && statements[1].contains("OFFSET"));
    }

    #[tokio::test]
    async fn archive_and_restore_preserve_the_application_workspace() {
        let now = Utc::now();
        let active_application = ai_applications::Model {
            id: 11,
            public_id: "app_lifecycle".to_string(),
            name: "Lifecycle".to_string(),
            description: None,
            status: "active".to_string(),
            created_by: 7,
            created_at: now,
            updated_at: now,
        };
        let mut archived_application = active_application.clone();
        archived_application.status = "archived".to_string();
        let workspace = ai_application_workspaces::Model {
            id: 17,
            application_id: 11,
            sandbox_public_id: Some("sbx_lifecycle".to_string()),
            desired_state: "running".to_string(),
            runtime: "full".to_string(),
            image: None,
            cpu_limit: 1.0,
            memory_limit_mb: 1024,
            pids_limit: 256,
            disk_limit_mb: 4096,
            idle_timeout_secs: 900,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        let mut paused_workspace = workspace.clone();
        paused_workspace.desired_state = "paused".to_string();

        let archive_db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![active_application.clone()]])
                .append_query_results([Vec::<ai_application_projects::Model>::new()])
                .append_query_results([vec![archived_application.clone()]])
                .append_query_results([vec![workspace.clone()]])
                .append_query_results([vec![paused_workspace.clone()]])
                .into_connection(),
        );
        let archive_service = ApplicationService::new(archive_db.clone());
        assert!(archive_service
            .archive(7, "app_lifecycle")
            .await
            .expect("archive application")
            .is_empty());
        drop(archive_service);
        let archive_sql = Arc::try_unwrap(archive_db)
            .expect("release archive database")
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(archive_sql.contains("UPDATE \"ai_applications\""));
        assert!(archive_sql.contains("UPDATE \"ai_application_workspaces\""));
        assert!(archive_sql.contains("\"status\" ="));
        assert!(archive_sql.contains("\"desired_state\" ="));
        assert!(!archive_sql.contains("DELETE"));

        let restore_db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![archived_application]])
                .append_query_results([Vec::<ai_application_projects::Model>::new()])
                .append_query_results([vec![active_application.clone()]])
                .append_query_results([vec![paused_workspace]])
                .append_query_results([vec![workspace]])
                .append_query_results([vec![active_application]])
                .append_query_results([Vec::<ai_application_projects::Model>::new()])
                .into_connection(),
        );
        let restored = ApplicationService::new(restore_db)
            .restore(7, "app_lifecycle")
            .await
            .expect("restore application");
        assert_eq!(restored.application.status, "active");
        assert!(restored.projects.is_empty());
    }

    #[tokio::test]
    async fn create_artifact_rejects_unknown_kinds_and_secret_payloads_before_database_access() {
        let service = application_service();
        assert!(matches!(
            service
                .create_artifact(1, "conv_1", 1, "html", None, serde_json::json!({}))
                .await,
            Err(ApplicationError::InvalidArtifactKind(kind)) if kind == "html"
        ));
        assert!(matches!(
            service
                .create_artifact(
                    1,
                    "conv_1",
                    1,
                    "form",
                    None,
                    serde_json::json!({"api_key": "sk-live-secret"}),
                )
                .await,
            Err(ApplicationError::SecretValue(path)) if path == "$.api_key"
        ));
    }

    #[tokio::test]
    async fn managed_workspace_is_created_beneath_the_instance_data_dir() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());

        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");

        assert_eq!(workspace.sandbox_label, "app_safe_123");
        assert_eq!(
            workspace.host_work_dir,
            data_dir.path().join("ai-applications").join("app_safe_123")
        );
        assert!(workspace.host_work_dir.join("projects").is_dir());
    }

    #[tokio::test]
    async fn staged_project_removal_can_be_restored_or_finalized() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        tokio::fs::create_dir(workspace.host_work_dir.join("projects/web"))
            .await
            .expect("project directory");
        let source = workspace.host_work_dir.join("projects/web/index.ts");
        tokio::fs::write(&source, b"export {};")
            .await
            .expect("source fixture");

        let staged = service
            .stage_project_removal("app_safe_123", "web")
            .await
            .expect("stage project")
            .expect("project exists");
        assert!(
            !source.exists(),
            "staged source must leave the mounted tree"
        );
        service
            .restore_staged_project(&staged)
            .await
            .expect("restore project");
        assert_eq!(
            tokio::fs::read(&source).await.expect("restored source"),
            b"export {};"
        );

        let staged = service
            .stage_project_removal("app_safe_123", "web")
            .await
            .expect("stage project again")
            .expect("project exists again");
        let staged_path = staged.staged_path.clone();
        service
            .finalize_staged_project(staged)
            .await
            .expect("finalize project removal");
        assert!(!source.exists());
        assert!(!staged_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staged_project_removal_rejects_symlink_source() {
        use std::os::unix::fs::symlink;

        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let outside = data_dir.path().join("outside");
        tokio::fs::create_dir(&outside)
            .await
            .expect("outside directory");
        symlink(&outside, workspace.host_work_dir.join("projects/web")).expect("project symlink");

        let error = service
            .stage_project_removal("app_safe_123", "web")
            .await
            .expect_err("symlink source must be rejected");
        assert!(matches!(error, ApplicationError::Workspace { .. }));
        assert!(outside.exists());
    }

    #[tokio::test]
    async fn managed_workspace_rejects_path_traversal_identifiers() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());

        let error = service
            .ensure("../outside", &[])
            .await
            .expect_err("path traversal must be rejected");

        assert!(matches!(
            error,
            ApplicationError::InvalidWorkspaceIdentifier(value) if value == "../outside"
        ));
        assert!(
            !service.root().exists(),
            "invalid identifiers must be rejected before any workspace path is created"
        );
    }

    #[tokio::test]
    async fn chat_attachment_is_written_inside_the_persistent_workspace() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");

        let path = service
            .store_chat_attachment(
                "app_safe_123",
                "conv_safe_123",
                "att_safe_123",
                "wireframe.png",
                b"image bytes".to_vec(),
            )
            .await
            .expect("attachment stored");

        assert_eq!(
            path,
            data_dir
                .path()
                .join("ai-applications/app_safe_123/.temps/chat-attachments/conv_safe_123/att_safe_123/wireframe.png")
        );
        assert_eq!(
            service
                .chat_attachment_size(
                    "app_safe_123",
                    "conv_safe_123",
                    "att_safe_123",
                    "wireframe.png",
                )
                .await
                .expect("attachment size"),
            11
        );
        assert_eq!(
            service
                .read_chat_attachment(
                    "app_safe_123",
                    "conv_safe_123",
                    "att_safe_123",
                    "wireframe.png",
                    20,
                )
                .await
                .expect("attachment content"),
            b"image bytes"
        );
        assert!(matches!(
            service
                .read_chat_attachment(
                    "app_safe_123",
                    "conv_safe_123",
                    "att_safe_123",
                    "wireframe.png",
                    5,
                )
                .await
                .expect_err("bounded reads must reject oversized files"),
            ApplicationError::InvalidAttachment(_)
        ));
    }

    #[tokio::test]
    async fn chat_attachment_enforces_workspace_file_quota() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let existing = workspace
            .host_work_dir
            .join(".temps/chat-attachments/conv_existing/att_existing");
        tokio::fs::create_dir_all(&existing)
            .await
            .expect("attachment directory");
        for index in 0..MAX_CHAT_ATTACHMENT_FILES_PER_WORKSPACE {
            tokio::fs::write(existing.join(format!("{index}.txt")), b"x")
                .await
                .expect("quota fixture");
        }

        let error = service
            .store_chat_attachment(
                "app_safe_123",
                "conv_safe_123",
                "att_safe_123",
                "notes.txt",
                b"blocked".to_vec(),
            )
            .await
            .expect_err("attachment quota must reject another file");

        assert!(matches!(
            error,
            ApplicationError::InvalidAttachment(message)
                if message.contains("attachment quota exceeded")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chat_attachment_rejects_a_sandbox_created_symlink() {
        use std::os::unix::fs::symlink;

        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let outside = data_dir.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.expect("outside");
        symlink(&outside, workspace.host_work_dir.join(".temps")).expect("symlink");

        let error = service
            .store_chat_attachment(
                "app_safe_123",
                "conv_safe_123",
                "att_safe_123",
                "notes.txt",
                b"blocked".to_vec(),
            )
            .await
            .expect_err("symlink traversal must fail");

        assert!(matches!(error, ApplicationError::Workspace { .. }));
        assert!(!outside.join("chat-attachments").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_workspace_rejects_sandbox_created_projects_symlink() {
        use std::os::unix::fs::symlink;

        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace_root = service.root().join("app_safe_123");
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .expect("workspace root");
        let outside = data_dir.path().join("outside");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("outside dir");
        symlink(&outside, workspace_root.join("projects")).expect("projects symlink");

        let error = service
            .ensure("app_safe_123", &[])
            .await
            .expect_err("symlink must be rejected");
        assert!(matches!(error, ApplicationError::Workspace { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_file_import_is_descriptor_relative_and_rejects_symlink_parents() {
        use std::os::unix::fs::symlink;

        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let project = workspace.host_work_dir.join("projects/web");
        tokio::fs::create_dir(&project).await.expect("project dir");

        let written = service
            .store_project_files_bounded(
                "app_safe_123",
                "web",
                vec![(PathBuf::from("src/index.ts"), b"export {};".to_vec(), None)],
                1024,
                100,
            )
            .await
            .expect("safe import");
        assert_eq!(written, 1);
        assert_eq!(
            tokio::fs::read(project.join("src/index.ts"))
                .await
                .expect("imported file"),
            b"export {};"
        );

        let outside = data_dir.path().join("outside");
        tokio::fs::create_dir(&outside).await.expect("outside dir");
        symlink(&outside, project.join("escape")).expect("escape symlink");
        let error = service
            .store_project_files_bounded(
                "app_safe_123",
                "web",
                vec![(PathBuf::from("escape/owned.txt"), b"blocked".to_vec(), None)],
                1024,
                100,
            )
            .await
            .expect_err("symlink parent must be rejected");
        assert!(matches!(error, ApplicationError::Workspace { .. }));
        assert!(!outside.join("owned.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_file_import_enforces_aggregate_quota_before_writing() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        tokio::fs::create_dir(workspace.host_work_dir.join("projects/web"))
            .await
            .expect("project dir");

        let error = service
            .store_project_files_bounded(
                "app_safe_123",
                "web",
                vec![(PathBuf::from("large.bin"), vec![0; 9], None)],
                8,
                100,
            )
            .await
            .expect_err("quota must reject oversized aggregate");
        assert!(matches!(error, ApplicationError::WorkspaceQuota(_)));
        assert!(!workspace
            .host_work_dir
            .join("projects/web/large.bin")
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_file_import_counts_bytes_across_all_application_projects() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let projects = workspace.host_work_dir.join("projects");
        tokio::fs::create_dir_all(projects.join("web"))
            .await
            .expect("web project");
        tokio::fs::create_dir_all(projects.join("api"))
            .await
            .expect("api project");
        tokio::fs::write(projects.join("api/existing.bin"), vec![0; 6])
            .await
            .expect("existing application file");

        let error = service
            .store_project_files_bounded(
                "app_safe_123",
                "web",
                vec![(PathBuf::from("new.bin"), vec![0; 3], None)],
                8,
                100,
            )
            .await
            .expect_err("files in another project must consume the application quota");
        assert!(matches!(error, ApplicationError::WorkspaceQuota(_)));
        assert!(!projects.join("web/new.bin").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_zero_byte_imports_enforce_application_entry_quota() {
        let data_dir = tempfile::tempdir().expect("temporary data directory");
        let service = ApplicationWorkspaceService::new(data_dir.path().to_path_buf());
        let workspace = service
            .ensure("app_safe_123", &[])
            .await
            .expect("managed workspace");
        let project = workspace.host_work_dir.join("projects/web");
        tokio::fs::create_dir_all(&project)
            .await
            .expect("web project");

        for name in ["one", "two"] {
            service
                .store_project_files_bounded(
                    "app_safe_123",
                    "web",
                    vec![(PathBuf::from(name), Vec::new(), None)],
                    1024,
                    4,
                )
                .await
                .expect("entry below aggregate quota");
        }
        let error = service
            .store_project_files_bounded(
                "app_safe_123",
                "web",
                vec![(PathBuf::from("three"), Vec::new(), None)],
                1024,
                4,
            )
            .await
            .expect_err("repeated zero-byte files must hit the aggregate entry quota");
        assert!(matches!(error, ApplicationError::WorkspaceQuota(_)));
        assert!(!project.join("three").exists());
    }

    #[test]
    fn artifact_payload_accepts_credential_references() {
        let payload = serde_json::json!({
            "capability": "payments",
            "credential_ref": "vault://connections/conn_123",
            "targets": ["payments-api"]
        });
        assert!(validate_secret_free_payload(&payload, "$").is_ok());
    }

    #[test]
    fn semantic_artifact_kinds_are_supported_without_component_names() {
        for kind in ["resource", "collection", "operation"] {
            assert!(is_allowed_artifact_kind(kind), "{kind} should be supported");
        }
        assert!(!is_allowed_artifact_kind("react_component"));
        assert!(!is_allowed_artifact_kind("html"));
    }

    #[test]
    fn artifact_payload_rejects_nested_secret_values() {
        let payload = serde_json::json!({
            "form": { "fields": [{ "api_key": "sk_test_value" }] }
        });
        assert!(matches!(
            validate_secret_free_payload(&payload, "$"),
            Err(ApplicationError::SecretValue(path)) if path == "$.form.fields[0].api_key"
        ));
    }

    #[test]
    fn artifact_payload_rejects_secret_disguised_under_neutral_key() {
        let payload = serde_json::json!({
            "content": "STRIPE_KEY=sk_test_1234567890123456"
        });
        assert!(matches!(
            validate_secret_free_payload(&payload, "$"),
            Err(ApplicationError::SecretValue(path)) if path == "$.content"
        ));
    }

    #[test]
    fn artifact_payload_rejects_arbitrary_reference_values() {
        let payload = serde_json::json!({
            "credential_ref": "this-is-a-real-secret-that-is-not-a-broker-reference"
        });
        assert!(matches!(
            validate_secret_free_payload(&payload, "$"),
            Err(ApplicationError::SecretValue(path)) if path == "$.credential_ref"
        ));
    }

    #[test]
    fn workspace_settings_accept_supported_resource_bounds() {
        let update = WorkspaceSettingsUpdate {
            runtime: Some("rust".to_string()),
            cpu_limit: Some(4.0),
            memory_limit_mb: Some(8_192),
            pids_limit: Some(1_024),
            disk_limit_mb: Some(32_768),
            idle_timeout_secs: Some(3_600),
            ..Default::default()
        };

        assert!(update.validate().is_ok());
    }

    #[test]
    fn workspace_settings_reject_unsafe_or_unbounded_values() {
        let cases = [
            WorkspaceSettingsUpdate {
                runtime: Some("host".to_string()),
                ..Default::default()
            },
            WorkspaceSettingsUpdate {
                cpu_limit: Some(0.0),
                ..Default::default()
            },
            WorkspaceSettingsUpdate {
                memory_limit_mb: Some(128),
                ..Default::default()
            },
            WorkspaceSettingsUpdate {
                disk_limit_mb: Some(2_000_000),
                ..Default::default()
            },
            WorkspaceSettingsUpdate {
                idle_timeout_secs: Some(30),
                ..Default::default()
            },
        ];

        for update in cases {
            assert!(matches!(
                update.validate(),
                Err(ApplicationError::InvalidWorkspaceSetting(_))
            ));
        }
    }

    #[test]
    fn aggregate_workspace_quota_rejects_host_exhaustion() {
        let at_limit = WorkspaceQuotaValues {
            cpu: MAX_USER_WORKSPACE_CPU,
            memory_mb: MAX_USER_WORKSPACE_MEMORY_MB,
            pids: MAX_USER_WORKSPACE_PIDS,
            disk_mb: MAX_USER_WORKSPACE_DISK_MB,
        };
        assert!(at_limit.validate_aggregate().is_ok());
        assert!(matches!(
            WorkspaceQuotaValues {
                cpu: MAX_USER_WORKSPACE_CPU + 0.25,
                ..at_limit
            }
            .validate_aggregate(),
            Err(ApplicationError::WorkspaceQuota(_))
        ));
    }

    #[test]
    fn global_workspace_reservation_is_counted_in_every_resource_dimension() {
        let mut aggregate = WorkspaceQuotaValues::default_empty();
        aggregate.add(WorkspaceQuotaValues::default());

        assert_eq!(aggregate.cpu, 4.0);
        assert_eq!(aggregate.memory_mb, 8_192);
        assert_eq!(aggregate.pids, 512);
        assert_eq!(aggregate.disk_mb, 10_240);
    }
}
