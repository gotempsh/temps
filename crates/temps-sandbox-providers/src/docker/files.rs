// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Moving files in and out of a sandbox: turn-scoped secret files, bounded
//! reads, and planning and staging directory uploads.

use super::*;

/// Tar framing a bounded `read_file` tolerates on top of the file's own
/// size: header blocks (including PAX/GNU long-name records for paths up to
/// `PATH_MAX`), padding and the end-of-archive marker.
const READ_FILE_TAR_OVERHEAD: u64 = 64 * 1024;

pub(super) fn is_turn_secret_path(path: &str) -> bool {
    path.strip_prefix("/run/secrets/temps-chat-")
        .is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        })
}

pub(super) fn runtime_user_owns_uploaded_file(path: &str) -> bool {
    path.starts_with(SANDBOX_HOME) || is_turn_secret_path(path)
}

impl DockerSandboxProvider {
    /// Upgrade-safe preparation for the tmpfs directory that carries a
    /// turn-scoped chat capability. Containers created before the directory
    /// ownership fix keep their original `0700 root:root` mount across a
    /// restart, so correcting only the create configuration would leave every
    /// existing persistent sandbox broken. Reconcile immediately before each
    /// capability upload and fail with context if the runtime user still could
    /// not traverse the directory.
    async fn prepare_turn_secret_directory(
        &self,
        handle: &SandboxHandle,
    ) -> Result<(), AgentError> {
        let exit = self
            .run_root_exec(
                &handle.sandbox_id,
                0,
                "turn-secret-directory",
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("chown 0:{SANDBOX_GID} /run/secrets && chmod 0710 /run/secrets"),
                ],
            )
            .await?;
        if exit != 0 {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "could not make /run/secrets traversable by sandbox gid {SANDBOX_GID}; maintenance command exited with {exit}"
                ),
            });
        }
        Ok(())
    }

    /// Stream a turn-scoped capability into the live tmpfs mount.
    ///
    /// Docker's archive upload API writes below a tmpfs mount and still
    /// reports success, leaving the uploaded file invisible to processes in
    /// the running container. Attached stdin reaches the mounted namespace
    /// without putting capability bytes in argv, env, logs, or a persistent
    /// staging file. The destination is restricted by
    /// [`is_turn_secret_path`] and is passed as a positional argument rather
    /// than interpolated into the fixed shell program.
    pub(super) async fn write_turn_secret_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        if mode != 0o600 {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "turn capability '{}' must use mode 0600, got {mode:o}",
                    path
                ),
            });
        }
        self.prepare_turn_secret_directory(handle).await?;
        let fail = |reason: String| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason,
        };
        let exec = self
            .docker
            .create_exec(
                &handle.sandbox_id,
                bollard::models::ExecConfig {
                    user: Some("0:0".to_string()),
                    cmd: Some(vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        concat!(
                            "umask 077; ",
                            "/usr/bin/tee -- \"$1\" >/dev/null && ",
                            "/usr/bin/chown 1000:1000 -- \"$1\" && ",
                            "/usr/bin/chmod 0600 -- \"$1\""
                        )
                        .to_string(),
                        "temps-turn-secret".to_string(),
                        path.to_string(),
                    ]),
                    env: Some(vec![format!("PATH={ROOT_EXEC_PATH}")]),
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| fail(format!("could not create turn capability upload: {error}")))?;
        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| fail(format!("could not start turn capability upload: {error}")))?;

        let StartExecResults::Attached {
            mut output,
            mut input,
        } = started
        else {
            return Err(fail(
                "turn capability upload unexpectedly started detached".to_string(),
            ));
        };
        input
            .write_all(contents)
            .await
            .map_err(|error| fail(format!("could not stream turn capability: {error}")))?;
        input
            .shutdown()
            .await
            .map_err(|error| fail(format!("could not finish turn capability stream: {error}")))?;

        let mut stderr_tail = String::new();
        while let Some(chunk) = output.next().await {
            match chunk {
                Ok(LogOutput::StdErr { message }) => {
                    stderr_tail.push_str(&String::from_utf8_lossy(&message));
                    if stderr_tail.len() > 2_048 {
                        stderr_tail.drain(..stderr_tail.len() - 2_048);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(fail(format!(
                        "turn capability upload output failed: {error}"
                    )))
                }
            }
        }
        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|error| fail(format!("could not inspect turn capability upload: {error}")))?
            .exit_code
            .unwrap_or(-1);
        if exit_code != 0 {
            return Err(fail(format!(
                "turn capability upload exited with {exit_code}: {}",
                stderr_tail.trim()
            )));
        }
        Ok(())
    }

    /// Download one file through Docker's archive endpoint. With
    /// `max_bytes`, the download stops as soon as the archive grows past the
    /// limit (plus tar framing), so an oversized file is never buffered.
    pub(super) async fn read_file_limited(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>, AgentError> {
        use futures::StreamExt;
        use std::io::Read;

        let options = bollard::query_parameters::DownloadFromContainerOptionsBuilder::default()
            .path(path)
            .build();

        let stream = self
            .docker
            .download_from_container(&handle.sandbox_id, Some(options));

        // Collect tar stream into memory with a hard 30s cap so we never hang.
        let collect = async {
            let mut buf: Vec<u8> = Vec::new();
            let mut s = stream;
            while let Some(chunk) = s.next().await {
                match chunk {
                    Ok(bytes) => {
                        buf.extend_from_slice(&bytes);
                        if let Some(max) = max_bytes {
                            if buf.len() as u64 > max.saturating_add(READ_FILE_TAR_OVERHEAD) {
                                return Err(super::file_too_large(handle, path, max));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id: handle.sandbox_id.clone(),
                            reason: format!("read_file: download {} failed: {}", path, e),
                        });
                    }
                }
            }
            Ok(buf)
        };

        let tar_bytes =
            match tokio::time::timeout(std::time::Duration::from_secs(30), collect).await {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("read_file: download {} timed out after 30s", path),
                    });
                }
            };

        // Extract the single file from the tar. Docker's archive endpoint
        // returns a tar whose top-level entry is the basename of `path`.
        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let mut entries = archive
            .entries()
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("read_file: tar open for {} failed: {}", path, e),
            })?;

        for entry in entries.by_ref() {
            let mut entry = entry.map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("read_file: tar entry for {} failed: {}", path, e),
            })?;
            // Skip directories, symlinks, etc. — we want the regular file.
            if entry.header().entry_type().is_file() {
                if let Some(max) = max_bytes {
                    if entry.size() > max {
                        return Err(super::file_too_large(handle, path, max));
                    }
                }
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("read_file: read entry for {} failed: {}", path, e),
                    })?;
                return Ok(contents);
            }
        }

        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!("read_file: no regular file entry in tar for {}", path),
        })
    }
}

