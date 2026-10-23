// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Volumes (ADR-055 §12).
//!
//! - [`Volume`]: a first-class resource with an owner, a backing, the node
//!   it lives on, a size and whether it outlives its owner.
//! - [`validate_mounts`]: what a spec may mount, checked at admission.
//! - [`pinning`]: a node-local volume pins the workload to its node (§8),
//!   with the reason the console shows.
//! - [`VolumeProvider`]: creates and deletes the backing for a driver's
//!   [`VolumeKinds`]. Snapshot and restore are optional capabilities.
//!
//! Nothing here touches Docker or the database: the Docker provider lives
//! with the Docker driver, the rows with the plane.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use temps_workload::{VolumeKinds, VolumeMount, WorkloadRef};

/// How a volume is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Backing {
    /// A Docker named volume.
    Named { name: String },
    /// An ext4 image file, attached as a block device (microVMs).
    BlockImage { path: String },
    /// A host directory preopened for a WASM instance.
    PreopenedDir { path: String },
}

impl Backing {
    pub fn kind(&self) -> BackingKind {
        match self {
            Backing::Named { .. } => BackingKind::Named,
            Backing::BlockImage { .. } => BackingKind::BlockImage,
            Backing::PreopenedDir { .. } => BackingKind::PreopenedDir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackingKind {
    Named,
    BlockImage,
    PreopenedDir,
}

impl BackingKind {
    pub fn supported_by(&self, kinds: &VolumeKinds) -> bool {
        match self {
            BackingKind::Named => kinds.named,
            BackingKind::BlockImage => kinds.block_image,
            BackingKind::PreopenedDir => kinds.preopened_dir,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            BackingKind::Named => "named",
            BackingKind::BlockImage => "block_image",
            BackingKind::PreopenedDir => "preopened_dir",
        }
    }
}

impl fmt::Display for BackingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub id: i64,
    /// Unique per owner; what a [`VolumeMount`] names.
    pub name: String,
    pub owner: WorkloadRef,
    pub backing: Backing,
    /// The node the data lives on. `None` until it is first created.
    pub node_id: Option<i32>,
    pub size_mb: Option<u64>,
    /// Keep the data when the owner is deleted.
    #[serde(default)]
    pub retain_on_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeError {
    #[error("volume name '{name}' is invalid: expected 2-64 characters of [a-z0-9_.-], starting with a letter or digit")]
    InvalidName { name: String },
    #[error("mount {index} of {owner} names volume '{volume}', which {owner} does not own")]
    UnknownVolume {
        owner: WorkloadRef,
        index: usize,
        volume: String,
    },
    #[error("mount {index} of {owner} has path '{path}': {reason}")]
    InvalidMountPath {
        owner: WorkloadRef,
        index: usize,
        path: String,
        reason: String,
    },
    #[error("{owner} mounts '{path}' twice")]
    DuplicateMountPath { owner: WorkloadRef, path: String },
    #[error(
        "{owner} mounts volumes on different nodes ({}); a workload runs on one node, so its node-local volumes must share it",
        .placements.iter().map(|(v, n)| format!("'{v}' on node {n}")).collect::<Vec<_>>().join(", ")
    )]
    SpreadAcrossNodes {
        owner: WorkloadRef,
        placements: Vec<(String, i32)>,
    },
    #[error("{owner} is pinned to nodes {nodes:?}, but volume '{volume}' lives on node {node_id}")]
    PinConflict {
        owner: WorkloadRef,
        volume: String,
        node_id: i32,
        nodes: Vec<i32>,
    },
    #[error("the {driver} driver cannot attach volume '{volume}': it does not support {backing} volumes")]
    UnsupportedBacking {
        driver: String,
        volume: String,
        backing: BackingKind,
    },
    #[error(
        "volume {volume_id} ('{name}'): {operation} is not supported by the {provider} provider"
    )]
    Unsupported {
        volume_id: i64,
        name: String,
        operation: &'static str,
        provider: String,
    },
    #[error("volume {volume_id} ('{name}'): {operation} failed: {reason}")]
    Provider {
        volume_id: i64,
        name: String,
        operation: &'static str,
        reason: String,
    },
}

/// Volume names become Docker volume names and file names, so they are
/// kept to a portable subset.
pub fn validate_name(name: &str) -> Result<(), VolumeError> {
    let mut chars = name.chars();
    let ok = (2..=64).contains(&name.len())
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(())
    } else {
        Err(VolumeError::InvalidName {
            name: name.to_string(),
        })
    }
}

