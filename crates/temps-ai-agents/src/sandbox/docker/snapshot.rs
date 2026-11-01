// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Snapshots: workspace archives, content-addressed artifacts, credential
//! scrubbing of committed images, and taking and restoring a snapshot.

use super::*;

fn workspace_mount_source(mounts: &[bollard::models::MountPoint]) -> Option<PathBuf> {
    mounts.iter().find_map(|mount| {
        (mount.destination.as_deref() == Some(CONTAINER_WORK_DIR))
            .then(|| mount.source.as_deref().map(PathBuf::from))
            .flatten()
    })
}

pub(super) fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size_bytes = size_bytes.saturating_add(read as u64);
    }
    Ok((hex::encode(hasher.finalize()), size_bytes))
}

pub(super) async fn publish_content_addressed_file(
    temporary: &Path,
    destination: &Path,
    expected_digest: &str,
) -> std::io::Result<bool> {
    match tokio::fs::hard_link(temporary, destination).await {
        Ok(()) => {
            tokio::fs::remove_file(temporary).await?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = destination.to_path_buf();
            let (actual_digest, _) = tokio::task::spawn_blocking(move || hash_file(&existing))
                .await
                .map_err(|join_error| {
                    std::io::Error::other(format!(
                        "verify existing content-addressed artifact task failed: {}",
                        join_error
                    ))
                })??;
            if actual_digest != expected_digest {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "existing content-addressed artifact '{}' has digest {}, expected {}",
                        destination.display(),
                        actual_digest,
                        expected_digest
                    ),
                ));
            }
            tokio::fs::remove_file(temporary).await?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

struct LimitedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W: std::io::Write> std::io::Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(buffer.len() as u64) > self.limit {
            return Err(std::io::Error::other(format!(
                "snapshot artifact exceeds the {} byte limit",
                self.limit
            )));
        }
        let written = self.inner.write(buffer)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn archive_workspace(
    source: &Path,
    destination: &Path,
    max_size_bytes: u64,
) -> std::io::Result<(String, u64)> {
    let file = LimitedWriter {
        inner: std::fs::File::create(destination)?,
        written: 0,
        limit: max_size_bytes,
    };
    let mut archive = tar::Builder::new(file);
    archive.follow_symlinks(false);
    archive.append_dir_all(".", source)?;
    let file = archive.into_inner()?.inner;
    file.sync_all()?;
    hash_file(destination)
}

pub(super) fn restore_workspace_archive(
    artifact: &super::SnapshotCompanionArtifact,
    destination: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    if std::fs::read_dir(destination)?
        .next()
        .transpose()?
        .is_some()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "workspace restore destination '{}' is not empty",
                destination.display()
            ),
        ));
    }

    let (actual_digest, _) = hash_file(&artifact.content_path)?;
    if actual_digest != artifact.content_digest {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "workspace artifact digest mismatch: expected {}, got {}",
                artifact.content_digest, actual_digest
            ),
        ));
    }

    let file = std::fs::File::open(&artifact.content_path)?;
    let mut archive = tar::Archive::new(file);
    archive.unpack(destination)
}

pub(super) fn combined_snapshot_digest(image_digest: &str, workspace_digest: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"temps-sandbox-snapshot-v2\0");
    hasher.update(image_digest.as_bytes());
    hasher.update(b"\0");
    hasher.update(workspace_digest.as_bytes());
    hex::encode(hasher.finalize())
}

// ── Credential-scrubbing helpers (ADR-037 §4) ─────────────────────────────────
//
// These are module-level so they can be unit-tested without a Docker daemon.
// The production path in `take_snapshot` calls them; the test suite verifies
// the pattern list and the scrubbing logic independently.

/// Known-sensitive env-var key patterns from ADR-013 + ADR-037.
///
/// Matching is case-insensitive substring: a key is considered sensitive when
/// its uppercased form **contains** any of these patterns. This deliberately
/// catches variants like `MY_ANTHROPIC_API_KEY` and `GITHUB_TOKEN_READONLY`
/// without requiring an exhaustive allowlist.
///
/// **Security invariant**: this list is the single source of truth for which
/// env vars the scrubber strips. Adding a new secret kind to the sandbox API
/// must be accompanied by adding its pattern here. Keep this in sync with
/// every env var injected at sandbox creation time in:
///   - `crates/temps-ai-agents/src/services/executor.rs`  (CLAUDE_CODE_OAUTH_TOKEN)
///   - `crates/temps-ai-agents/src/handlers/trigger.rs`   (CLAUDE_CODE_OAUTH_TOKEN)
///   - `crates/temps-ai-agents/src/services/sandbox_injector.rs`
pub(crate) const SENSITIVE_ENV_PATTERNS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "BITBUCKET_TOKEN",
    "GIT_TOKEN",
    "API_KEY",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AWS_SECRET",
    "AWS_ACCESS_KEY",
    "AZURE_CLIENT_SECRET",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CODEX_API_KEY",
    "OPENCODE_API_KEY",
    // OAuth tokens — covers CLAUDE_CODE_OAUTH_TOKEN injected by executor.rs
    // and trigger.rs for Claude subscription auth, and any future OAuth tokens.
    "OAUTH_TOKEN",
    "TEMPS_",
];

/// Returns `true` if `key` (case-insensitive) matches any sensitive pattern.
///
/// Called by `take_snapshot` for both the scrubbing step (zero-out the value)
/// and the verification step (reject if a non-empty value survives).
pub(crate) fn is_sensitive_env_key(key: &str) -> bool {
    let key_upper = key.to_uppercase();
    SENSITIVE_ENV_PATTERNS
        .iter()
        .any(|pat| key_upper.contains(pat))
}

/// Build Dockerfile-style `ENV KEY=` change instructions for every sensitive
/// entry found in a `KEY=VALUE` env list.
///
/// For each entry whose key matches [`is_sensitive_env_key`], produces a
/// `"ENV KEY="` string (empty value). These strings are passed to
/// `CommitContainerOptionsBuilder::changes()`, which maps to Docker's
/// `changes` query parameter — the **only** mechanism that actually overwrites
/// env-var values in the committed image's `Config.Env`.
///
/// **Why zeroing rather than removal:** Docker's commit API has no mechanism
/// to delete an env entry; it can only overwrite the value. `ENV KEY=` sets
/// the key to an empty string in the committed image. This was verified
/// against a real Docker daemon using `docker commit --change 'ENV KEY='`.
/// The `ContainerConfig` body's `env` field (the previous approach) is
/// silently ignored by the Docker Engine and has zero effect on the committed
/// image — a confirmed no-op, not a design choice.
///
/// Non-sensitive entries are not included in the result — only the change
/// instructions for the keys that need scrubbing are returned.
pub(crate) fn build_env_scrub_changes(env: &[String]) -> Vec<String> {
    env.iter()
        .filter_map(|kv| {
            let key = kv.split('=').next().unwrap_or("");
            if is_sensitive_env_key(key) {
                Some(format!("ENV {}=", key))
            } else {
                None
            }
        })
        .collect()
}

