// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deploy the files in a chat's sandbox as a project's source, with no Git.
//!
//! A chat that builds an app in its sandbox already has the files. Publishing
//! them should not need a repository: the server packs the sandbox directory,
//! leaves out what must never be uploaded (`node_modules`, `.git`, `.env`
//! files, credentials, build output), and hands the archive to the same
//! uploaded-source pipeline that `temps deploy drop` and the application
//! workspaces use.
//!
//! The packing is done in two steps so the boundary does not depend on the
//! sandbox: the sandbox only runs `tar`, which is a size optimisation, and the
//! server re-reads the tarball entry by entry. Every path is checked, links and
//! special files are refused, and the server decides what is excluded. Whatever
//! the model or a command wrote into the tarball, the archive that is deployed
//! is the one this module builds.
//!
//! Nothing the sandbox reports is trusted for the size either. The `wc -c`
//! that follows `tar` only lets an obviously oversized tarball be refused
//! before it is read; the limit itself ([`MAX_PACKED_BYTES`]) is enforced by
//! the read, which stops and fails as soon as the file grows past it, so a
//! shimmed `wc` or a process still appending to the tarball cannot make the
//! server buffer more.
//!
//! The deploy is a write to the platform, so the chat runtime asks the user to
//! approve it (or approves it automatically in the modes that do) before
//! [`SandboxLinkService::deploy_prepared`] runs; the approver's own permissions
//! are checked again at that point.

use std::io::{Read, Write};
use std::path::{Component, Path};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use temps_ai::ChatTool;
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_core::{AuditContext, SourceDropRequest};
use thiserror::Error;
use tracing::{info, warn};

use crate::sandbox_link::{resolve_path, SandboxLinkService, SANDBOX_EXEC_TOOL};

pub const SANDBOX_DEPLOY_TOOL: &str = "temps_deploy_sandbox_source";

/// Largest tarball taken out of a sandbox, enforced by a bounded read of the
/// staged file (not by the size the sandbox reports). The tarball is held in
/// memory while it is unpacked, so this bounds the control plane's memory for
/// one deploy (the provider's transfer framing comes on top while it reads).
const MAX_PACKED_BYTES: u64 = 200 * 1024 * 1024;
const MAX_SOURCE_FILES: usize = 100_000;
const MAX_SOURCE_BYTES: u64 = 500 * 1024 * 1024;
const PACK_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Deserialize)]
pub struct DeployArgs {
    pub project_id: i32,
    #[serde(default)]
    pub environment_id: Option<i32>,
    /// Directory in the sandbox that holds the project. Defaults to the
    /// sandbox work directory.
    #[serde(default)]
    pub directory: Option<String>,
}

#[derive(Error, Debug)]
pub enum SandboxDeployError {
    #[error(
        "`{SANDBOX_DEPLOY_TOOL}` needs {{\"project_id\": <number>}} and optionally \
         \"environment_id\" and \"directory\": {reason}. Nothing was deployed."
    )]
    Arguments { reason: String },

    #[error("Could not pack {directory} in sandbox {sandbox_id}: {reason}")]
    Pack {
        sandbox_id: String,
        directory: String,
        reason: String,
    },

    #[error(
        "{directory} in sandbox {sandbox_id} packs to {bytes} bytes, over the {max} byte limit. \
         Remove generated files (caches, build output, datasets) and try again"
    )]
    PackedTooLarge {
        sandbox_id: String,
        directory: String,
        bytes: u64,
        max: u64,
    },

    #[error(
        "{directory} in sandbox {sandbox_id} packs to more than the {max} byte limit. \
         Remove generated files (caches, build output, datasets) and try again"
    )]
    PackedOverLimit {
        sandbox_id: String,
        directory: String,
        max: u64,
    },

    #[error("The source archive of sandbox {sandbox_id} has more than {max} files")]
    TooManyFiles { sandbox_id: String, max: usize },

    #[error("The source of sandbox {sandbox_id} is larger than {max} bytes uncompressed")]
    SourceTooLarge { sandbox_id: String, max: u64 },

    #[error(
        "Sandbox {sandbox_id} holds `{path}`, which cannot be deployed: {reason}. Remove or \
         replace it and try again"
    )]
    UnsafeEntry {
        sandbox_id: String,
        path: String,
        reason: String,
    },

    #[error(
        "{directory} in sandbox {sandbox_id} has no deployable files (node_modules, .git, \
         .env files, credentials and folders named dist, build, .next, target or coverage are \
         never uploaded, at any depth)"
    )]
    Empty {
        sandbox_id: String,
        directory: String,
    },

    #[error("Could not build the source archive of sandbox {sandbox_id}: {reason}")]
    Archive { sandbox_id: String, reason: String },
}

pub fn deploy_tool() -> ChatTool {
    ChatTool {
        name: SANDBOX_DEPLOY_TOOL.to_string(),
        description: "Deploy the files in the linked sandbox as the source of a Temps project, \
            with no Git repository: Temps builds them with the project's preset and serves the \
            result. Use it to publish an app you built in the sandbox. The server packs the \
            directory itself and leaves out node_modules, .git, .env files, credentials and every \
            folder named dist, build, .next, target or coverage at any depth (so a source folder \
            such as `src/build/` must be renamed to be deployed), so do not zip or upload \
            anything yourself and do not commit the code just to deploy it. The user is asked to \
            approve the deployment. The project must deploy from uploaded source (create it with \
            `source_type` `uploaded_source` so Temps builds it) or have alternate sources \
            enabled; a `static_files` project takes plain HTML, CSS and JS, and to deploy a \
            site already built in the sandbox, point `directory` at the output folder itself \
            (for example `web/dist`). \
            Returns the deployment. The chat shows its build stages live to the user, so never \
            poll it or sleep waiting for it."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "project_id": { "type": "integer", "description": "The Temps project to deploy to." },
                "environment_id": { "type": "integer", "description": "Target environment. Defaults to the project's production environment." },
                "directory": { "type": "string", "description": "Directory in the sandbox that holds the project (the one with package.json, index.html, a Dockerfile, …). Defaults to the sandbox work directory, where a single project lives; set it only when the sandbox holds several projects in their own folders." }
            },
            "required": ["project_id"]
        }),
    }
}

