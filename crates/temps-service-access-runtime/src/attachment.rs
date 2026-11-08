// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-sandbox host attachment. Mount only `directory()` read-only into the
//! owning sandbox; never mount the parent or use a shared TCP listener as an
//! identity boundary. The caller must resolve the actual runtime incarnation.
use crate::{RuntimeError, SandboxBinding, MAX_SANDBOX_CONNECTIONS};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokenless_dataplane::CutReport;
use tokio::{
    net::UnixListener,
    task::{JoinHandle, JoinSet},
};

/// Owns its endpoint and all forwarding tasks. Drop cuts access even when the
/// lifecycle operation creating it is cancelled or fails after attachment.
pub struct UnixAttachment {
    binding: Arc<SandboxBinding>,
    task: JoinHandle<()>,
    directory: TempDir,
}

impl UnixAttachment {
    /// `parent` is an operator-owned directory outside every sandbox mount.
    /// It must not be writable by a sandbox user. A fresh, unpredictable child
    /// is created on every invocation; existing paths are never replaced.
    pub fn bind(parent: &Path, binding: Arc<SandboxBinding>) -> Result<Self, RuntimeError> {
        let io_error = |source| RuntimeError::Attachment {
            binding: binding.id().into(),
            operation: "create Unix attachment",
            source,
        };
        let directory = tempfile::Builder::new()
            .prefix("access-")
            .tempdir_in(parent)
            .map_err(io_error)?;
        // The parent remains private to the worker. Only this child is exposed
        // through a read-only mount; nobody can replace the socket or add files.
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .map_err(io_error)?;
        let path = directory.path().join("proxy.sock");
        let listener = UnixListener::bind(&path).map_err(io_error)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .map_err(io_error)?;
        let task_binding = Arc::clone(&binding);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (stream, _) = match accepted {
                            Ok(accepted) => accepted,
                            Err(_) => {
                                tracing::warn!(binding = task_binding.id(), "Sandbox proxy attachment stopped accepting connections");
                                task_binding.revoke();
                                break;
                            }
                        };
                        // Bound tasks before spawn; the SDK also enforces the
                        // configured per-binding and aggregate worker limits.
                        if connections.len() >= MAX_SANDBOX_CONNECTIONS {
                            drop(stream);
                            continue;
                        }
                        let serving = Arc::clone(&task_binding);
                        connections.spawn(async move {
                            if serving.serve(stream).await.is_err() {
                                // Never format SDK sources: they can contain
                                // request metadata controlled by the guest.
                                tracing::debug!(binding = serving.id(), "Sandbox proxy connection refused or closed");
                            }
                        });
                    }
                }
            }
            // JoinSet aborts every owned connection when the listener exits.
        });
        Ok(Self {
            binding,
            task,
            directory,
        })
    }

    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Stops admission, cuts all traffic, then returns the enforcement receipt.
    /// A timeout is an error, never a successful revocation acknowledgement.
    pub async fn revoke_and_wait(&self, timeout: Duration) -> Result<CutReport, RuntimeError> {
        self.task.abort();
        self.binding.revoke_and_wait(timeout).await
    }
}

impl Drop for UnixAttachment {
    fn drop(&mut self) {
        self.binding.revoke();
        self.task.abort();
        // TempDir removes only the uniquely owned endpoint directory.
    }
}