/// Returns the keys of any env entries that are sensitive AND have a non-empty
/// value after `=` (i.e. were not successfully zeroed by the scrubbing step).
///
/// A sensitive key with an **empty** value (`KEY=`) has been successfully
/// zeroed by `docker commit --change 'ENV KEY='` and is NOT a survivor.
/// A sensitive key with a **non-empty** value (`KEY=actual-secret`) means
/// the Docker `changes` mechanism was bypassed or failed and the value leaked
/// into the committed image — this is the real failure condition.
///
/// Called after `docker commit` to verify the scrubbing worked. An empty
/// return means the image config is clean. A non-empty return triggers an
/// abort: the staged image is removed and `take_snapshot` returns an error.
pub(crate) fn find_surviving_sensitive_keys(env: &[String]) -> Vec<String> {
    env.iter()
        .filter_map(|kv| {
            let mut parts = kv.splitn(2, '=');
            let key = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            // A sensitive key with an empty value has been zeroed by the
            // `ENV KEY=` change instruction — that is the expected post-commit
            // state and is NOT a failure. Only a non-empty value means the
            // secret survived into the committed image.
            if is_sensitive_env_key(key) && !value.is_empty() {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn snapshot_failed(container_id: &str, reason: String) -> AgentError {
    AgentError::SandboxExecFailed {
        run_id: 0,
        sandbox_id: container_id.to_string(),
        reason,
    }
}

/// Force-remove a staged snapshot image in the background. Best effort: a
/// cleanup failure must not mask the error that triggered it.
fn spawn_remove_staged_image(docker: Arc<Docker>, image_id: String) {
    tokio::spawn(async move {
        let _ = docker
            .remove_image(
                &image_id,
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .force(true)
                        .build(),
                ),
                None,
            )
            .await;
    });
}

/// `<data dir>/snapshots`, created if missing.
async fn create_snapshots_dir(container_id: &str) -> Result<PathBuf, AgentError> {
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: TEMPS_DATA_DIR (with host lookup of HOME as fallback) locates the data directory and is fixed before the database exists"
    )]
    let data_dir = std::env::var("TEMPS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".temps")
        });
    let snapshots_dir = data_dir.join("snapshots");
    tokio::fs::create_dir_all(&snapshots_dir)
        .await
        .map_err(|e| {
            snapshot_failed(
                container_id,
                format!(
                    "snapshot: failed to create snapshots directory {}: {}",
                    snapshots_dir.display(),
                    e
                ),
            )
        })?;
    Ok(snapshots_dir)
}

/// A committed, scrubbed snapshot image and the temp files its export
/// writes: everything a failed snapshot must clean up.
struct StagedSnapshot {
    docker: Arc<Docker>,
    container_id: String,
    committed_image_id: String,
    /// Repository of the staging tag (`<commit_tag>:latest`).
    commit_tag: String,
    tmp_path: PathBuf,
    workspace_tmp_path: PathBuf,
}

/// The content-addressed artifacts a snapshot published, and whether this
/// attempt created each file (a deduplicated one belongs to another).
struct PublishedSnapshot {
    digest_hex: String,
    final_path: PathBuf,
    workspace_final_path: PathBuf,
    created_primary: bool,
    created_workspace: bool,
}

impl StagedSnapshot {
    fn failed(&self, reason: String) -> AgentError {
        snapshot_failed(&self.container_id, reason)
    }

    /// Best-effort cleanup on failure — remove the staged Docker image and
    /// unlink the temp files if they were created. Both are best-effort: a
    /// cleanup failure should not mask the original error.
    fn cleanup_on_err(&self) {
        let docker = self.docker.clone();
        let img = self.committed_image_id.clone();
        let tmp = self.tmp_path.clone();
        let workspace_tmp = self.workspace_tmp_path.clone();
        tokio::spawn(async move {
            let _ = docker
                .remove_image(
                    &img,
                    Some(
                        bollard::query_parameters::RemoveImageOptionsBuilder::new()
                            .force(true)
                            .build(),
                    ),
                    None,
                )
                .await;
            // Best-effort unlink of the partially-written temp file.
            let _ = tokio::fs::remove_file(&tmp).await;
            let _ = tokio::fs::remove_file(&workspace_tmp).await;
        });
    }

    /// Stream `docker save` of the staged image into the image temp file,
    /// returning its SHA-256 and size. Fails past `max_size_bytes`.
    async fn export_image_to_temp(&self, max_size_bytes: u64) -> Result<(String, u64), AgentError> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;

        let mut tmp_file = match tokio::fs::File::create(&self.tmp_path).await {
            Ok(f) => f,
            Err(e) => {
                self.cleanup_on_err();
                return Err(self.failed(format!("snapshot: failed to create temp file: {}", e)));
            }
        };

        let mut hasher = Sha256::new();
        let mut size_bytes: u64 = 0;