/// What the model is told once the deployment has started. The console shows
/// the deployment's stages live from this result, so the model is told not to
/// poll or sleep, and is handed the exact goal that watches it: guessing one
/// tends to miss `project_id` or point at a field that does not exist.
fn started_result(
    deployment_id: i32,
    project_id: i32,
    environment_id: i32,
    state: &str,
    files: usize,
    left_out: usize,
) -> String {
    json!({
        "status": "started",
        "deployment_id": deployment_id,
        "project_id": project_id,
        "environment_id": environment_id,
        "state": state,
        "files": files,
        "left_out": left_out,
        "goal": {
            "title": format!("Deployment {deployment_id} goes live"),
            "operation_id": "get_deployment",
            "params": { "project_id": project_id, "deployment_id": deployment_id },
            "success_path": "status",
            "success_values": ["completed"],
            "failure_values": ["failed", "cancelled"],
        },
        "instruction": "Temps is building the project from those files, and the user already \
            sees its stages live in the chat. Do not check on it, poll it or sleep waiting for \
            it. If you have `temps_goal`, call it with exactly the `goal` above so you are \
            brought back if the deployment fails, and once it is live to check the running app. \
            Then tell the user in a sentence that it is building and end your reply.",
    })
    .to_string()
}

/// Files packed from a sandbox, as the ZIP the uploaded-source pipeline takes.
pub struct PackedSource {
    pub archive: tempfile::NamedTempFile,
    pub files: usize,
    pub source_bytes: u64,
    /// Entries left out because they must not be uploaded.
    pub left_out: usize,
}

/// A deployment the chat runtime may now ask the user to approve.
pub struct PreparedDeploy {
    pub packed: PackedSource,
    pub args: DeployArgs,
    pub sandbox_id: String,
    pub directory: String,
}

impl PreparedDeploy {
    /// The sentence on the approval card.
    pub fn summary(&self) -> String {
        let target = match self.args.environment_id {
            Some(environment_id) => format!(" (environment {environment_id})"),
            None => String::new(),
        };
        format!(
            "Deploy {} files ({}) from the chat's sandbox to project {}{target}, without Git. \
             node_modules, .git, .env files, credentials and build folders (dist, build, .next, \
             target, coverage) are left out.",
            self.packed.files,
            human_bytes(self.packed.source_bytes),
            self.args.project_id
        )
    }
}

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KiB", bytes.div_ceil(KIB).max(1))
    }
}

/// Single-quote `value` for `sh -c`.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The command that tars `directory` into `output` and prints the size of the
/// tarball. Excluding `node_modules` and `.git` here only keeps the transfer
/// small; the server filters every entry again.
///
/// GNU tar exits 1, not 0, when a file changes while it is read. The sandbox's
/// work directory itself is touched constantly (a dev server, editors, the
/// preview), so packing a project kept flat at its top failed every time. The
/// archive is still complete then, and is fully parsed on the server, so 1 is
/// accepted when the archive was written; real errors are 2.
fn pack_command(directory: &str, output: &str) -> String {
    let output = shell_quote(output);
    format!(
        "tar -czf {output} -C {} --exclude=node_modules --exclude=.git .; status=$?; \
         if [ \"$status\" -gt 1 ] || [ ! -s {output} ]; then exit \"$status\"; fi; \
         wc -c < {output}",
        shell_quote(directory)
    )
}

fn normalized_entry_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn is_left_out(path: &str) -> bool {
    path.split('/')
        .any(|part| part.eq_ignore_ascii_case("node_modules"))
        || crate::applications::is_sensitive_workspace_path(path)
}