/// Why [`directory_upload_files`] left an entry out of an upload.
///
/// The variants deliberately carry no host paths: they are rendered into
/// user-facing run logs, while the resolved host path is logged at `warn`
/// where the entry is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadSkipReason {
    /// A symlink that resolves outside the containment root.
    OutsideRoot,
    /// A symlink that leads back into one of its own ancestors.
    LinkLoop,
    /// A dangling symlink, or an entry that could not be read or resolved.
    Unreadable,
}

impl std::fmt::Display for UploadSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            UploadSkipReason::OutsideRoot => "link points outside the allowed directory",
            UploadSkipReason::LinkLoop => "link loops back into its own parent",
            UploadSkipReason::Unreadable => "dangling link or unreadable entry",
        })
    }
}

/// An entry under the uploaded directory that was not uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedUploadEntry {
    /// Path relative to the uploaded directory.
    pub relative_path: PathBuf,
    pub reason: UploadSkipReason,
}

/// What `write_directory` uploads from a directory, and what it left out.
#[derive(Debug, Default)]
pub struct DirectoryUploadPlan {
    /// Regular files as (resolved path on this host, path relative to the
    /// uploaded directory).
    pub files: Vec<(PathBuf, PathBuf)>,
    /// Entries left out, in walk order. Every one is also logged at `warn`.
    pub skipped: Vec<SkippedUploadEntry>,
}