        {
            let mut export_stream = self.docker.export_image(&self.committed_image_id);
            while let Some(chunk) = futures::StreamExt::next(&mut export_stream).await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        drop(tmp_file);
                        self.cleanup_on_err();
                        return Err(
                            self.failed(format!("snapshot: image export stream error: {}", e))
                        );
                    }
                };
                if size_bytes.saturating_add(chunk.len() as u64) > max_size_bytes {
                    drop(tmp_file);
                    self.cleanup_on_err();
                    return Err(AgentError::SnapshotSizeLimitExceeded {
                        sandbox_id: self.container_id.clone(),
                        stage: "exporting the Docker image".to_string(),
                        max_size_bytes,
                    });
                }
                hasher.update(&chunk);
                size_bytes += chunk.len() as u64;
                if let Err(e) = tmp_file.write_all(&chunk).await {
                    drop(tmp_file);
                    self.cleanup_on_err();
                    return Err(
                        self.failed(format!("snapshot: failed to write to temp file: {}", e))
                    );
                }
            }
        }

        if let Err(e) = tmp_file.flush().await {
            drop(tmp_file);
            self.cleanup_on_err();
            return Err(self.failed(format!("snapshot: failed to flush temp file: {}", e)));
        }
        drop(tmp_file);

        Ok((hex::encode(hasher.finalize()), size_bytes))
    }

    /// Archive `workspace_source` into the workspace temp file, within
    /// `workspace_limit` bytes (what the image left of `max_size_bytes`).
    async fn archive_workspace_to_temp(
        &self,
        workspace_source: &Path,
        workspace_limit: u64,
        max_size_bytes: u64,
    ) -> Result<(String, u64), AgentError> {
        let workspace_source_for_archive = workspace_source.to_path_buf();
        let workspace_tmp_for_archive = self.workspace_tmp_path.clone();
        let workspace_archive = tokio::task::spawn_blocking(move || {
            archive_workspace(
                &workspace_source_for_archive,
                &workspace_tmp_for_archive,
                workspace_limit,
            )
        })
        .await;
        match workspace_archive {
            Ok(Ok(artifact)) => Ok(artifact),
            Ok(Err(e)) => {
                self.cleanup_on_err();
                if e.to_string().contains("snapshot artifact exceeds the") {
                    return Err(AgentError::SnapshotSizeLimitExceeded {
                        sandbox_id: self.container_id.clone(),
                        stage: "archiving the Docker workspace".to_string(),
                        max_size_bytes,
                    });
                }
                Err(self.failed(format!(
                    "snapshot: failed to archive workspace '{}': {}",
                    workspace_source.display(),
                    e
                )))
            }
            Err(e) => {
                self.cleanup_on_err();
                Err(self.failed(format!("snapshot: workspace archive task failed: {}", e)))
            }
        }
    }

    /// Move both temp files to their content-addressed names in
    /// `snapshots_dir`.
    async fn publish(
        &self,
        snapshots_dir: &Path,
        image_digest: &str,
        workspace_digest: &str,
    ) -> Result<PublishedSnapshot, AgentError> {
        let digest_hex = combined_snapshot_digest(image_digest, workspace_digest);
        let final_path = snapshots_dir.join(format!("{}.tar", digest_hex));
        let workspace_final_path = snapshots_dir.join(format!("{}.workspace.tar", digest_hex));

        // Publish without replacing an existing content-addressed file. A
        // concurrent or deduplicated snapshot may already own the path; this
        // attempt must never unlink or overwrite that shared artifact.
        let created_workspace = match publish_content_addressed_file(
            &self.workspace_tmp_path,
            &workspace_final_path,
            workspace_digest,
        )
        .await
        {
            Ok(created) => created,
            Err(e) => {
                self.cleanup_on_err();
                return Err(self.failed(format!(
                    "snapshot: workspace artifact publish failed: {}",
                    e
                )));
            }
        };
        let created_primary =
            match publish_content_addressed_file(&self.tmp_path, &final_path, image_digest).await {
                Ok(created) => created,
                Err(e) => {
                    if created_workspace {
                        let _ = tokio::fs::remove_file(&workspace_final_path).await;
                    }
                    self.cleanup_on_err();
                    return Err(
                        self.failed(format!("snapshot: image artifact publish failed: {}", e))
                    );
                }
            };

        Ok(PublishedSnapshot {
            digest_hex,
            final_path,
            workspace_final_path,
            created_primary,
            created_workspace,
        })
    }

    /// Tag the committed image with the canonical, content-addressed name,
    /// returning the image reference.
    async fn tag_with_digest(&self, published: &PublishedSnapshot) -> Result<String, AgentError> {
        // The tag is derived from the tarball digest, never from the caller's
        // human label.
        //
        // Docker tags are mutable and shared per daemon. When the tag was
        // `temps-snapshot/<sanitized-label>:latest`, any tenant taking a
        // snapshot labelled "backup" re-pointed `temps-snapshot/backup:latest`
        // at their own image — and restore only checks whether that tag
        // *exists*, so the next tenant to restore their own "backup" snapshot
        // would silently run the attacker's image, with their env vars and
        // their git credentials.
        //
        // A content-addressed tag removes the collision entirely: two tenants
        // can only share a tag by having byte-identical images, in which case
        // sharing it is correct and is what the existing
        // `content_digest`-based dedup already does.
        let image_label = published.digest_hex.clone();
        let image_ref = format!("temps-snapshot/{}:latest", image_label);

        // Re-tag the committed image to the canonical snapshot name.
        if let Err(e) = self
            .docker
            .tag_image(
                &self.committed_image_id,
                Some(
                    bollard::query_parameters::TagImageOptionsBuilder::new()
                        .repo(format!("temps-snapshot/{}", image_label).as_str())
                        .tag("latest")
                        .build(),
                ),
            )
            .await
        {
            tracing::warn!(
                "snapshot: failed to tag committed image as {}: {}",
                image_ref,
                e
            );
            // A failed row has no DB reference to either artifact. Remove both
            // published files here so a daemon tagging failure cannot leak
            // storage indefinitely.
            if published.created_primary {
                let _ = tokio::fs::remove_file(&published.final_path).await;
            }
            if published.created_workspace {
                let _ = tokio::fs::remove_file(&published.workspace_final_path).await;
            }
            self.cleanup_on_err();
            return Err(self.failed(format!(
                "snapshot: failed to tag image {}: {}",
                image_ref, e
            )));
        }
        Ok(image_ref)
    }
}