/// Rebuild a gzip-compressed tarball as the ZIP the uploaded-source pipeline
/// takes, keeping only files that may be uploaded.
///
/// Nothing the tarball says is trusted: paths are rebuilt from normal
/// components only, links and special files are refused, and file counts and
/// sizes are bounded.
pub fn zip_from_tar_gz(
    sandbox_id: &str,
    directory: &str,
    tarball: &[u8],
) -> Result<PackedSource, SandboxDeployError> {
    let archive_error = |reason: String| SandboxDeployError::Archive {
        sandbox_id: sandbox_id.to_string(),
        reason,
    };
    let output = tempfile::NamedTempFile::new().map_err(|e| archive_error(e.to_string()))?;
    let mut writer = zip::ZipWriter::new(
        output
            .reopen()
            .map_err(|e| archive_error(format!("could not open the staging file: {e}")))?,
    );
    let mut files = 0_usize;
    let mut left_out = 0_usize;
    let mut source_bytes = 0_u64;

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    let entries = archive
        .entries()
        .map_err(|e| archive_error(format!("the tarball is not readable: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| archive_error(format!("bad tarball entry: {e}")))?;
        let raw_path = entry
            .path()
            .map_err(|e| archive_error(format!("bad tarball path: {e}")))?
            .into_owned();
        let kind = entry.header().entry_type();
        // Directories carry no content; files recreate their parents.
        if kind.is_dir() {
            continue;
        }
        let Some(path) = normalized_entry_path(&raw_path) else {
            return Err(SandboxDeployError::UnsafeEntry {
                sandbox_id: sandbox_id.to_string(),
                path: raw_path.display().to_string(),
                reason: "its path leaves the project directory".to_string(),
            });
        };
        if is_left_out(&path) {
            left_out += 1;
            continue;
        }
        if !(kind.is_file() || kind.is_contiguous()) {
            return Err(SandboxDeployError::UnsafeEntry {
                sandbox_id: sandbox_id.to_string(),
                path,
                reason: "it is a link or a special file, and uploads do not follow links"
                    .to_string(),
            });
        }
        files += 1;
        if files > MAX_SOURCE_FILES {
            return Err(SandboxDeployError::TooManyFiles {
                sandbox_id: sandbox_id.to_string(),
                max: MAX_SOURCE_FILES,
            });
        }
        let executable = entry
            .header()
            .mode()
            .map(|m| m & 0o111 != 0)
            .unwrap_or(false);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(if executable { 0o755 } else { 0o644 });
        writer
            .start_file(path.clone(), options)
            .map_err(|e| archive_error(format!("could not add {path}: {e}")))?;
        let remaining = MAX_SOURCE_BYTES.saturating_sub(source_bytes);
        let copied = std::io::copy(&mut (&mut entry).take(remaining + 1), &mut writer)
            .map_err(|e| archive_error(format!("could not copy {path}: {e}")))?;
        if copied > remaining {
            return Err(SandboxDeployError::SourceTooLarge {
                sandbox_id: sandbox_id.to_string(),
                max: MAX_SOURCE_BYTES,
            });
        }
        source_bytes += copied;
    }
    if files == 0 {
        return Err(SandboxDeployError::Empty {
            sandbox_id: sandbox_id.to_string(),
            directory: directory.to_string(),
        });
    }
    writer
        .finish()
        .map_err(|e| archive_error(format!("could not finish the archive: {e}")))?
        .flush()
        .map_err(|e| archive_error(format!("could not flush the archive: {e}")))?;
    Ok(PackedSource {
        archive: output,
        files,
        source_bytes,
        left_out,
    })
}

/// Permissions a sandbox deploy needs, both instance-wide and within the
/// target project: it starts a deployment, and it may switch a manual project
/// to an uploaded-source one (`promote_manual_source`), which edits the
/// project. A project viewer holds neither within the project.
const DEPLOY_PERMISSIONS: [Permission; 2] =
    [Permission::DeploymentsCreate, Permission::ProjectsWrite];

/// Whether `auth` may deploy to `project_id` right now: the instance
/// permissions, and the same project-scoped permissions the uploaded-source
/// HTTP handler checks (`project_permission_guard!` on `DeploymentsCreate`),
/// plus `ProjectsWrite` for the source promotion. The checker is asked on
/// every call, so this is checked when the tool is called and again, with the
/// roles held at that moment, for the user who approves, right before the
/// deploy runs.
pub async fn authorize_deploy(
    auth: &AuthContext,
    project_id: i32,
    chat_project_id: Option<i32>,
    checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<(), String> {
    for permission in DEPLOY_PERMISSIONS {
        if !auth.has_permission(&permission) {
            return Err(format!(
                "`{SANDBOX_DEPLOY_TOOL}` is not available: deploying needs the {permission} \
                 permission."
            ));
        }
    }
    if let Some(chat_project_id) = chat_project_id {
        if chat_project_id != project_id {
            return Err(format!(
                "This chat belongs to project {chat_project_id}; it can deploy only to that \
                 project, not to project {project_id}."
            ));
        }
    }
    // A deployment token is confined to its own project and carries no user
    // for the team checks below (as with `project_scope_guard!`).
    if !auth.is_scoped_to_project(project_id) {
        return Err(format!(
            "This credential is scoped to a different project and cannot deploy to project \
             {project_id}. Nothing was deployed."
        ));
    }
    if auth.is_deployment_token() {
        return Ok(());
    }
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return Ok(());
    }
    let Some(checker) = checker else {
        // No team layer on this server: the instance permission decides.
        return Ok(());
    };
    let Some(user_id) = auth.user_id_opt() else {
        return Err(format!(
            "`{SANDBOX_DEPLOY_TOOL}` is not available: this credential is not tied to a user."
        ));
    };
    match checker
        .effective_project_permissions(user_id, project_id)
        .await
    {
        Ok(Some(held)) => {
            let missing: Vec<String> = DEPLOY_PERMISSIONS
                .iter()
                .map(|permission| permission.to_string())
                .filter(|required| !held.iter().any(|p| p == required))
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "Your role on project {project_id} does not include {}, so nothing was \
                     deployed to it.",
                    missing.join(" and ")
                ))
            }
        }
        // No per-permission opinion: fall back to plain project access, as
        // `project_permission_guard!` does.
        Ok(None) => match checker.user_can_access_project(user_id, project_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "You do not have access to project {project_id}, so nothing was deployed to it."
            )),
            Err(error) => {
                warn!(project_id, user_id, "project access check failed: {error}");
                Err(format!(
                    "Access to project {project_id} could not be verified, so nothing was \
                     deployed."
                ))
            }
        },
        Err(error) => {
            warn!(
                project_id,
                user_id, "project permission check failed: {error}"
            );
            Err(format!(
                "Your permissions on project {project_id} could not be verified, so nothing was \
                 deployed."
            ))
        }
    }
}

