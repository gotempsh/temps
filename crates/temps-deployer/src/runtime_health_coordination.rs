// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A Docker-daemon-wide pointer to a protected, shared file-lock namespace.
//! The volume stores metadata only; it is never mounted in an application.

use bollard::{models::VolumeCreateRequest, Docker};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use temps_runtime_health::host::{
    persist, private_directory, private_file, read_record, verify_private_directory, HostError,
};
use uuid::Uuid;

pub const COORDINATION_VOLUME: &str = "temps-runtime-health-coordination-v1";
const PROTOCOL: &str = "sh.temps.runtime-health.protocol";
const DIRECTORY: &str = "sh.temps.runtime-health.directory";
const PROOF: &str = "sh.temps.runtime-health.proof";
const DAEMON: &str = "sh.temps.runtime-health.daemon";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    id: Uuid,
}

fn unavailable(reason: &'static str) -> HostError {
    HostError::Coordination { reason }
}

fn verify_local_filesystem(path: &Path) -> Result<(), HostError> {
    #[cfg(target_os = "linux")]
    {
        use nix::sys::statfs::{
            statfs, FsType, BTRFS_SUPER_MAGIC, EXT4_SUPER_MAGIC, OVERLAYFS_SUPER_MAGIC, TMPFS_MAGIC,
        };
        // Linux's XFS magic is stable, but nix only exports its named constant
        // on non-musl targets. Keep the same filesystem allowlist on musl.
        const XFS: FsType = FsType(0x5846_5342);
        let filesystem = statfs(path)
            .map_err(|_| unavailable("cannot inspect coordination filesystem"))?
            .filesystem_type();
        if ![
            EXT4_SUPER_MAGIC,
            XFS,
            BTRFS_SUPER_MAGIC,
            OVERLAYFS_SUPER_MAGIC,
            TMPFS_MAGIC,
        ]
        .contains(&filesystem)
        {
            return Err(unavailable(
                "coordination requires a local filesystem with shared advisory locks",
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let filesystem = nix::sys::statfs::statfs(path)
            .map_err(|_| unavailable("cannot inspect coordination filesystem"))?;
        if !matches!(filesystem.filesystem_type_name(), "apfs" | "hfs" | "tmpfs") {
            return Err(unavailable(
                "coordination requires a local filesystem with shared advisory locks",
            ));
        }
    }
    Ok(())
}

fn proof(directory: &Path) -> Result<String, HostError> {
    verify_private_directory(directory)?;
    verify_local_filesystem(directory)?;
    let marker: Marker = read_record(&directory.join("namespace.json"))?
        .ok_or_else(|| unavailable("shared coordination marker is missing"))?;
    if marker.id.is_nil() {
        return Err(unavailable("shared coordination marker is invalid"));
    }
    let metadata = std::fs::symlink_metadata(directory.join("namespace.json"))
        .map_err(|_| unavailable("shared coordination marker cannot be inspected"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(unavailable(
            "shared coordination marker is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{}",
            marker.id,
            metadata.dev(),
            metadata.ino()
        ))
    }
    #[cfg(not(unix))]
    {
        Err(unavailable(
            "this platform cannot establish shared runtime ownership",
        ))
    }
}

fn prepare(root: &Path) -> Result<(PathBuf, String), HostError> {
    let directory = root.join("locks");
    private_directory(&directory)?;
    verify_local_filesystem(&directory)?;
    let lock = private_file(&directory.join("namespace.lock"))?;
    lock.try_lock()
        .map_err(|_| unavailable("another process is initializing coordination"))?;
    if read_record::<Marker>(&directory.join("namespace.json"))?.is_none() {
        persist(&directory, "namespace.json", &Marker { id: Uuid::new_v4() })?;
    }
    let identity = proof(&directory)?;
    let directory = std::fs::canonicalize(directory)
        .map_err(|_| unavailable("cannot resolve protected coordination directory"))?;
    Ok((directory, identity))
}

pub async fn namespace(
    docker: &Docker,
    root: &Path,
    daemon: &str,
    name: &str,
) -> Result<PathBuf, HostError> {
    let owned_root = root.to_path_buf();
    let (local, local_proof) = tokio::task::spawn_blocking(move || prepare(&owned_root))
        .await
        .map_err(|_| HostError::JournalFailed)??;
    let existing = tokio::time::timeout(Duration::from_secs(5), docker.inspect_volume(name))
        .await
        .map_err(|_| unavailable("Docker coordination lookup timed out"))?;
    let volume = match existing {
        Ok(volume) => volume,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            let path = local
                .to_str()
                .ok_or_else(|| unavailable("coordination directory is not UTF-8"))?;
            let labels = [
                (PROTOCOL.into(), "1".into()),
                (DIRECTORY.into(), path.into()),
                (PROOF.into(), local_proof.clone()),
                (DAEMON.into(), daemon.into()),
            ]
            .into();
            // Docker's named-volume create is idempotent and never replaces
            // existing labels. Read back the winner of concurrent first boots.
            tokio::time::timeout(
                Duration::from_secs(5),
                docker.create_volume(VolumeCreateRequest {
                    name: Some(name.into()),
                    labels: Some(labels),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|_| unavailable("Docker coordination creation timed out"))?
            .map_err(|_| unavailable("Docker coordination creation failed"))?;
            tokio::time::timeout(Duration::from_secs(5), docker.inspect_volume(name))
                .await
                .map_err(|_| unavailable("Docker coordination confirmation timed out"))?
                .map_err(|_| unavailable("Docker coordination confirmation failed"))?
        }
        Err(_) => return Err(unavailable("Docker coordination lookup failed")),
    };
    if volume.labels.get(PROTOCOL).map(String::as_str) != Some("1")
        || volume.labels.get(DAEMON).map(String::as_str) != Some(daemon)
    {
        return Err(unavailable(
            "an existing coordination volume has an unexpected identity; it was not modified",
        ));
    }
    let expected = volume
        .labels
        .get(PROOF)
        .cloned()
        .ok_or_else(|| unavailable("coordination proof is missing"))?;
    if expected == local_proof {
        return Ok(local);
    }
    let advertised = volume
        .labels
        .get(DIRECTORY)
        .filter(|path| path.len() <= 4096)
        .map(PathBuf::from)
        .ok_or_else(|| unavailable("shared coordination directory is missing"))?;
    if !advertised.is_absolute() {
        return Err(unavailable("shared coordination directory is not absolute"));
    }
    // Only verify an existing directory. No path from Docker metadata is ever
    // created or chmod'ed. Workload labels cannot create this platform record.
    tokio::task::spawn_blocking(move || {
        if proof(&advertised).is_ok_and(|actual|actual==expected) { Ok(advertised) }
        else { Err(unavailable("co-located processes sharing Docker must share the same protected runtime-health coordination directory")) }
    }).await.map_err(|_|HostError::JournalFailed)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coordination_reuses_one_namespace_and_does_not_adopt_unshared_or_foreign_metadata(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping coordination integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping coordination integration");
            return Ok(());
        }
        let daemon = docker
            .info()
            .await?
            .id
            .ok_or("Docker daemon identity missing")?;
        let root_a = tempfile::tempdir()?;
        let root_b = tempfile::tempdir()?;
        let name = format!("temps-runtime-health-test-{}", Uuid::new_v4());
        let foreign = format!("temps-runtime-health-test-{}", Uuid::new_v4());
        let first = namespace(&docker, root_a.path(), &daemon, &name).await?;
        let labels = docker.inspect_volume(&name).await?.labels;
        let mut replacement = labels.clone();
        replacement.insert(PROOF.into(), "different-proof".into());
        docker
            .create_volume(VolumeCreateRequest {
                name: Some(name.clone()),
                labels: Some(replacement),
                ..Default::default()
            })
            .await?;
        assert_eq!(
            docker.inspect_volume(&name).await?.labels,
            labels,
            "idempotent create must not overwrite the existing coordination identity"
        );
        assert_eq!(
            namespace(&docker, root_b.path(), &daemon, &name).await?,
            first
        );
        let absent = root_a.path().join("unshared-namespace");
        let mut unshared = labels.clone();
        unshared.insert(DIRECTORY.into(), absent.to_string_lossy().into_owned());
        docker
            .create_volume(VolumeCreateRequest {
                name: Some(foreign.clone()),
                labels: Some(unshared),
                ..Default::default()
            })
            .await?;
        assert!(matches!(
            namespace(&docker, root_b.path(), &daemon, &foreign).await,
            Err(HostError::Coordination { .. })
        ));
        assert!(
            !absent.exists(),
            "paths from daemon metadata must not be created"
        );
        assert_eq!(docker.inspect_volume(&name).await?.labels, labels);
        for volume in [&name, &foreign] {
            docker
                .remove_volume(
                    volume,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await?;
        }
        Ok(())
    }
}