impl DockerSandboxProvider {
    /// Capture the current state of `handle` as a content-addressed tarball.
    ///
    /// **Security contract (ADR-037 §4):** The caller (`SnapshotService`) is
    /// responsible for shredding `/etc/temps/credential-daemon.env` via
    /// `exec_as_root` **before** stopping the sandbox and calling this method.
    /// Any failure in that step must abort the snapshot at the service layer.
    ///
    /// This method then executes two additional scrubbing steps:
    ///
    /// 1. Inspect the stopped container's `Config.Env` and zero every
    ///    known-sensitive env-var value in the committed image config via
    ///    Docker's `--change "ENV KEY="` mechanism. Each sensitive key is set
    ///    to an empty value (`KEY=`) — this is the only mechanism the Docker
    ///    commit API actually supports; deletion is not possible. Verified
    ///    against a real Docker daemon. The `ContainerConfig` body's `env`
    ///    field (the previous approach) is silently ignored by the Docker
    ///    Engine and has no effect on the committed image — confirmed no-op.
    /// 2. Inspect the committed image's `Config.Env` and reject the snapshot —
    ///    removing the committed image and returning `SandboxExecFailed` — if
    ///    any known-sensitive key has a non-empty value (i.e. the zeroing
    ///    did not take effect for that key).
    ///
    /// The sandbox must be stopped by the caller before this is called for
    /// filesystem consistency.
    pub(super) async fn take_snapshot_inner(
        &self,
        handle: &SandboxHandle,
        _label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<super::SnapshotArtifact, AgentError> {
        let container_id = &handle.sandbox_id;

        // ── Step 1: collect and scrub known-sensitive env vars ────────────────
        let (workspace_source, container_env) =
            self.inspect_container_for_snapshot(container_id).await?;

        // ── Step 1b: commit the container with sensitive env vars zeroed ───────
        // The snapshot image tag is `temps-snapshot/<container_id_short>:latest`
        // during the commit phase; we rename it to the public_id tag after
        // digest verification. Using the container_id avoids a race if two
        // snapshots are taken concurrently.
        let short_id = &container_id[..container_id.len().min(12)];
        let commit_tag = format!("temps-snapshot-staging/{}", short_id);
        let committed_image_id = self
            .commit_scrubbed_container(container_id, &container_env, &commit_tag)
            .await?;

        // ── Step 3: verify no sensitive key survived in the committed image ───
        self.verify_committed_image_is_scrubbed(container_id, &committed_image_id)
            .await?;

        // ── Export to content-addressed tarball ───────────────────────────────
        let snapshots_dir = create_snapshots_dir(container_id).await?;
        let staged = StagedSnapshot {
            docker: self.docker.clone(),
            container_id: container_id.clone(),
            committed_image_id,
            commit_tag,
            // Stream the export through a Sha256 hasher while writing to a temp
            // file, then rename atomically. All file I/O uses tokio::fs so we
            // never block the async runtime on a potentially multi-GB write.
            tmp_path: snapshots_dir.join(format!(".tmp-{}-image", short_id)),
            workspace_tmp_path: snapshots_dir.join(format!(".tmp-{}-workspace", short_id)),
        };
        self.write_snapshot_artifacts(staged, &workspace_source, &snapshots_dir, max_size_bytes)
            .await
    }

    /// Step 1: the host directory mounted as the workspace, and the
    /// container's `Config.Env`.
    async fn inspect_container_for_snapshot(
        &self,
        container_id: &str,
    ) -> Result<(PathBuf, Vec<String>), AgentError> {
        // Inspect the stopped container's Config.Env to find injected vars.
        // The sensitive-pattern list and scrubbing helpers are module-level
        // functions (see `SENSITIVE_ENV_PATTERNS`, `scrub_env_vars`, and
        // `find_surviving_sensitive_keys`) so they are unit-testable without
        // a live Docker daemon.
        let container_inspect = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| {
                snapshot_failed(
                    container_id,
                    format!("snapshot: failed to inspect container: {}", e),
                )
            })?;

        let workspace_source = container_inspect
            .mounts
            .as_deref()
            .and_then(workspace_mount_source)
            .ok_or_else(|| {
                snapshot_failed(
                    container_id,
                    format!(
                        "snapshot: container has no host workspace mounted at '{}'",
                        CONTAINER_WORK_DIR
                    ),
                )
            })?;

        // Collect env vars set on the container (from the original create call
        // via ContainerCreateBody.env).
        let container_env: Vec<String> = container_inspect
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .map(|env| env.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        Ok((workspace_source, container_env))
    }

    /// Step 1b: commit the stopped container as `commit_tag:latest` with
    /// every sensitive env var zeroed, returning the committed image ID.
    async fn commit_scrubbed_container(
        &self,
        container_id: &str,
        container_env: &[String],
        commit_tag: &str,
    ) -> Result<String, AgentError> {
        // Build Dockerfile-style "ENV KEY=" change instructions for each
        // sensitive key, then count for telemetry.
        //
        // IMPORTANT: The ContainerConfig body's `env` field is NOT used for
        // scrubbing. Live testing against a real Docker daemon confirmed that
        // passing `ContainerConfig { env: Some(scrubbed_env), .. }` to
        // `commit_container` has zero effect — the Docker Engine silently
        // ignores/merges the body's Env field rather than replacing the
        // committed image's Config.Env. The `changes` query parameter (i.e.
        // `docker commit --change 'ENV KEY='`) is the only mechanism that
        // actually zeroes values in the committed image, verified directly.
        let scrub_changes = build_env_scrub_changes(container_env);
        let scrubbed_key_count = scrub_changes.len();

        tracing::info!(
            container_id = %container_id,
            commit_tag = %commit_tag,
            scrubbed_key_count = %scrubbed_key_count,
            "snapshot: committing container image"
        );

        // Pass `ENV KEY=` change instructions via the `changes` parameter —
        // this is Docker's `--change` flag, which is the only API mechanism
        // that actually overwrites env-var values in the committed image's
        // Config.Env. Each sensitive key is set to an empty value; Docker's
        // commit API cannot delete entries, only overwrite them.
        //
        // bollard's `CommitContainerOptionsBuilder::changes()` takes a single
        // `&str`; multiple Dockerfile instructions are separated by newlines.
        let changes_str = scrub_changes.join("\n");
        let mut commit_opts_builder =
            bollard::query_parameters::CommitContainerOptionsBuilder::new()
                .container(container_id)
                .repo(commit_tag)
                .tag("latest");
        if !changes_str.is_empty() {
            commit_opts_builder = commit_opts_builder.changes(&changes_str);
        }
        let commit_opts = commit_opts_builder.build();

        // The ContainerConfig body is passed as required by bollard's API
        // signature but left at defaults — its `env` field does nothing (the
        // Docker Engine ignores it; only the `changes` query parameter above
        // is effective, as confirmed against a live Docker daemon).
        let config_override = bollard::models::ContainerConfig::default();

        let commit_resp = self
            .docker
            .commit_container(commit_opts, config_override)
            .await
            .map_err(|e| {
                snapshot_failed(
                    container_id,
                    format!("snapshot: docker commit failed: {}", e),
                )
            })?;

        // IdResponse.id is a plain String (not Option<String>) in bollard 0.20.
        Ok(commit_resp.id)
    }

    /// Step 3: refuse the committed image if a sensitive env var survived
    /// with a value. The staged image is removed on every failure here.
    async fn verify_committed_image_is_scrubbed(
        &self,
        container_id: &str,
        committed_image_id: &str,
    ) -> Result<(), AgentError> {
        let committed_inspect = self
            .docker
            .inspect_image(committed_image_id)
            .await
            .map_err(|e| {
                // Clean up the staged image before returning the error
                spawn_remove_staged_image(self.docker.clone(), committed_image_id.to_string());
                snapshot_failed(
                    container_id,
                    format!(
                        "snapshot: failed to inspect committed image for scrub verification: {}",
                        e
                    ),
                )
            })?;

        let committed_env: Vec<String> = committed_inspect
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .map(|env| env.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        // Use the module-level helper to find any sensitive keys that survived.
        // An empty list means the image is clean; any key triggers an abort.
        let survivors = find_surviving_sensitive_keys(&committed_env);
        if let Some(key) = survivors.first() {
            // Scrub verification failed — remove the staged image and abort.
            spawn_remove_staged_image(self.docker.clone(), committed_image_id.to_string());
            return Err(snapshot_failed(
                container_id,
                format!(
                    "snapshot: scrub verification failed — sensitive key '{}' survived in committed image config; snapshot aborted and staged image removed",
                    key
                ),
            ));
        }
        Ok(())
    }

    /// Export the staged image and archive the workspace into
    /// `snapshots_dir`, publish both under their content digest, and tag the
    /// image with it. Every failure removes the staged image and the temp
    /// files, and any artifact this attempt published.
    async fn write_snapshot_artifacts(
        &self,
        staged: StagedSnapshot,
        workspace_source: &Path,
        snapshots_dir: &Path,
        max_size_bytes: u64,
    ) -> Result<super::SnapshotArtifact, AgentError> {
        let (image_digest, image_size_bytes) = staged.export_image_to_temp(max_size_bytes).await?;
        let (workspace_digest, workspace_size_bytes) = staged
            .archive_workspace_to_temp(
                workspace_source,
                max_size_bytes.saturating_sub(image_size_bytes),
                max_size_bytes,
            )
            .await?;
        let published = staged
            .publish(snapshots_dir, &image_digest, &workspace_digest)
            .await?;
        let size_bytes = image_size_bytes.saturating_add(workspace_size_bytes);
        let image_ref = staged.tag_with_digest(&published).await?;

        // Remove the staging tag now that the canonical tag is in place.
        let _ = self
            .docker
            .remove_image(
                &format!("{}:latest", staged.commit_tag),
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .noprune(true) // keep the layers — only untag
                        .build(),
                ),
                None,
            )
            .await;

        tracing::info!(
            container_id = %staged.container_id,
            digest = %published.digest_hex,
            size_bytes = %size_bytes,
            path = %published.final_path.display(),
            image_ref = %image_ref,
            workspace_path = %published.workspace_final_path.display(),
            "snapshot: completed successfully"
        );

        Ok(super::SnapshotArtifact {
            content_path: published.final_path,
            content_digest: published.digest_hex,
            primary_digest: image_digest,
            size_bytes,
            backend: super::SandboxBackend::Docker,
            image_ref: Some(image_ref),
            image_id: Some(staged.committed_image_id),
            workspace: Some(super::SnapshotCompanionArtifact {
                content_path: published.workspace_final_path,
                content_digest: workspace_digest,
                size_bytes: workspace_size_bytes,
            }),
        })
    }

    /// Create a new sandbox seeded from a snapshot artifact.
    ///
    /// Ensures the snapshot image is present in the Docker daemon (loading
    /// from the tarball if the tag is absent or stale), then delegates to
    /// `create` with the image override set to `artifact.image_ref`.
    pub(super) async fn create_from_snapshot_inner(
        &self,
        artifact: &super::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        config.validate_allow_docker(super::SandboxBackend::Docker)?;
        let image_ref =
            artifact
                .image_ref
                .as_deref()
                .ok_or_else(|| AgentError::SandboxExecFailed {
                    run_id: config.run_id,
                    sandbox_id: String::new(),
                    reason: "snapshot: artifact has no image_ref (non-Docker snapshot?)"
                        .to_string(),
                })?;

        let primary_path = artifact.content_path.clone();
        let expected_primary_digest = artifact.primary_digest.clone();
        let (actual_primary_digest, _) =
            tokio::task::spawn_blocking(move || hash_file(&primary_path))
                .await
                .map_err(|e| AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!("verify snapshot image task failed: {}", e),
                })?
                .map_err(|e| AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!(
                        "verify snapshot image artifact '{}': {}",
                        artifact.content_path.display(),
                        e
                    ),
                })?;
        if actual_primary_digest != expected_primary_digest {
            return Err(AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: format!(
                    "snapshot image digest mismatch: expected {}, got {}",
                    expected_primary_digest, actual_primary_digest
                ),
            });
        }
        if let Some(workspace) = &artifact.workspace {
            let workspace_path = workspace.content_path.clone();
            let workspace_path_for_hash = workspace_path.clone();
            let expected_workspace_digest = workspace.content_digest.clone();
            let logical_snapshot_digest = artifact.content_digest.clone();
            let (actual_workspace_digest, _) =
                tokio::task::spawn_blocking(move || hash_file(&workspace_path_for_hash))
                    .await
                    .map_err(|e| AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".to_string(),
                        reason: format!(
                            "verify workspace artifact '{}' for logical snapshot '{}' \
                             task failed: {}",
                            workspace_path.display(),
                            logical_snapshot_digest,
                            e
                        ),
                    })?
                    .map_err(|e| AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".to_string(),
                        reason: format!(
                            "verify workspace artifact '{}' for logical snapshot '{}': {}",
                            workspace_path.display(),
                            logical_snapshot_digest,
                            e
                        ),
                    })?;
            if actual_workspace_digest != expected_workspace_digest {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!(
                        "workspace artifact '{}' for logical snapshot '{}' has digest mismatch: expected {}, got {}",
                        workspace_path.display(),
                        logical_snapshot_digest,
                        expected_workspace_digest,
                        actual_workspace_digest
                    ),
                });
            }
            let actual_combined =
                combined_snapshot_digest(&actual_primary_digest, &actual_workspace_digest);
            if actual_combined != artifact.content_digest {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!(
                        "combined snapshot digest mismatch: expected {}, got {}",
                        artifact.content_digest, actual_combined
                    ),
                });
            }
        }

        // Generic restores reproduce the snapshot image. Managed application
        // restores pass an approved runtime image and restore only workspace
        // data; importing or executing the snapshot image would let an
        // unrelated custom-image snapshot cross the trusted-runtime boundary.
        if config.image.is_none() {
            let expected_image_id = artifact.image_id.as_deref().ok_or_else(|| {
            AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: "snapshot metadata has no immutable Docker image ID; refusing to trust a mutable image tag"
                    .to_string(),
            }
        })?;

            // A Docker tag is mutable. It is usable only when it still resolves to
            // the immutable image ID captured with this snapshot.
            let image_present = match self.docker.inspect_image(image_ref).await {
                Ok(inspect) => inspect.id.as_deref() == Some(expected_image_id),
                Err(_) => false,
            };

            if !image_present {
                // Load from the tarball. `docker load` imports all tags embedded
                // in the tarball — the image will re-appear under `image_ref`.
                tracing::info!(
                    image_ref = %image_ref,
                    path = %artifact.content_path.display(),
                    "snapshot: image not in daemon — loading from tarball"
                );

                // Hard cap: reject tarballs larger than 20 GiB before reading
                // anything into memory. This prevents an unbounded Vec allocation
                // on a 4 GB reference host. The cap is intentionally generous
                // (a typical sandbox image is 2–4 GB) while still bounding risk.
                const MAX_SNAPSHOT_BYTES: u64 = 20 * 1024 * 1024 * 1024; // 20 GiB

                let path = artifact.content_path.clone();
                let file_size = tokio::fs::metadata(&path)
                    .await
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: config.run_id,
                        sandbox_id: String::new(),
                        reason: format!("snapshot: failed to stat tarball: {}", e),
                    })?
                    .len();

                if file_size > MAX_SNAPSHOT_BYTES {
                    return Err(AgentError::SandboxExecFailed {
                        run_id: config.run_id,
                        sandbox_id: String::new(),
                        reason: format!(
                            "snapshot: tarball too large to import: {} bytes exceeds \
                         maximum {} bytes (20 GiB); \
                         contact support if you need to restore a larger snapshot",
                            file_size, MAX_SNAPSHOT_BYTES,
                        ),
                    });
                }

                let file = tokio::fs::File::open(&path).await.map_err(|e| {
                    AgentError::SandboxExecFailed {
                        run_id: config.run_id,
                        sandbox_id: String::new(),
                        reason: format!(
                            "snapshot: failed to open tarball {}: {}",
                            artifact.content_path.display(),
                            e
                        ),
                    }
                })?;
                let file_stream = futures::stream::try_unfold(file, |mut file| async move {
                    use tokio::io::AsyncReadExt;
                    let mut chunk = vec![0u8; 1024 * 1024];
                    let read = file.read(&mut chunk).await?;
                    if read == 0 {
                        Ok::<Option<(bytes::Bytes, tokio::fs::File)>, std::io::Error>(None)
                    } else {
                        chunk.truncate(read);
                        Ok::<Option<(bytes::Bytes, tokio::fs::File)>, std::io::Error>(Some((
                            bytes::Bytes::from(chunk),
                            file,
                        )))
                    }
                });

                let mut load_stream = self.docker.import_image_stream(
                    bollard::query_parameters::ImportImageOptionsBuilder::new()
                        .quiet(true)
                        .build(),
                    file_stream,
                    None,
                );

                while let Some(result) = futures::StreamExt::next(&mut load_stream).await {
                    match result {
                        Ok(info) => {
                            if let Some(ref detail) = info.error_detail {
                                let msg = detail
                                    .message
                                    .as_deref()
                                    .unwrap_or("unknown docker load error");
                                return Err(AgentError::SandboxExecFailed {
                                    run_id: config.run_id,
                                    sandbox_id: String::new(),
                                    reason: format!("snapshot: docker load error: {}", msg),
                                });
                            }
                        }
                        Err(e) => {
                            return Err(AgentError::SandboxExecFailed {
                                run_id: config.run_id,
                                sandbox_id: String::new(),
                                reason: format!("snapshot: docker load stream error: {}", e),
                            });
                        }
                    }
                }

                self.docker
                    .inspect_image(expected_image_id)
                    .await
                    .map_err(|e| AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".to_string(),
                        reason: format!(
                            "verified snapshot imported without expected image ID '{}': {}",
                            expected_image_id, e
                        ),
                    })?;
                self.docker
                    .tag_image(
                        expected_image_id,
                        Some(
                            bollard::query_parameters::TagImageOptionsBuilder::new()
                                .repo(image_ref.split(':').next().unwrap_or(image_ref))
                                .tag(image_ref.rsplit(':').next().unwrap_or("latest"))
                                .build(),
                        ),
                    )
                    .await
                    .map_err(|e| AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".to_string(),
                        reason: format!(
                            "tag imported snapshot image '{}' as '{}': {}",
                            expected_image_id, image_ref, e
                        ),
                    })?;

                tracing::info!(
                    image_ref = %image_ref,
                    "snapshot: image loaded from tarball"
                );
            }
        }

        let mut config = config;
        if let Some(workspace) = artifact.workspace.clone() {
            let destination = config.host_work_dir.clone();
            tokio::task::spawn_blocking(move || {
                restore_workspace_archive(&workspace, &destination)
            })
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: format!("restore workspace archive task failed: {}", e),
            })?
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: format!(
                    "restore workspace archive into '{}': {}",
                    config.host_work_dir.display(),
                    e
                ),
            })?;
        }

        // A caller-provided image is an explicit trust boundary (managed
        // application workspaces use it). Generic restores have no override
        // and continue reproducing the snapshot image.
        if config.image.is_none() {
            config.image = Some(image_ref.to_string());
        }
        self.create(config).await
    }
}