/// The staged tarball as the bounded read returned it, or why it cannot be
/// used. `read` was made with a [`MAX_PACKED_BYTES`] bound, which the sandbox
/// cannot influence: a file over it fails that read as a validation error,
/// whatever `reported_bytes` (the sandbox's own `wc -c`) claimed.
fn bounded_tarball(
    sandbox_id: &str,
    directory: &str,
    reported_bytes: u64,
    read: Result<Vec<u8>, temps_sandbox::error::SandboxError>,
) -> Result<Vec<u8>, SandboxDeployError> {
    match read {
        // Defence in depth: a provider must refuse an oversized file, but the
        // size actually held is what decides.
        Ok(tarball) if tarball.len() as u64 > MAX_PACKED_BYTES => {
            Err(SandboxDeployError::PackedTooLarge {
                sandbox_id: sandbox_id.to_string(),
                directory: directory.to_string(),
                bytes: tarball.len() as u64,
                max: MAX_PACKED_BYTES,
            })
        }
        Ok(tarball) => Ok(tarball),
        Err(temps_sandbox::error::SandboxError::Validation { message }) => {
            warn!(
                sandbox_id,
                directory,
                reported_bytes,
                "the staged tarball was refused by the bounded read: {message}"
            );
            Err(SandboxDeployError::PackedOverLimit {
                sandbox_id: sandbox_id.to_string(),
                directory: directory.to_string(),
                max: MAX_PACKED_BYTES,
            })
        }
        Err(error) => Err(SandboxDeployError::Pack {
            sandbox_id: sandbox_id.to_string(),
            directory: directory.to_string(),
            reason: format!(
                "could not read the tarball (reported as {reported_bytes} bytes) back from the \
                 sandbox: {error}"
            ),
        }),
    }
}

