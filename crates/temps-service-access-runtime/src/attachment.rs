// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-sandbox host attachment. Mount only `directory()` read-only into the
//! owning sandbox; never mount the parent or use a shared TCP listener as an
//! identity boundary. The caller must resolve the actual runtime incarnation.
use crate::{RuntimeError, SandboxBinding, MAX_SANDBOX_CONNECTIONS};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};
use tempfile::TempDir;
use tokenless_dataplane::CutReport;
use tokio::{
    net::UnixListener,
    task::{JoinHandle, JoinSet},
};

/// Owns its endpoint and all forwarding tasks. Drop cuts access even when the
/// lifecycle operation creating it is cancelled or fails after attachment.
pub struct UnixAttachment {
    slot: Arc<AttachmentSlot>,
    task: JoinHandle<()>,
    directory: TempDir,
}

struct AttachmentSlot {
    id: String,
    binding: OnceLock<Arc<SandboxBinding>>,
    closed: AtomicBool,
}

impl AttachmentSlot {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(binding) = self.binding.get() {
            binding.revoke();
        }
    }
}

impl UnixAttachment {
    /// `parent` is an operator-owned directory outside every sandbox mount.
    /// It must not be writable by a sandbox user. A fresh, unpredictable child
    /// is created on every invocation; existing paths are never replaced.
    pub fn bind(parent: &Path, binding: Arc<SandboxBinding>) -> Result<Self, RuntimeError> {
        let attachment = Self::reserve(parent, binding.id().into())?;
        attachment.activate(binding)?;
        Ok(attachment)
    }

    /// Reserve the mount before container creation. An ungranted attachment
    /// closes connections immediately. `id` must come from the host's fresh
    /// sandbox incarnation, never a guest request or a reusable display name.
    pub fn reserve(parent: &Path, id: String) -> Result<Self, RuntimeError> {
        if id.is_empty() || id.len() > 256 {
            return Err(RuntimeError::Invalid {
                operation: "reserve attachment",
                binding: "unassigned".into(),
                reason: "binding identity must contain 1 to 256 bytes",
            });
        }
        let slot = Arc::new(AttachmentSlot {
            id,
            binding: OnceLock::new(),
            closed: AtomicBool::new(false),
        });
        let io_error = |source| RuntimeError::Attachment {
            binding: slot.id.clone(),
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
        let task_slot = Arc::clone(&slot);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (stream, _) = match accepted {
                            Ok(accepted) => accepted,
                            Err(_) => {
                                tracing::warn!(binding = task_slot.id, "Sandbox proxy attachment stopped accepting connections");
                                task_slot.close();
                                break;
                            }
                        };
                        // Bound tasks before spawn; the SDK also enforces the
                        // configured per-binding and aggregate worker limits.
                        if connections.len() >= MAX_SANDBOX_CONNECTIONS {
                            drop(stream);
                            continue;
                        }
                        if task_slot.closed.load(Ordering::SeqCst) {
                            drop(stream);
                            continue;
                        }
                        let Some(serving) = task_slot.binding.get().cloned() else {
                            drop(stream);
                            continue;
                        };
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
            slot,
            task,
            directory,
        })
    }

    /// Activate once for the reserved identity. Changes to grants use the
    /// worker's version-fenced binding updates, never replacement identities.
    pub fn activate(&self, binding: Arc<SandboxBinding>) -> Result<(), RuntimeError> {
        let invalid = |reason| RuntimeError::Invalid {
            operation: "activate attachment",
            binding: self.slot.id.clone(),
            reason,
        };
        if self.slot.closed.load(Ordering::SeqCst) {
            return Err(invalid("attachment is permanently closed"));
        }
        if binding.id() != self.slot.id {
            return Err(invalid(
                "binding does not match the reserved sandbox incarnation",
            ));
        }
        if let Some(current) = self.slot.binding.get() {
            return if Arc::ptr_eq(current, &binding) {
                Ok(())
            } else {
                Err(invalid("attachment already has a different binding"))
            };
        }
        self.slot
            .binding
            .set(Arc::clone(&binding))
            .map_err(|_| invalid("attachment was activated concurrently"))?;
        // Close and activation can race. A binding installed after close's
        // lookup is revoked here; admission also checks closed before serving.
        if self.slot.closed.load(Ordering::SeqCst) {
            binding.revoke();
            return Err(invalid("attachment closed during activation"));
        }
        Ok(())
    }

    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Stops admission, cuts all traffic, then returns the enforcement receipt.
    /// A timeout is an error, never a successful revocation acknowledgement.
    pub async fn revoke_and_wait(&self, timeout: Duration) -> Result<CutReport, RuntimeError> {
        self.slot.close();
        self.task.abort();
        match self.slot.binding.get() {
            Some(binding) => binding.revoke_and_wait(timeout).await,
            None => Ok(CutReport {
                snapshot_version: None,
                connections_closed: 0,
                waited: Duration::ZERO,
            }),
        }
    }
}

impl Drop for UnixAttachment {
    fn drop(&mut self) {
        self.slot.close();
        self.task.abort();
        // TempDir removes only the uniquely owned endpoint directory.
    }
}
