// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A persistent mount inode with a private host-side lease. Only `guest_path`
//! is exposed read-only to a container. The outer directory and lease file must
//! stay outside every guest mount. Recovery confers no forwarding authority.
use crate::RuntimeError;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use uuid::Uuid;

pub struct MountDirectory {
    id: String,
    outer: PathBuf,
    guest: PathBuf,
    // Held through endpoint shutdown and cleanup, never exposed to the guest.
    _lease: File,
    // A failed create cleans itself up. Once a container owns the mount,
    // retain() transfers cleanup responsibility to its destroy lifecycle.
    temporary: Option<TempDir>,
}

fn failure(id: &str, operation: &'static str, source: std::io::Error) -> RuntimeError {
    RuntimeError::Attachment {
        binding: id.into(),
        operation,
        source,
    }
}

fn invalid(id: &str, reason: &'static str) -> RuntimeError {
    RuntimeError::Invalid {
        operation: "open sandbox proxy mount",
        binding: id.into(),
        reason,
    }
}

fn private_directory(path: &Path, id: &str, owner: Option<u32>) -> Result<u32, RuntimeError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| failure(id, "inspect host directory", e))?;
    if !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
        || owner.is_some_and(|uid| uid != metadata.uid())
    {
        return Err(invalid(
            id,
            "host directories must be private, owned by the host and not symlinks",
        ));
    }
    Ok(metadata.uid())
}

impl MountDirectory {
    /// `root` is an existing private directory under the worker's data root.
    /// New directories belong to this invocation and are removed on failure.
    pub fn create(root: &Path) -> Result<Self, RuntimeError> {
        let owner = private_directory(root, "new", None)?;
        let id = Uuid::new_v4().simple().to_string();
        let temporary = tempfile::Builder::new()
            .prefix(&format!("{id}-"))
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .map_err(|e| failure(&id, "allocate host directory", e))?;
        // The random TempDir suffix is part of the opaque mount ID. Never use
        // a sandbox display name or a path supplied by a guest.
        let id = temporary
            .path()
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid(&id, "allocated mount name is not UTF-8"))?
            .to_string();
        let outer = temporary.path().to_path_buf();
        private_directory(&outer, &id, Some(owner))?;
        let guest = outer.join("guest");
        fs::create_dir(&guest).map_err(|e| failure(&id, "create guest mount directory", e))?;
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o755))
            .map_err(|e| failure(&id, "set guest mount permissions", e))?;
        let lease = Self::lease(&outer, &id, true)?;
        Ok(Self {
            id,
            outer,
            guest,
            _lease: lease,
            temporary: Some(temporary),
        })
    }

    /// Reopen an already-owned mount after a worker restart or sandbox start.
    /// Caller must obtain `id` from its managed container's host metadata and
    /// verify that container mounts exactly `guest_path()` read-only.
    pub fn recover(root: &Path, id: &str) -> Result<Self, RuntimeError> {
        let (uuid, suffix) = id
            .split_once('-')
            .ok_or_else(|| invalid("unassigned", "invalid mount ID"))?;
        if uuid.len() != 32
            || Uuid::parse_str(uuid).is_err()
            || suffix.is_empty()
            || suffix.len() > 32
            || !suffix.bytes().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(invalid("unassigned", "invalid mount ID"));
        }
        // TempDir creation makes relative roots absolute. Recovery must use
        // the same lexical resolution so Docker's absolute source still matches.
        let root = std::path::absolute(root).map_err(|e| failure(id, "resolve mount root", e))?;
        let owner = private_directory(&root, id, None)?;
        let outer = root.join(id);
        private_directory(&outer, id, Some(owner))?;
        let guest = outer.join("guest");
        let metadata =
            fs::symlink_metadata(&guest).map_err(|e| failure(id, "inspect guest directory", e))?;
        if !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(invalid(id,"guest mount must be a host-owned directory without group or other write permission"));
        }
        // Recovery must not fabricate a missing lease/ownership marker.
        let lease = Self::lease(&outer, id, false)?;
        Ok(Self {
            id: id.into(),
            outer,
            guest,
            _lease: lease,
            temporary: None,
        })
    }

    fn lease(outer: &Path, id: &str, create: bool) -> Result<File, RuntimeError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(outer.join("lease"))
            .map_err(|e| failure(id, "open mount lease", e))?;
        let metadata = file
            .metadata()
            .map_err(|e| failure(id, "inspect mount lease", e))?;
        let owner = fs::symlink_metadata(outer)
            .map_err(|e| failure(id, "inspect lease owner", e))?
            .uid();
        if !metadata.is_file()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(invalid(
                id,
                "mount lease must be a private host-owned regular file",
            ));
        }
        file.try_lock()
            .map_err(|e| failure(id, "exclusively claim mount", std::io::Error::other(e)))?;
        Ok(file)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn guest_path(&self) -> &Path {
        &self.guest
    }

    /// Call only once the provider has created a container owning this mount.
    /// Dropping a retained mount stops its listener but preserves its inode.
    pub fn retain(&mut self) {
        if let Some(directory) = self.temporary.take() {
            let _ = directory.keep();
        }
    }

    /// Called with the lease held, before replacing an endpoint after restart.
    /// Unexpected regular files/symlinks are errors, never silently overwritten.
    pub(crate) fn clear_socket(&self) -> Result<(), RuntimeError> {
        let socket = self.guest.join("proxy.sock");
        match fs::symlink_metadata(&socket) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                fs::remove_file(socket).map_err(|e| failure(&self.id, "remove old proxy socket", e))
            }
            Ok(_) => Err(invalid(
                &self.id,
                "proxy endpoint exists and is not a Unix socket",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(&self.id, "inspect old proxy socket", error)),
        }
    }

    /// Explicit destroy only, after verifying the owning container is absent.
    /// A failed removal remains recoverable; no ancestor or sibling is removed.
    pub fn remove(self) -> Result<(), RuntimeError> {
        fs::remove_dir_all(&self.outer)
            .map_err(|e| failure(&self.id, "remove detached mount directory", e))
    }
}

#[cfg(test)]
mod tests;