impl SandboxLinkService {
    /// Pack the linked sandbox's project directory for a deployment. Nothing is
    /// deployed and nothing outside the sandbox changes.
    pub async fn prepare_deploy(
        &self,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> Result<PreparedDeploy, String> {
        let args: DeployArgs = serde_json::from_str(arguments).map_err(|error| {
            SandboxDeployError::Arguments {
                reason: error.to_string(),
            }
            .to_string()
        })?;
        let user_id = auth.user_id();
        if !auth.has_permission(&Permission::SandboxesExec) {
            return Err(format!(
                "`{SANDBOX_DEPLOY_TOOL}` is not available: reading a sandbox's files needs the {} permission.",
                Permission::SandboxesExec
            ));
        }
        let ready = self
            .ready_sandbox(SANDBOX_DEPLOY_TOOL, conversation_id, user_id)
            .await?;
        let sandbox_id = ready.link.sandbox_id.clone();
        let directory = resolve_path(&ready.work_dir, args.directory.as_deref().unwrap_or("."));
        let directory = directory.trim_end_matches('/').to_string();
        let pack_error = |reason: String| {
            SandboxDeployError::Pack {
                sandbox_id: sandbox_id.clone(),
                directory: directory.clone(),
                reason,
            }
            .to_string()
        };

        let staging = format!("/tmp/temps-deploy-{}.tgz", uuid::Uuid::new_v4().simple());
        let options = temps_sandbox::services::ExecOptions {
            cmd: vec![
                "sh".to_string(),
                "-c".to_string(),
                pack_command(&directory, &staging),
            ],
            env: Default::default(),
            cwd: Some(ready.work_dir.clone()),
        };
        let packed_size = tokio::time::timeout(
            std::time::Duration::from_secs(PACK_TIMEOUT_SECS),
            self.sandboxes.exec(&sandbox_id, user_id, options),
        )
        .await;
        let outcome = match packed_size {
            Err(_) => Err(pack_error(format!(
                "`tar` did not finish within {PACK_TIMEOUT_SECS} seconds"
            ))),
            Ok(Err(error)) => Err(self
                .tool_failure(
                    &format!("Could not pack {directory}"),
                    error,
                    conversation_id,
                    &ready.link,
                )
                .await),
            Ok(Ok(result)) if result.exit_code != 0 => Err(pack_error(format!(
                "`tar` exited with {}: {}. Check the directory exists (look with `{SANDBOX_EXEC_TOOL}`)",
                result.exit_code,
                result.stderr.trim()
            ))),
            Ok(Ok(result)) => result.stdout.trim().parse::<u64>().map_err(|_| {
                pack_error(format!(
                    "could not read the tarball size from `{}`",
                    result.stdout.trim()
                ))
            }),
        };
        // `wc -c` ran in the sandbox, so its answer is only advisory: it saves
        // reading a tarball that is plainly too big. The bounded read below is
        // what enforces the limit.
        let bytes = match outcome {
            Ok(bytes) if bytes > MAX_PACKED_BYTES => {
                self.remove_staging(&sandbox_id, user_id, &staging).await;
                return Err(SandboxDeployError::PackedTooLarge {
                    sandbox_id,
                    directory,
                    bytes,
                    max: MAX_PACKED_BYTES,
                }
                .to_string());
            }
            Ok(bytes) => bytes,
            Err(message) => {
                self.remove_staging(&sandbox_id, user_id, &staging).await;
                return Err(message);
            }
        };

        let tarball = self
            .sandboxes
            .fs_read_bounded(&sandbox_id, user_id, &staging, MAX_PACKED_BYTES)
            .await;
        self.remove_staging(&sandbox_id, user_id, &staging).await;
        let tarball = bounded_tarball(&sandbox_id, &directory, bytes, tarball)
            .map_err(|error| error.to_string())?;

        let (sandbox_for_task, directory_for_task) = (sandbox_id.clone(), directory.clone());
        let packed = tokio::task::spawn_blocking(move || {
            zip_from_tar_gz(&sandbox_for_task, &directory_for_task, &tarball)
        })
        .await
        .map_err(|error| pack_error(format!("the archive task failed: {error}")))?
        .map_err(|error| error.to_string())?;
        info!(
            sandbox_id = %sandbox_id,
            project_id = args.project_id,
            files = packed.files,
            left_out = packed.left_out,
            source_bytes = packed.source_bytes,
            "packed a chat sandbox's files for deployment"
        );
        Ok(PreparedDeploy {
            packed,
            args,
            sandbox_id,
            directory,
        })
    }

    async fn remove_staging(&self, sandbox_id: &str, user_id: i32, path: &str) {
        let options = temps_sandbox::services::ExecOptions {
            cmd: vec!["rm".to_string(), "-f".to_string(), path.to_string()],
            env: Default::default(),
            cwd: None,
        };
        if let Err(error) = self.sandboxes.exec(sandbox_id, user_id, options).await {
            warn!(
                sandbox_id,
                path, "could not remove the staged tarball: {error}"
            );
        }
    }

    /// Hand a prepared, approved archive to the uploaded-source pipeline and
    /// say what happened in a sentence the model can act on.
    pub async fn deploy_prepared(
        &self,
        prepared: PreparedDeploy,
        approver: &AuthContext,
        conversation_id: i64,
        chat_project_id: Option<i32>,
    ) -> String {
        let Some(deployer) = self.source_drop.as_ref() else {
            return "Deploying from a sandbox is not available: this server has no deployments \
                    plugin. Nothing was deployed."
                .to_string();
        };
        let project_id = prepared.args.project_id;
        let deployed = deployer
            .deploy_source_drop(SourceDropRequest {
                project_id,
                environment_id: prepared.args.environment_id,
                archive_path: prepared.packed.archive.path().to_path_buf(),
                original_filename: format!("sandbox-{}.zip", prepared.sandbox_id),
                promote_manual_source: true,
            })
            .await;
        match deployed {
            Ok(deployment) => {
                self.audit_deployed(
                    &prepared,
                    approver,
                    conversation_id,
                    chat_project_id,
                    deployment.id,
                    deployment.environment_id,
                )
                .await;
                let operation = crate::chat_operations::NewChatOperation {
                    kind: crate::chat_operations::ChatOperationKind::Deployment,
                    operation_id: SANDBOX_DEPLOY_TOOL.to_string(),
                    target_id: Some(deployment.id),
                    project_id: Some(deployment.project_id),
                    environment_id: Some(deployment.environment_id),
                    service_id: None,
                };
                if let Err(error) =
                    crate::chat_operations::record(self.db.as_ref(), conversation_id, operation)
                        .await
                {
                    warn!(deployment_id = deployment.id, "{error}");
                }
                started_result(
                    deployment.id,
                    deployment.project_id,
                    deployment.environment_id,
                    &deployment.state,
                    prepared.packed.files,
                    prepared.packed.left_out,
                )
            }
            Err(error) => {
                warn!(project_id, sandbox_id = %prepared.sandbox_id, "sandbox source deploy failed: {error}");
                let hint = match &error {
                    temps_core::SourceDropError::SourceNotAllowed { .. } => {
                        " Create the project with source_type `uploaded_source` (or \
                         `static_files`), or enable alternate sources on it."
                    }
                    _ => "",
                };
                format!("Nothing was deployed: {error}.{hint}")
            }
        }
    }

    async fn audit_deployed(
        &self,
        prepared: &PreparedDeploy,
        approver: &AuthContext,
        conversation_id: i64,
        chat_project_id: Option<i32>,
        deployment_id: i32,
        environment_id: i32,
    ) {
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let entry = crate::audit::AiSandboxSourceDeployedAudit {
            context: AuditContext {
                user_id: approver.user_id(),
                ip_address: None,
                user_agent: "temps-ai-assistant".to_string(),
            },
            project_id: prepared.args.project_id,
            chat_project_id,
            conversation_id: conversation_id.to_string(),
            sandbox_id: prepared.sandbox_id.clone(),
            environment_id,
            deployment_id,
            files: prepared.packed.files,
            source_bytes: prepared.packed.source_bytes,
        };
        if let Err(error) = audit.create_audit_log(&entry).await {
            warn!(
                deployment_id,
                "failed to audit a sandbox source deployment: {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_started_deployment_hands_the_model_the_goal_that_watches_it() {
        let result: serde_json::Value =
            serde_json::from_str(&started_result(41, 39, 12, "pending", 48, 3)).expect("json");

        assert_eq!(result["status"], "started");
        assert_eq!(result["deployment_id"], 41);
        assert_eq!(result["project_id"], 39);
        let goal = &result["goal"];
        assert_eq!(goal["operation_id"], "get_deployment");
        assert_eq!(
            goal["params"],
            json!({"project_id": 39, "deployment_id": 41})
        );
        assert_eq!(goal["success_path"], "status");
        assert_eq!(goal["success_values"], json!(["completed"]));
        assert_eq!(goal["failure_values"], json!(["failed", "cancelled"]));
        let instruction = result["instruction"].as_str().expect("instruction");
        assert!(instruction.contains("Do not check on it, poll it or sleep"));
    }

    fn tarball(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        let bytes = builder.into_inner().expect("tar finishes");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&bytes).expect("gzip writes");
        encoder.finish().expect("gzip finishes")
    }

    fn add_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, body: &[u8], mode: u32) {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, path, body)
            .expect("file appended");
    }