#[cfg(test)]
mod tests {
    //! The steps of `take_snapshot_inner` against a fake Engine API,
    //! without the sandbox image or a Docker daemon. Once the container is
    //! committed, every failing step must remove the staged image and the
    //! temp files, and any artifact this attempt published.

    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;
    use temps_docker_capture::{Call, FakeDocker, Method};

    const CONTAINER: &str = "fake-snapshot-container-0001";
    const IMAGE_ID: &str = "fake-committed-image";
    /// What the fake streams as the image export: a JSON string, i.e. 19
    /// bytes including the quotes.
    const EXPORTED: &str = "image-layer-bytes";
    const EXPORTED_LEN: u64 = EXPORTED.len() as u64 + 2;

    async fn fake_provider() -> (FakeDocker, DockerSandboxProvider) {
        let fake = FakeDocker::start()
            .await
            .expect("start the fake Engine API");
        let provider =
            DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default());
        (fake, provider)
    }

    fn handle() -> SandboxHandle {
        SandboxHandle {
            node_id: None,
            sandbox_id: CONTAINER.to_string(),
            sandbox_name: "temps-sandbox-snapshot-steps".to_string(),
            work_dir: PathBuf::from(CONTAINER_WORK_DIR),
            backend: super::super::SandboxBackend::Docker,
            image: String::new(),
            conditions: Vec::new(),
        }
    }

    fn container_inspect(fake: &FakeDocker, mounts: Value, env: &[&str]) {
        fake.respond(
            Method::GET,
            &format!("/containers/{CONTAINER}/json"),
            200,
            json!({ "Id": CONTAINER, "Mounts": mounts, "Config": { "Env": env } }),
        );
    }

    fn workspace_mount(workspace: &Path) -> Value {
        json!([{ "Type": "bind", "Source": workspace, "Destination": CONTAINER_WORK_DIR }])
    }

    fn commit_succeeds(fake: &FakeDocker) {
        fake.respond(Method::POST, "/commit", 201, json!({ "Id": IMAGE_ID }));
    }

    fn committed_image_env(fake: &FakeDocker, env: &[&str]) {
        fake.respond(
            Method::GET,
            &format!("/images/{IMAGE_ID}/json"),
            200,
            json!({ "Id": IMAGE_ID, "Config": { "Env": env } }),
        );
    }

    fn export_succeeds(fake: &FakeDocker) {
        fake.respond(
            Method::GET,
            &format!("/images/{IMAGE_ID}/get"),
            200,
            json!(EXPORTED),
        );
    }

    fn tag_succeeds(fake: &FakeDocker) {
        fake.respond(
            Method::POST,
            &format!("/images/{IMAGE_ID}/tag"),
            201,
            Value::Null,
        );
    }

    fn staged(fake: &FakeDocker, snapshots_dir: &Path) -> StagedSnapshot {
        StagedSnapshot {
            docker: Arc::new(fake.docker()),
            container_id: CONTAINER.to_string(),
            committed_image_id: IMAGE_ID.to_string(),
            commit_tag: "temps-snapshot-staging/fake-snapsho".to_string(),
            tmp_path: snapshots_dir.join(".tmp-fake-snapsho-image"),
            workspace_tmp_path: snapshots_dir.join(".tmp-fake-snapsho-workspace"),
        }
    }

    fn workspace_with(contents: &[u8]) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("workspace dir");
        std::fs::write(workspace.path().join("main.rs"), contents).expect("workspace file");
        workspace
    }

    fn describe(calls: &[Call]) -> String {
        calls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn removes_staged_image(call: &Call) -> bool {
        call.method == "DELETE"
            && call.path == format!("/images/{IMAGE_ID}")
            && call.query.contains("force=true")
    }

    /// Cleanup runs in a spawned task: wait for it, bounded.
    async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..250 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn assert_staged_snapshot_cleaned_up(fake: &FakeDocker, staged_paths: [PathBuf; 2]) {
        eventually("the staged image removal", || {
            fake.calls().iter().any(removes_staged_image)
        })
        .await;
        eventually("the temp files removal", || {
            staged_paths.iter().all(|path| !path.exists())
        })
        .await;
    }

    fn snapshot_failure(error: AgentError) -> String {
        match error {
            AgentError::SandboxExecFailed {
                sandbox_id, reason, ..
            } => {
                assert_eq!(sandbox_id, CONTAINER);
                reason
            }
            other => panic!("expected SandboxExecFailed, got {other:?}"),
        }
    }

    fn published_tars(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read snapshots dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_container_without_a_workspace_mount_is_never_committed() {
        let (fake, provider) = fake_provider().await;
        container_inspect(&fake, json!([]), &[]);

        let error = provider
            .take_snapshot_inner(&handle(), None, 1024)
            .await
            .expect_err("no workspace mount");
        let reason = snapshot_failure(error);
        assert!(
            reason.contains("container has no host workspace mounted at '/home/temps/workspace'"),
            "{reason}"
        );
        assert!(!fake.calls().iter().any(|call| call.path == "/commit"));
    }

    #[tokio::test]
    async fn a_failed_commit_stages_nothing_to_clean_up() {
        let (fake, provider) = fake_provider().await;
        let workspace = workspace_with(b"fn main() {}");
        container_inspect(
            &fake,
            workspace_mount(workspace.path()),
            &["GITHUB_TOKEN=ghp-test-value", "PATH=/usr/bin"],
        );

        let error = provider
            .take_snapshot_inner(&handle(), None, 1024)
            .await
            .expect_err("the fake has no commit endpoint");
        let reason = snapshot_failure(error);
        assert!(
            reason.starts_with("snapshot: docker commit failed:"),
            "{reason}"
        );

        let calls = fake.calls();
        let commit = calls
            .iter()
            .find(|call| call.method == "POST" && call.path == "/commit")
            .unwrap_or_else(|| panic!("no commit in:\n{}", describe(&calls)));
        // Only the sensitive key is zeroed, under the staging tag.
        assert!(commit.query.contains("GITHUB_TOKEN"), "{commit}");
        assert!(!commit.query.contains("PATH"), "{commit}");
        assert!(commit.query.contains("fake-snapsho"), "{commit}");
        assert!(
            !calls.iter().any(|call| call.method == "DELETE"),
            "{}",
            describe(&calls)
        );
    }

    #[tokio::test]
    async fn a_committed_image_that_cannot_be_inspected_is_removed() {
        let (fake, provider) = fake_provider().await;
        let workspace = workspace_with(b"fn main() {}");
        container_inspect(&fake, workspace_mount(workspace.path()), &[]);
        commit_succeeds(&fake);

        let error = provider
            .take_snapshot_inner(&handle(), None, 1024)
            .await
            .expect_err("the committed image cannot be inspected");
        let reason = snapshot_failure(error);
        assert!(
            reason
                .starts_with("snapshot: failed to inspect committed image for scrub verification:"),
            "{reason}"
        );
        eventually("the staged image removal", || {
            fake.calls().iter().any(removes_staged_image)
        })
        .await;
    }

    #[tokio::test]
    async fn a_secret_that_survives_the_commit_aborts_and_removes_the_image() {
        let (fake, provider) = fake_provider().await;
        let workspace = workspace_with(b"fn main() {}");
        container_inspect(&fake, workspace_mount(workspace.path()), &[]);
        commit_succeeds(&fake);
        committed_image_env(&fake, &["PATH=/usr/bin", "OPENAI_API_KEY=sk-test-value"]);

        let error = provider
            .take_snapshot_inner(&handle(), None, 1024)
            .await
            .expect_err("a secret survived");
        let reason = snapshot_failure(error);
        assert!(
            reason.contains("sensitive key 'OPENAI_API_KEY' survived"),
            "{reason}"
        );
        assert!(!reason.contains("sk-test-value"), "{reason}");
        eventually("the staged image removal", || {
            fake.calls().iter().any(removes_staged_image)
        })
        .await;
    }

    #[tokio::test]
    async fn a_snapshot_publishes_both_artifacts_under_their_digest() {
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        tag_succeeds(&fake);
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(b"fn main() {}");

        let artifact = provider
            .write_snapshot_artifacts(
                staged(&fake, snapshots.path()),
                workspace.path(),
                snapshots.path(),
                1024 * 1024,
            )
            .await
            .expect("snapshot");

        let (image_digest, image_size) = hash_file(&artifact.content_path).expect("image");
        assert_eq!(image_digest, artifact.primary_digest);
        assert_eq!(image_size, EXPORTED_LEN);
        let workspace_artifact = artifact.workspace.expect("workspace artifact");
        let (workspace_digest, workspace_size) =
            hash_file(&workspace_artifact.content_path).expect("workspace");
        assert_eq!(workspace_digest, workspace_artifact.content_digest);
        assert_eq!(workspace_size, workspace_artifact.size_bytes);
        assert_eq!(
            artifact.content_digest,
            combined_snapshot_digest(&image_digest, &workspace_digest)
        );
        assert_eq!(
            artifact.content_path,
            snapshots
                .path()
                .join(format!("{}.tar", artifact.content_digest))
        );
        assert_eq!(artifact.size_bytes, image_size + workspace_size);
        assert_eq!(
            artifact.image_ref,
            Some(format!("temps-snapshot/{}:latest", artifact.content_digest))
        );
        assert_eq!(artifact.image_id.as_deref(), Some(IMAGE_ID));
        // Only the two published files remain.
        assert_eq!(published_tars(snapshots.path()).len(), 2);
        assert!(!snapshots.path().join(".tmp-fake-snapsho-image").exists());

        let calls = fake.calls();
        // The staging tag is removed without pruning, the image is kept.
        assert!(
            calls.iter().any(|call| call.method == "DELETE"
                && call.path.contains("temps-snapshot-staging")
                && call.query.contains("noprune=true")),
            "{}",
            describe(&calls)
        );
        assert!(
            !calls.iter().any(removes_staged_image),
            "{}",
            describe(&calls)
        );
    }

    #[tokio::test]
    async fn an_image_export_over_the_size_limit_is_cleaned_up() {
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(b"fn main() {}");
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];

        let error = provider
            .write_snapshot_artifacts(staged, workspace.path(), snapshots.path(), 4)
            .await
            .expect_err("the export exceeds the limit");
        match error {
            AgentError::SnapshotSizeLimitExceeded {
                sandbox_id,
                stage,
                max_size_bytes,
            } => {
                assert_eq!(sandbox_id, CONTAINER);
                assert_eq!(stage, "exporting the Docker image");
                assert_eq!(max_size_bytes, 4);
            }
            other => panic!("expected SnapshotSizeLimitExceeded, got {other:?}"),
        }
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
        assert!(published_tars(snapshots.path()).is_empty());
    }

    #[tokio::test]
    async fn a_failed_image_export_is_cleaned_up() {
        let (fake, provider) = fake_provider().await;
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(b"fn main() {}");
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];

        let error = provider
            .write_snapshot_artifacts(staged, workspace.path(), snapshots.path(), 1024)
            .await
            .expect_err("the fake has no export endpoint");
        let reason = snapshot_failure(error);
        assert!(
            reason.starts_with("snapshot: image export stream error:"),
            "{reason}"
        );
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
    }

    #[tokio::test]
    async fn a_workspace_over_the_remaining_limit_is_cleaned_up() {
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(&[b'x'; 64 * 1024]);
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];

        let error = provider
            .write_snapshot_artifacts(
                staged,
                workspace.path(),
                snapshots.path(),
                EXPORTED_LEN + 1024,
            )
            .await
            .expect_err("the workspace exceeds what the image left");
        match error {
            AgentError::SnapshotSizeLimitExceeded { stage, .. } => {
                assert_eq!(stage, "archiving the Docker workspace");
            }
            other => panic!("expected SnapshotSizeLimitExceeded, got {other:?}"),
        }
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
        assert!(published_tars(snapshots.path()).is_empty());
    }

    #[tokio::test]
    async fn a_workspace_that_cannot_be_archived_is_named() {
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let missing = snapshots.path().join("no-such-workspace");
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];

        let error = provider
            .write_snapshot_artifacts(staged, &missing, snapshots.path(), 1024 * 1024)
            .await
            .expect_err("the workspace does not exist");
        let reason = snapshot_failure(error);
        assert!(
            reason.starts_with(&format!(
                "snapshot: failed to archive workspace '{}':",
                missing.display()
            )),
            "{reason}"
        );
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
    }

    #[tokio::test]
    async fn a_failed_tag_removes_the_artifacts_this_attempt_published() {
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(b"fn main() {}");
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];

        let error = provider
            .write_snapshot_artifacts(staged, workspace.path(), snapshots.path(), 1024 * 1024)
            .await
            .expect_err("the fake has no tag endpoint");
        let reason = snapshot_failure(error);
        assert!(
            reason.starts_with("snapshot: failed to tag image temps-snapshot/"),
            "{reason}"
        );
        // Removed before the error returns, not by the background cleanup.
        assert!(published_tars(snapshots.path()).is_empty());
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
    }

    #[tokio::test]
    async fn a_failed_tag_keeps_the_artifacts_an_earlier_snapshot_published() {
        let snapshots = tempfile::tempdir().expect("snapshots dir");
        let workspace = workspace_with(b"fn main() {}");
        let (earlier_fake, earlier) = fake_provider().await;
        export_succeeds(&earlier_fake);
        tag_succeeds(&earlier_fake);
        let first = earlier
            .write_snapshot_artifacts(
                staged(&earlier_fake, snapshots.path()),
                workspace.path(),
                snapshots.path(),
                1024 * 1024,
            )
            .await
            .expect("the earlier snapshot");
        let before = published_tars(snapshots.path());
        assert_eq!(before.len(), 2);

        // Byte-identical content dedups onto the same files, and this
        // attempt's tag failure must not delete what it did not create.
        let (fake, provider) = fake_provider().await;
        export_succeeds(&fake);
        let staged = staged(&fake, snapshots.path());
        let paths = [staged.tmp_path.clone(), staged.workspace_tmp_path.clone()];
        let error = provider
            .write_snapshot_artifacts(staged, workspace.path(), snapshots.path(), 1024 * 1024)
            .await
            .expect_err("the fake has no tag endpoint");
        let reason = snapshot_failure(error);
        assert!(reason.contains(&first.content_digest), "{reason}");
        assert_eq!(published_tars(snapshots.path()), before);
        assert_staged_snapshot_cleaned_up(&fake, paths).await;
    }
}