/// Paths a volume may never cover: they hold what Temps itself delivers
/// (`/run/secrets`) or what the kernel owns.
const RESERVED_PATHS: &[&str] = &[
    "/",
    "/proc",
    "/sys",
    "/dev",
    "/run/secrets",
    "/etc/hosts",
    "/etc/resolv.conf",
];

fn path_error(owner: WorkloadRef, index: usize, path: &str, reason: &str) -> VolumeError {
    VolumeError::InvalidMountPath {
        owner,
        index,
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

/// Checks a spec's mounts against the volumes its owner has. Returns the
/// volume each mount resolves to, in mount order.
pub fn validate_mounts<'a>(
    owner: WorkloadRef,
    mounts: &[VolumeMount],
    volumes: &'a [Volume],
    driver: &str,
    kinds: &VolumeKinds,
) -> Result<Vec<&'a Volume>, VolumeError> {
    let by_name: BTreeMap<&str, &Volume> = volumes
        .iter()
        .filter(|v| v.owner == owner)
        .map(|v| (v.name.as_str(), v))
        .collect();
    let mut paths = BTreeSet::new();
    let mut resolved = Vec::with_capacity(mounts.len());
    for (index, mount) in mounts.iter().enumerate() {
        let volume = by_name.get(mount.volume.as_str()).copied().ok_or_else(|| {
            VolumeError::UnknownVolume {
                owner,
                index,
                volume: mount.volume.clone(),
            }
        })?;
        let path = mount.mount_path.as_str();
        if !path.starts_with('/') {
            return Err(path_error(owner, index, path, "it must be absolute"));
        }
        if path
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
        {
            return Err(path_error(
                owner,
                index,
                path,
                "it must not contain '.' or '..' segments",
            ));
        }
        if path.contains('\0') || path.contains(':') {
            return Err(path_error(
                owner,
                index,
                path,
                "it must not contain ':' or NUL",
            ));
        }
        let normalized = path.trim_end_matches('/');
        let normalized = if normalized.is_empty() {
            "/"
        } else {
            normalized
        };
        // A mount may neither sit inside a reserved path nor hide one
        // (a volume at `/run` would cover `/run/secrets`).
        let overlaps = |reserved: &str| {
            if reserved == "/" {
                return normalized == "/";
            }
            normalized == reserved
                || normalized.starts_with(&format!("{reserved}/"))
                || reserved.starts_with(&format!("{normalized}/"))
        };
        if let Some(reserved) = RESERVED_PATHS.iter().find(|r| overlaps(r)) {
            return Err(path_error(
                owner,
                index,
                path,
                &format!("it overlaps '{reserved}', which Temps or the kernel provides"),
            ));
        }
        if !paths.insert(normalized.to_string()) {
            return Err(VolumeError::DuplicateMountPath {
                owner,
                path: normalized.to_string(),
            });
        }
        let backing = volume.backing.kind();
        if !backing.supported_by(kinds) {
            return Err(VolumeError::UnsupportedBacking {
                driver: driver.to_string(),
                volume: volume.name.clone(),
                backing,
            });
        }
        resolved.push(volume);
    }
    Ok(resolved)
}

/// Why a workload is pinned, for the console ("pinned by volume `data`").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumePin {
    pub node_id: i32,
    /// The volumes that pin it there, sorted.
    pub volumes: Vec<String>,
}

impl fmt::Display for VolumePin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.volumes.iter().map(|v| format!("`{v}`")).collect();
        let noun = if names.len() == 1 {
            "volume"
        } else {
            "volumes"
        };
        write!(
            f,
            "pinned to node {} by {noun} {}",
            self.node_id,
            names.join(", ")
        )
    }
}

/// The node the mounted volumes pin the workload to (§8).
///
/// - Volumes not created yet (`node_id = None`) do not pin: they are
///   created where the workload lands.
/// - Volumes on two nodes cannot be satisfied and are refused.
/// - A pin outside the workload's own required node set is a conflict:
///   pinning is strict and never moves a workload quietly.
pub fn pinning(
    owner: WorkloadRef,
    mounted: &[&Volume],
    required_nodes: Option<&[i32]>,
) -> Result<Option<VolumePin>, VolumeError> {
    let mut by_node: BTreeMap<i32, BTreeSet<String>> = BTreeMap::new();
    for volume in mounted {
        if let Some(node) = volume.node_id {
            by_node.entry(node).or_default().insert(volume.name.clone());
        }
    }
    if by_node.len() > 1 {
        let placements = by_node
            .iter()
            .flat_map(|(node, names)| names.iter().map(move |n| (n.clone(), *node)))
            .collect();
        return Err(VolumeError::SpreadAcrossNodes { owner, placements });
    }
    let Some((node_id, names)) = by_node.into_iter().next() else {
        return Ok(None);
    };
    if let Some(nodes) = required_nodes {
        if !nodes.contains(&node_id) {
            return Err(VolumeError::PinConflict {
                owner,
                volume: names.iter().next().cloned().unwrap_or_default(),
                node_id,
                nodes: nodes.to_vec(),
            });
        }
    }
    Ok(Some(VolumePin {
        node_id,
        volumes: names.into_iter().collect(),
    }))
}