    fn names(packed: &PackedSource) -> Vec<String> {
        let file = std::fs::File::open(packed.archive.path()).expect("archive opens");
        let mut zip = zip::ZipArchive::new(file).expect("valid zip");
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| {
                zip.by_index(i)
                    .expect("entry")
                    .name()
                    .expect("utf-8 entry name")
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn source_files_are_packed_and_what_must_not_be_uploaded_is_left_out() {
        let data = tarball(|b| {
            add_file(b, "./package.json", b"{}", 0o644);
            add_file(b, "./src/index.js", b"console.log(1)", 0o644);
            add_file(b, "./start.sh", b"#!/bin/sh", 0o755);
            add_file(b, "./.env", b"SECRET=1", 0o600);
            add_file(b, "./.env.example", b"SECRET=", 0o644);
            add_file(b, "./.env.production", b"SECRET=2", 0o600);
            add_file(b, "./node_modules/left-pad/index.js", b"x", 0o644);
            add_file(b, "./web/Node_Modules/x.js", b"x", 0o644);
            add_file(b, "./.git/config", b"[core]", 0o644);
            add_file(b, "./.ssh/id_rsa", b"key", 0o600);
            add_file(b, "./dist/bundle.js", b"x", 0o644);
        });
        let packed = zip_from_tar_gz("sbx_t", "/work", &data).expect("packs");
        assert_eq!(
            names(&packed),
            vec![".env.example", "package.json", "src/index.js", "start.sh"]
        );
        assert_eq!(packed.files, 4);
        assert_eq!(packed.left_out, 7);
    }

    /// What the deploy tool tells the model: build folders are left out at any
    /// depth, and a site already built deploys by packing the output folder
    /// itself, whose files then sit at the top of the archive.
    #[test]
    fn build_folders_are_left_out_at_any_depth_but_their_contents_pack_alone() {
        let project = tarball(|b| {
            add_file(b, "./package.json", b"{}", 0o644);
            add_file(b, "./src/build/helpers.js", b"x", 0o644);
            add_file(b, "./web/.next/cache/x", b"x", 0o644);
            add_file(b, "./api/target/debug/app", b"x", 0o755);
        });
        let packed = zip_from_tar_gz("sbx_t", "/work", &project).expect("packs");
        assert_eq!(names(&packed), vec!["package.json"]);
        assert_eq!(packed.left_out, 3);

        let output = tarball(|b| {
            add_file(b, "./index.html", b"<!doctype html>", 0o644);
            add_file(b, "./assets/app.js", b"x", 0o644);
        });
        let packed = zip_from_tar_gz("sbx_t", "/work/web/dist", &output).expect("packs");
        assert_eq!(names(&packed), vec!["assets/app.js", "index.html"]);
        assert_eq!(packed.left_out, 0);
    }

    #[test]
    fn the_executable_bit_survives() {
        let data = tarball(|b| {
            add_file(b, "./run.sh", b"x", 0o755);
            add_file(b, "./notes.txt", b"x", 0o600);
        });
        let packed = zip_from_tar_gz("sbx_t", "/work", &data).expect("packs");
        let file = std::fs::File::open(packed.archive.path()).expect("opens");
        let mut zip = zip::ZipArchive::new(file).expect("zip");
        assert_eq!(
            zip.by_name("run.sh").expect("run").unix_mode(),
            Some(0o100755)
        );
        assert_eq!(
            zip.by_name("notes.txt").expect("notes").unix_mode(),
            Some(0o100644)
        );
    }

    #[test]
    fn a_symlink_outside_excluded_paths_is_refused_by_name() {
        let data = tarball(|b| {
            add_file(b, "./index.html", b"<html>", 0o644);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_cksum();
            b.append_link(&mut header, "./config", "/etc/passwd")
                .expect("link appended");
        });
        let error = zip_from_tar_gz("sbx_t", "/work", &data)
            .err()
            .expect("refused");
        assert!(
            matches!(&error, SandboxDeployError::UnsafeEntry { path, .. } if path == "config"),
            "{error}"
        );
        assert!(error.to_string().contains("sbx_t"));
    }

    #[test]
    fn a_symlink_inside_node_modules_is_ignored_with_the_rest_of_it() {
        let data = tarball(|b| {
            add_file(b, "./index.html", b"<html>", 0o644);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_cksum();
            b.append_link(&mut header, "./node_modules/.bin/tool", "../tool/cli.js")
                .expect("link appended");
        });
        let packed = zip_from_tar_gz("sbx_t", "/work", &data).expect("packs");
        assert_eq!(names(&packed), vec!["index.html"]);
    }

    #[test]
    fn a_path_that_leaves_the_directory_is_refused() {
        // `tar::Builder` refuses to write `..`, so write the name bytes
        // directly, as a hostile tarball would.
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        let name = b"../escape.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        let mut raw = Vec::new();
        raw.extend_from_slice(header.as_bytes());
        let mut block = [0_u8; 512];
        block[0] = b'x';
        raw.extend_from_slice(&block);
        raw.extend_from_slice(&[0_u8; 1024]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).expect("gzip writes");
        let data = encoder.finish().expect("gzip finishes");

        let error = zip_from_tar_gz("sbx_t", "/work", &data)
            .err()
            .expect("refused");
        assert!(
            matches!(error, SandboxDeployError::UnsafeEntry { .. }),
            "{error}"
        );
    }

    #[test]
    fn nothing_deployable_is_an_error_that_names_the_directory() {
        let data = tarball(|b| {
            add_file(b, "./.env", b"SECRET=1", 0o600);
            add_file(b, "./node_modules/a.js", b"x", 0o644);
        });
        let error = zip_from_tar_gz("sbx_t", "/work/app", &data)
            .err()
            .expect("empty");
        assert!(matches!(error, SandboxDeployError::Empty { .. }));
        assert!(error.to_string().contains("/work/app"));
    }

    #[test]
    fn something_that_is_not_a_tarball_is_reported_not_panicked_on() {
        let error = zip_from_tar_gz("sbx_t", "/work", b"definitely not gzip")
            .err()
            .expect("rejected");
        assert!(
            matches!(error, SandboxDeployError::Archive { .. }),
            "{error}"
        );
    }

    #[test]
    fn the_pack_command_quotes_hostile_directories() {
        let command = pack_command("/work/it's a dir; rm -rf /", "/tmp/out.tgz");
        assert!(command.contains(r"'/work/it'\''s a dir; rm -rf /'"));
        assert!(command.starts_with("tar -czf '/tmp/out.tgz' -C "));
        assert!(command.ends_with("wc -c < '/tmp/out.tgz'"));
    }

    /// Runs the command for real: it packs a directory and prints the size,
    /// and a missing directory still fails rather than reporting an archive.
    #[test]
    fn the_pack_command_packs_and_still_fails_on_a_real_error() {
        let work = tempfile::tempdir().expect("tempdir");
        std::fs::write(work.path().join("index.html"), "<h1>hi</h1>").expect("write");
        let output = work.path().join("out.tgz");
        let source = work.path().join("site");
        std::fs::create_dir(&source).expect("mkdir");
        std::fs::write(source.join("index.html"), "<h1>hi</h1>").expect("write");
        let run = |directory: &str| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(pack_command(directory, &output.to_string_lossy()))
                .output()
                .expect("sh runs")
        };

        let packed = run(&source.to_string_lossy());
        assert!(packed.status.success(), "{packed:?}");
        let size: u64 = String::from_utf8_lossy(&packed.stdout)
            .trim()
            .parse()
            .expect("prints the size");
        assert_eq!(size, std::fs::metadata(&output).expect("archive").len());

        std::fs::remove_file(&output).expect("rm");
        let missing = run(&work.path().join("missing").to_string_lossy());
        assert!(!missing.status.success());
        assert!(missing.stdout.is_empty());
    }

    #[test]
    fn the_approval_sentence_says_what_will_be_uploaded_and_where() {
        let data = tarball(|b| add_file(b, "./index.html", &[b'x'; 3000], 0o644));
        let packed = zip_from_tar_gz("sbx_t", "/work", &data).expect("packs");
        let prepared = PreparedDeploy {
            packed,
            args: DeployArgs {
                project_id: 7,
                environment_id: Some(3),
                directory: None,
            },
            sandbox_id: "sbx_t".to_string(),
            directory: "/work".to_string(),
        };
        let summary = prepared.summary();
        assert!(summary.contains("1 files"), "{summary}");
        assert!(summary.contains("project 7"));
        assert!(summary.contains("environment 3"));
        assert!(summary.contains("without Git"));
    }

    #[test]
    fn arguments_without_a_project_are_explained() {
        let error = serde_json::from_str::<DeployArgs>("{}")
            .map_err(|e| {
                SandboxDeployError::Arguments {
                    reason: e.to_string(),
                }
                .to_string()
            })
            .expect_err("missing project");
        assert!(error.contains("project_id"), "{error}");
        assert!(error.contains("Nothing was deployed"));
    }

    #[test]
    fn the_tool_requires_a_project_and_is_a_sandbox_tool() {
        let tool = deploy_tool();
        assert_eq!(tool.name, SANDBOX_DEPLOY_TOOL);
        assert_eq!(tool.parameters["required"], json!(["project_id"]));
        assert!(crate::sandbox_link::is_sandbox_tool(SANDBOX_DEPLOY_TOOL));
    }

    #[test]
    fn a_tarball_within_the_limit_is_used_whatever_wc_said() {
        let read = Ok(vec![1u8, 2, 3]);
        assert_eq!(
            bounded_tarball("sbx_1", "/w", 999, read).expect("used"),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn a_tarball_the_bounded_read_refused_is_too_large_even_if_wc_said_it_was_small() {
        // A shimmed `wc` reported 10 bytes; the bounded read saw more than the
        // limit and refused it.
        let read = Err(temps_sandbox::error::SandboxError::Validation {
            message: "read_file: '/tmp/x.tgz' is larger than the limit".to_string(),
        });
        let error = bounded_tarball("sbx_1", "/w", 10, read).unwrap_err();
        assert!(matches!(
            error,
            SandboxDeployError::PackedOverLimit { ref sandbox_id, ref directory, max }
                if sandbox_id == "sbx_1" && directory == "/w" && max == MAX_PACKED_BYTES
        ));
        let message = error.to_string();
        assert!(
            message.contains("more than the 209715200 byte limit"),
            "{message}"
        );
        assert!(message.contains("sbx_1"));
    }

    #[test]
    fn an_oversized_tarball_a_provider_returned_anyway_is_refused_by_its_real_size() {
        let held = vec![0u8; MAX_PACKED_BYTES as usize + 1];
        let error = bounded_tarball("sbx_1", "/w", 10, Ok(held)).unwrap_err();
        assert!(matches!(
            error,
            SandboxDeployError::PackedTooLarge { bytes, max, .. }
                if bytes == MAX_PACKED_BYTES + 1 && max == MAX_PACKED_BYTES
        ));
    }

    #[test]
    fn a_failed_tarball_read_is_a_pack_error_that_names_the_sandbox() {
        let read = Err(temps_sandbox::error::SandboxError::FileOp {
            sandbox_id: "sbx_1".to_string(),
            op: "read".to_string(),
            path: "/tmp/x.tgz".to_string(),
            reason: "download failed".to_string(),
        });
        let error = bounded_tarball("sbx_1", "/w", 42, read).unwrap_err();
        assert!(matches!(error, SandboxDeployError::Pack { .. }));
        let message = error.to_string();
        assert!(message.contains("sbx_1") && message.contains("download failed"));
        assert!(message.contains("reported as 42 bytes"));
    }

    fn deploy_user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 7,
            name: "Deploying User".to_string(),
            email: "deployer@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn session(role: temps_auth::permissions::Role) -> AuthContext {
        AuthContext::new_session(deploy_user(), role)
    }

    /// A team layer that answers with a project role's permissions, which the
    /// test may change between calls (a role revoked mid-approval).
    struct ProjectRoleChecker {
        permissions: std::sync::Mutex<Option<Vec<String>>>,
        member: bool,
        fail: bool,
    }

    impl ProjectRoleChecker {
        fn with(permissions: Option<&[Permission]>) -> Arc<Self> {
            Arc::new(Self {
                permissions: std::sync::Mutex::new(
                    permissions.map(|p| p.iter().map(|p| p.to_string()).collect()),
                ),
                member: true,
                fail: false,
            })
        }
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for ProjectRoleChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.member)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            if self.fail {
                return Err(Box::new(std::io::Error::other("team store unavailable")));
            }
            Ok(self
                .permissions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone())
        }
    }

