// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a worker keeps on disk.
//!
//! - [`SnapshotStore`]: the last received state, `state.json`, mode 0600.
//!   Specs and secret references only: a workload spec's variables whose
//!   names classify as secret (`temps_workload_env::is_secret_variable`)
//!   are written without their value, and instance templates carry no
//!   environment at all.
//! - [`SecretStore`]: one file per instance under `secrets/`, mode 0400 in
//!   a 0700 directory, holding the environment the control plane gave for
//!   that instance's create call. It is what lets an instance be
//!   recreated while the control plane is unreachable.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use temps_workload::stream::{DnsClaimObject, InstanceObject, InstanceReport, WorkloadObject};
use temps_workload_env::is_secret_variable;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("node state store: could not {operation} '{path}': {reason}")]
    Io {
        operation: &'static str,
        path: String,
        reason: String,
    },
    #[error("node state store: '{path}' could not be decoded: {reason}")]
    Decode { path: String, reason: String },
    #[error("node state store: instance name '{name}' is not a safe file name")]
    UnsafeName { name: String },
}

fn io<'a>(
    operation: &'static str,
    path: &'a Path,
) -> impl FnOnce(std::io::Error) -> StoreError + 'a {
    move |e| StoreError::Io {
        operation,
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// The state a node holds: what it last received, and what it last
/// observed of each instance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeState {
    /// The cursor of the next read (`0`: ask for the whole view).
    pub watermark: String,
    pub workloads: BTreeMap<i64, WorkloadObject>,
    pub instances: BTreeMap<i64, InstanceObject>,
    pub dns_claims: BTreeMap<i64, DnsClaimObject>,
    /// What the node last observed, by instance id.
    pub observed: BTreeMap<i64, InstanceReport>,
}

/// Write `bytes` to `path` atomically with `mode` (Unix).
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Created 0600 so the file is never readable by others, even
        // before its final mode is set.
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io("create", &tmp))?;
    file.write_all(bytes).map_err(io("write", &tmp))?;
    file.sync_all().map_err(io("sync", &tmp))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(io("set the mode of", &tmp))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    // A 0400 target cannot be replaced in place on every platform.
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(io("rename into place", path))?;
    Ok(())
}

fn create_private_dir(dir: &Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir).map_err(io("create", dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(io("set the mode of", dir))?;
    }
    Ok(())
}

/// The node's last received state, `<dir>/state.json`, mode 0600.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    dir: PathBuf,
}

impl SnapshotStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// The stored state, or the empty state when there is none (the next
    /// read asks for the whole view).
    pub fn load(&self) -> Result<NodeState, StoreError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(NodeState::default()),
            Err(e) => return Err(io("read", &path)(e)),
        };
        serde_json::from_slice(&bytes).map_err(|e| StoreError::Decode {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// Store `state`, without any secret value.
    pub fn save(&self, state: &NodeState) -> Result<(), StoreError> {
        create_private_dir(&self.dir)?;
        let mut stored = state.clone();
        for workload in stored.workloads.values_mut() {
            for (name, value) in workload.spec.env.vars.iter_mut() {
                if is_secret_variable(name) {
                    value.clear();
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(&stored).map_err(|e| StoreError::Decode {
            path: self.path().display().to_string(),
            reason: e.to_string(),
        })?;
        write_atomic(&self.path(), &bytes, 0o600)
    }
}

/// Per-instance environment files, `<dir>/secrets/<instance>.env`.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into().join("secrets"),
        }
    }

    fn path_of(&self, instance: &str) -> Result<PathBuf, StoreError> {
        let safe = !instance.is_empty()
            && instance
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && !instance.starts_with('.');
        if !safe {
            return Err(StoreError::UnsafeName {
                name: instance.to_string(),
            });
        }
        Ok(self.dir.join(format!("{instance}.env")))
    }

    /// Keep `env` for `instance`, mode 0400.
    pub fn save(&self, instance: &str, env: &[(String, String)]) -> Result<(), StoreError> {
        create_private_dir(&self.dir)?;
        let path = self.path_of(instance)?;
        let bytes = serde_json::to_vec(env).map_err(|e| StoreError::Decode {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        write_atomic(&path, &bytes, 0o400)
    }

    /// The kept environment of `instance`; `None` when its file is gone.
    pub fn load(&self, instance: &str) -> Result<Option<Vec<(String, String)>>, StoreError> {
        let path = self.path_of(instance)?;
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StoreError::Decode {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io("read", &path)(e)),
        }
    }

    pub fn remove(&self, instance: &str) -> Result<(), StoreError> {
        let path = self.path_of(instance)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io("remove", &path)(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_files_refuse_names_that_leave_their_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SecretStore::new(dir.path());
        assert!(matches!(
            store.save("../escape", &[]),
            Err(StoreError::UnsafeName { .. })
        ));
        assert!(matches!(
            store.load(".hidden"),
            Err(StoreError::UnsafeName { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn files_get_their_modes_and_round_trip() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let secrets = SecretStore::new(dir.path());
        let env = vec![("A".to_string(), "1".to_string())];
        secrets.save("temps-job-1-a1", &env).expect("saved");
        // Saving again replaces a 0400 file.
        secrets.save("temps-job-1-a1", &env).expect("saved again");
        let path = dir.path().join("secrets/temps-job-1-a1.env");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o400);
        assert_eq!(secrets.load("temps-job-1-a1").expect("loaded"), Some(env));
        secrets.remove("temps-job-1-a1").expect("removed");
        assert_eq!(secrets.load("temps-job-1-a1").expect("loaded"), None);

        let snapshots = SnapshotStore::new(dir.path());
        assert_eq!(snapshots.load().expect("empty"), NodeState::default());
        let state = NodeState {
            watermark: "42".into(),
            ..Default::default()
        };
        snapshots.save(&state).expect("saved");
        let mode = std::fs::metadata(snapshots.path())
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(snapshots.load().expect("loaded"), state);
    }
}