/// A handle to a snapshot a provider took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSnapshot {
    pub volume_id: i64,
    pub reference: String,
}

/// Creates and removes the backing of a volume on one node.
#[async_trait]
pub trait VolumeProvider: Send + Sync {
    fn name(&self) -> &str;

    fn kinds(&self) -> VolumeKinds;

    /// Creates the backing if it does not exist. Idempotent.
    async fn ensure(&self, volume: &Volume) -> Result<(), VolumeError>;

    /// Removes the backing. Idempotent. Callers check `retain_on_delete`.
    async fn delete(&self, volume: &Volume) -> Result<(), VolumeError>;

    async fn snapshot(&self, volume: &Volume) -> Result<VolumeSnapshot, VolumeError> {
        Err(VolumeError::Unsupported {
            volume_id: volume.id,
            name: volume.name.clone(),
            operation: "snapshot",
            provider: self.name().to_string(),
        })
    }

    async fn restore(&self, volume: &Volume, _from: &VolumeSnapshot) -> Result<(), VolumeError> {
        Err(VolumeError::Unsupported {
            volume_id: volume.id,
            name: volume.name.clone(),
            operation: "restore",
            provider: self.name().to_string(),
        })
    }
}

/// What deleting an owner does to each of its volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnOwnerDelete {
    Delete,
    Retain,
}