    fn as_checker(checker: &Arc<ProjectRoleChecker>) -> Arc<dyn temps_core::ProjectAccessChecker> {
        checker.clone()
    }

    const VIEWER: &[Permission] = &[Permission::ProjectsRead, Permission::DeploymentsRead];
    const EDITOR: &[Permission] = &[
        Permission::ProjectsRead,
        Permission::ProjectsWrite,
        Permission::DeploymentsRead,
        Permission::DeploymentsCreate,
    ];

    #[tokio::test]
    async fn a_project_viewer_cannot_deploy_even_with_the_instance_permission() {
        let auth = session(temps_auth::permissions::Role::User);
        assert!(auth.has_permission(&Permission::DeploymentsCreate));
        let checker = as_checker(&ProjectRoleChecker::with(Some(VIEWER)));
        let message = authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .unwrap_err();
        assert!(message.contains("project 5"), "{message}");
        assert!(message.contains("deployments:create"), "{message}");
        assert!(message.contains("nothing was deployed"), "{message}");
    }

    #[tokio::test]
    async fn a_project_editor_can_deploy() {
        let auth = session(temps_auth::permissions::Role::User);
        let checker = as_checker(&ProjectRoleChecker::with(Some(EDITOR)));
        authorize_deploy(&auth, 5, Some(5), Some(&checker))
            .await
            .expect("an editor deploys");
    }