/// Regular files to upload from `local_dir`, and the entries left out.
///
/// Symlinks are followed — skills and `.claude/` overlays legitimately link
/// files and directories inside their own tree — but only while they
/// resolve inside `containment_root`. A link that points anywhere else
/// (`/`, the node's token or TLS key, a different sandbox's work directory)
/// is skipped, so the contents of the uploaded tree can never pull host
/// files into a sandbox. Directories reached through such a link are not
/// descended into; dangling links and link loops are skipped too. Every
/// skipped entry is logged at `warn` and returned in
/// [`DirectoryUploadPlan::skipped`] so the caller can tell the user.
///
/// `containment_root` is the uploaded directory itself unless the caller
/// owns a wider tree the links may legitimately point into — e.g. the
/// repository checkout a `.claude/` overlay comes from. It must contain
/// `local_dir`; anything else is rejected with `InvalidInput`.
pub fn directory_upload_files(
    local_dir: &Path,
    containment_root: &Path,
) -> std::io::Result<DirectoryUploadPlan> {
    let root = std::fs::canonicalize(containment_root)?;
    let resolved_dir = std::fs::canonicalize(local_dir)?;
    if !resolved_dir.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "upload directory {} resolves to {}, outside its containment root {}",
                local_dir.display(),
                resolved_dir.display(),
                root.display()
            ),
        ));
    }

    let relative_of = |path: &Path| -> PathBuf {
        path.strip_prefix(local_dir)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut plan = DirectoryUploadPlan::default();
    let mut walker = walkdir::WalkDir::new(local_dir)
        .follow_links(true)
        .into_iter();
    while let Some(next) = walker.next() {
        let entry = match next {
            Ok(entry) => entry,
            Err(error) => {
                let reason = if error.loop_ancestor().is_some() {
                    UploadSkipReason::LinkLoop
                } else {
                    UploadSkipReason::Unreadable
                };
                let relative_path = error.path().map(relative_of).unwrap_or_default();
                tracing::warn!(
                    path = %relative_path.display(),
                    local_dir = %local_dir.display(),
                    %reason,
                    %error,
                    "Skipping an entry that cannot be walked while uploading a directory to a sandbox"
                );
                plan.skipped.push(SkippedUploadEntry {
                    relative_path,
                    reason,
                });
                continue;
            }
        };
        let is_dir = entry.file_type().is_dir();
        let resolved = match std::fs::canonicalize(entry.path()) {
            Ok(resolved) if resolved.starts_with(&root) => resolved,
            Ok(resolved) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    resolves_to = %resolved.display(),
                    root = %root.display(),
                    "Skipping a link that resolves outside the containment root of a sandbox upload"
                );
                if is_dir {
                    walker.skip_current_dir();
                }
                plan.skipped.push(SkippedUploadEntry {
                    relative_path: relative_of(entry.path()),
                    reason: UploadSkipReason::OutsideRoot,
                });
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    root = %root.display(),
                    %error,
                    "Skipping an entry that cannot be resolved while uploading a directory to a sandbox"
                );
                if is_dir {
                    walker.skip_current_dir();
                }
                plan.skipped.push(SkippedUploadEntry {
                    relative_path: relative_of(entry.path()),
                    reason: UploadSkipReason::Unreadable,
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue; // dirs are created implicitly by tar entries
        }
        let Ok(relative) = entry.path().strip_prefix(local_dir) else {
            continue;
        };
        // Read from the path that was checked, not the logical one, so the
        // bytes uploaded are the ones the containment check approved.
        plan.files.push((resolved, relative.to_path_buf()));
    }
    Ok(plan)
}

/// Copy the files [`directory_upload_files`] selects from `local_dir`
/// (links followed while they stay inside `containment_root`) into
/// `staging_dir` as plain files, and return the plan that was applied.
///
/// For callers whose links may legitimately reach outside the uploaded
/// directory: they stage here, then hand `staging_dir` — which contains no
/// links at all — to [`SandboxProvider::write_directory`]. That works the
/// same for every provider (local Docker, worker nodes, Firecracker), each
/// of which only follows links that stay inside the directory it is given.
///
/// [`SandboxProvider::write_directory`]: super::SandboxProvider::write_directory
pub fn stage_directory_upload(
    local_dir: &Path,
    containment_root: &Path,
    staging_dir: &Path,
) -> std::io::Result<DirectoryUploadPlan> {
    let plan = directory_upload_files(local_dir, containment_root)?;
    for (source, relative) in &plan.files {
        let destination = staging_dir.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("create staging directory {}: {}", parent.display(), e),
                )
            })?;
        }
        std::fs::copy(source, &destination).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "copy {} to staging file {}: {}",
                    source.display(),
                    destination.display(),
                    e
                ),
            )
        })?;
    }
    Ok(plan)
}