pub fn on_owner_delete(volume: &Volume) -> OnOwnerDelete {
    if volume.retain_on_delete {
        OnOwnerDelete::Retain
    } else {
        OnOwnerDelete::Delete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use temps_workload::WorkloadKind;

    fn owner() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Deployment, 3)
    }

    fn volume(id: i64, name: &str, node_id: Option<i32>) -> Volume {
        Volume {
            id,
            name: name.to_string(),
            owner: owner(),
            backing: Backing::Named {
                name: format!("temps-vol-{id}"),
            },
            node_id,
            size_mb: None,
            retain_on_delete: false,
        }
    }

    fn mount(volume: &str, path: &str) -> VolumeMount {
        VolumeMount {
            volume: volume.to_string(),
            mount_path: path.to_string(),
            read_only: false,
            source: None,
        }
    }

    const DOCKER: VolumeKinds = VolumeKinds {
        named: true,
        block_image: false,
        preopened_dir: false,
    };

    #[test]
    fn names_are_portable() {
        for ok in ["data", "pg-data", "cache.v2", "a1", "0x"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a", "Data", "-data", "da ta", "da/ta", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mounts_resolve_to_the_owners_volumes() {
        let volumes = [volume(1, "data", Some(2)), volume(2, "cache", None)];
        let resolved = validate_mounts(
            owner(),
            &[mount("data", "/var/lib/data"), mount("cache", "/cache/")],
            &volumes,
            "docker",
            &DOCKER,
        )
        .unwrap();
        assert_eq!(resolved.iter().map(|v| v.id).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn another_owners_volume_is_unknown() {
        let mut theirs = volume(9, "data", None);
        theirs.owner = WorkloadRef::new(WorkloadKind::Deployment, 4);
        let err = validate_mounts(
            owner(),
            &[mount("data", "/data")],
            &[theirs],
            "docker",
            &DOCKER,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "mount 0 of deployment/3 names volume 'data', which deployment/3 does not own"
        );
    }

    #[test]
    fn reserved_relative_and_duplicate_paths_are_refused() {
        let volumes = [volume(1, "data", None), volume(2, "more", None)];
        let check = |mounts: Vec<VolumeMount>| {
            validate_mounts(owner(), &mounts, &volumes, "docker", &DOCKER)
        };
        for path in [
            "/",
            "/run/secrets",
            "/run/secrets/x",
            "/run",
            "/proc/1",
            "/etc/resolv.conf",
            "data",
            "/a/../etc",
            "/a:b",
        ] {
            assert!(
                matches!(
                    check(vec![mount("data", path)]),
                    Err(VolumeError::InvalidMountPath { .. })
                ),
                "{path} must be refused"
            );
        }
        assert!(check(vec![mount("data", "/srv/data")]).is_ok());
        assert!(check(vec![mount("data", "/etc/app")]).is_ok());
        assert_eq!(
            check(vec![mount("data", "/srv"), mount("more", "/srv/")]),
            Err(VolumeError::DuplicateMountPath {
                owner: owner(),
                path: "/srv".into()
            })
        );
    }

    #[test]
    fn a_driver_without_the_backing_is_refused() {
        let mut image = volume(1, "disk", None);
        image.backing = Backing::BlockImage {
            path: "/var/lib/temps/disk.ext4".into(),
        };
        let err = validate_mounts(
            owner(),
            &[mount("disk", "/data")],
            &[image],
            "docker",
            &DOCKER,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the docker driver cannot attach volume 'disk': it does not support block_image volumes"
        );
    }

    #[test]
    fn node_local_volumes_pin_and_say_why() {
        let a = volume(1, "data", Some(2));
        let b = volume(2, "logs", Some(2));
        let fresh = volume(3, "cache", None);
        let pin = pinning(owner(), &[&b, &a, &fresh], None).unwrap().unwrap();
        assert_eq!(
            pin,
            VolumePin {
                node_id: 2,
                volumes: vec!["data".into(), "logs".into()]
            }
        );
        assert_eq!(
            pin.to_string(),
            "pinned to node 2 by volumes `data`, `logs`"
        );
        assert_eq!(pinning(owner(), &[&fresh], None).unwrap(), None);
        assert_eq!(
            pinning(owner(), &[&a], None).unwrap().unwrap().to_string(),
            "pinned to node 2 by volume `data`"
        );
    }

    #[test]
    fn volumes_on_two_nodes_or_outside_the_pin_are_refused() {
        let a = volume(1, "data", Some(2));
        let b = volume(2, "logs", Some(5));
        let err = pinning(owner(), &[&a, &b], None).unwrap_err();
        assert!(
            err.to_string()
                .contains("'data' on node 2, 'logs' on node 5"),
            "{err}"
        );
        assert_eq!(
            pinning(owner(), &[&a], Some(&[4, 5])),
            Err(VolumeError::PinConflict {
                owner: owner(),
                volume: "data".into(),
                node_id: 2,
                nodes: vec![4, 5]
            })
        );
        assert!(pinning(owner(), &[&a], Some(&[2, 5])).unwrap().is_some());
    }

    #[test]
    fn retain_on_delete_is_explicit() {
        let mut v = volume(1, "data", Some(2));
        assert_eq!(on_owner_delete(&v), OnOwnerDelete::Delete);
        v.retain_on_delete = true;
        assert_eq!(on_owner_delete(&v), OnOwnerDelete::Retain);
    }

    #[test]
    fn backings_serialize_with_a_type_tag() {
        let json = serde_json::to_value(Backing::Named {
            name: "temps-vol-1".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "named", "name": "temps-vol-1"})
        );
    }

    /// An in-memory provider: the trait's contract, without a daemon.
    #[derive(Default)]
    struct Memory {
        present: Mutex<BTreeSet<i64>>,
    }

    #[async_trait]
    impl VolumeProvider for Memory {
        fn name(&self) -> &str {
            "memory"
        }

        fn kinds(&self) -> VolumeKinds {
            DOCKER
        }

        async fn ensure(&self, volume: &Volume) -> Result<(), VolumeError> {
            self.present
                .lock()
                .map_err(|e| VolumeError::Provider {
                    volume_id: volume.id,
                    name: volume.name.clone(),
                    operation: "ensure",
                    reason: e.to_string(),
                })?
                .insert(volume.id);
            Ok(())
        }

        async fn delete(&self, volume: &Volume) -> Result<(), VolumeError> {
            self.present
                .lock()
                .map_err(|e| VolumeError::Provider {
                    volume_id: volume.id,
                    name: volume.name.clone(),
                    operation: "delete",
                    reason: e.to_string(),
                })?
                .remove(&volume.id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn providers_are_idempotent_and_snapshots_are_optional() {
        let provider = Memory::default();
        let v = volume(1, "data", Some(2));
        provider.ensure(&v).await.unwrap();
        provider.ensure(&v).await.unwrap();
        assert_eq!(provider.present.lock().unwrap().len(), 1);
        provider.delete(&v).await.unwrap();
        provider.delete(&v).await.unwrap();
        assert!(provider.present.lock().unwrap().is_empty());
        let err = provider.snapshot(&v).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "volume 1 ('data'): snapshot is not supported by the memory provider"
        );
    }
}