    #[tokio::test]
    async fn deploying_also_needs_project_write_because_it_may_promote_the_source() {
        let auth = session(temps_auth::permissions::Role::User);
        let deploy_only = [Permission::ProjectsRead, Permission::DeploymentsCreate];
        let checker = as_checker(&ProjectRoleChecker::with(Some(&deploy_only)));
        let message = authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .unwrap_err();
        assert!(message.contains("projects:write"), "{message}");
        assert!(!message.contains("deployments:create"), "{message}");
    }

    #[tokio::test]
    async fn a_role_revoked_before_approval_is_refused_at_execution() {
        let auth = session(temps_auth::permissions::Role::User);
        let roles = ProjectRoleChecker::with(Some(EDITOR));
        let checker = as_checker(&roles);
        authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .expect("allowed when the tool is called");
        *roles
            .permissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(VIEWER.iter().map(|p| p.to_string()).collect());
        assert!(authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn without_a_per_permission_answer_project_access_decides() {
        let auth = session(temps_auth::permissions::Role::User);
        let member = as_checker(&ProjectRoleChecker::with(None));
        authorize_deploy(&auth, 5, None, Some(&member))
            .await
            .expect("a member deploys");
        let outsider: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(ProjectRoleChecker {
            permissions: std::sync::Mutex::new(None),
            member: false,
            fail: false,
        });
        let message = authorize_deploy(&auth, 5, None, Some(&outsider))
            .await
            .unwrap_err();
        assert!(
            message.contains("do not have access to project 5"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_failed_permission_lookup_refuses_the_deploy() {
        let auth = session(temps_auth::permissions::Role::User);
        let broken: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(ProjectRoleChecker {
            permissions: std::sync::Mutex::new(Some(Vec::new())),
            member: true,
            fail: true,
        });
        let message = authorize_deploy(&auth, 5, None, Some(&broken))
            .await
            .unwrap_err();
        assert!(message.contains("could not be verified"), "{message}");
    }

    #[tokio::test]
    async fn instance_admins_are_not_narrowed_by_project_roles() {
        let auth = session(temps_auth::permissions::Role::Admin);
        let checker = as_checker(&ProjectRoleChecker::with(Some(VIEWER)));
        authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .expect("an admin deploys");
    }

    #[tokio::test]
    async fn an_instance_reader_is_refused_before_any_project_check() {
        let auth = session(temps_auth::permissions::Role::Reader);
        let checker = as_checker(&ProjectRoleChecker::with(Some(EDITOR)));
        let message = authorize_deploy(&auth, 5, None, Some(&checker))
            .await
            .unwrap_err();
        assert!(message.contains("deployments:create"), "{message}");
    }

    #[tokio::test]
    async fn a_project_chat_deploys_only_to_its_project() {
        let auth = session(temps_auth::permissions::Role::Admin);
        let message = authorize_deploy(&auth, 6, Some(5), None).await.unwrap_err();
        assert!(message.contains("belongs to project 5"), "{message}");
    }
}
