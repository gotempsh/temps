// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How the Docker daemon must be told to mount a path this process can see.
//!
//! A bind mount's source is resolved by the daemon, on the daemon's
//! filesystem. A native process shares that filesystem, so a path it wrote is
//! a path the daemon can mount. A process in a container does not: asking the
//! daemon to bind `/app/data/secrets/web` names a directory on the *host*,
//! which the daemon silently creates empty, and the workload starts with none
//! of the files this process wrote.
//!
//! What such a process can hand to the daemon instead depends on how its own
//! container was given the directory:
//!
//! - bound from a host path: the same file is at that host path plus the
//!   remainder, which the daemon can bind;
//! - a named volume: the daemon can mount that volume, narrowed to the
//!   remainder with a volume subpath (Docker Engine 26.0, API 1.45);
//! - neither (the container's own writable layer): the daemon has no way to
//!   mount it, and saying so is the only honest answer.
//!
//! [`DaemonMounts`] finds out which applies, once, by inspecting this
//! process's own container, and [`DaemonMounts::resolve`] answers per path.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bollard::models::{Mount, MountType, MountVolumeOptions};
use bollard::Docker;
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::runtime::ExecutionEnvironment;

/// First Docker Engine API version that accepts a volume subpath.
const VOLUME_SUBPATH_API_VERSION: (u32, u32) = (1, 45);

/// Where the files of one mount of this process's own container come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnMountSource {
    /// Bound from this path on the daemon's host.
    HostPath(PathBuf),
    /// A named volume.
    Volume(String),
}

/// One mount of this process's own container, as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnMount {
    /// Where it is mounted inside this container.
    pub destination: PathBuf,
    pub source: OwnMountSource,
}

/// What the daemon can see of this process's filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonFilesystemView {
    /// The process runs natively: every path it sees, the daemon sees.
    Shared,
    /// The process runs in a container: only its mounts reach the daemon.
    Container {
        /// This process's own container, for error messages.
        container: String,
        mounts: Vec<OwnMount>,
        /// Whether the daemon accepts a volume subpath, and the API version
        /// it reported.
        volume_subpath_supported: bool,
        api_version: String,
    },
}

/// What to give the daemon so a workload sees a path of this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonMountSource {
    /// Bind this path on the daemon's host.
    HostPath(PathBuf),
    /// Mount this named volume, narrowed to `subpath` when it is set.
    VolumeSubpath {
        volume: String,
        subpath: Option<String>,
    },
}

impl DaemonMountSource {
    /// The mount that puts this source at `target` in a workload.
    pub fn into_mount(self, target: &str, read_only: bool) -> Mount {
        match self {
            Self::HostPath(path) => Mount {
                target: Some(target.to_string()),
                source: Some(path.to_string_lossy().into_owned()),
                typ: Some(MountType::BIND),
                read_only: Some(read_only),
                ..Default::default()
            },
            Self::VolumeSubpath { volume, subpath } => Mount {
                target: Some(target.to_string()),
                source: Some(volume),
                typ: Some(MountType::VOLUME),
                read_only: Some(read_only),
                volume_options: Some(MountVolumeOptions {
                    // The directory already holds what the workload must see;
                    // never seed it from the image.
                    no_copy: Some(true),
                    subpath,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum DaemonMountError {
    #[error("Cannot mount '{path}' into a workload: the path is not absolute")]
    RelativePath { path: String },

    #[error(
        "Cannot mount '{path}' into a workload: Temps runs in container '{container}' and that \
         path is in the container's own filesystem, which the Docker daemon cannot mount. Mount \
         a named volume or a host directory at a parent of '{path}' in the Temps container \
         (the data directory is the usual one) and restart Temps"
    )]
    NotOnAMount { path: String, container: String },

    #[error(
        "Cannot mount '{path}' into a workload: it is inside the named volume '{volume}', and \
         mounting a directory of a volume needs Docker Engine 26.0 (API 1.45) or newer; this \
         daemon reports API {api_version}. Upgrade Docker, or give the Temps container that \
         directory as a bind mount from a host path instead of a named volume"
    )]
    VolumeSubpathUnsupported {
        path: String,
        volume: String,
        api_version: String,
    },

    #[error(
        "Cannot tell which container Temps runs in, so paths cannot be translated for the Docker \
         daemon: {reason}"
    )]
    OwnContainerUnknown { reason: String },

    #[error("Failed to inspect Temps' own container '{container}': {reason}")]
    InspectFailed { container: String, reason: String },

    #[error("Failed to read the Docker daemon version: {reason}")]
    VersionFailed { reason: String },
}

impl DaemonFilesystemView {
    /// What to give the daemon so a workload sees `path`.
    pub fn resolve(&self, path: &Path) -> Result<DaemonMountSource, DaemonMountError> {
        if !path.is_absolute() {
            return Err(DaemonMountError::RelativePath {
                path: path.display().to_string(),
            });
        }
        let (container, mounts, volume_subpath_supported, api_version) = match self {
            Self::Shared => return Ok(DaemonMountSource::HostPath(path.to_path_buf())),
            Self::Container {
                container,
                mounts,
                volume_subpath_supported,
                api_version,
            } => (container, mounts, *volume_subpath_supported, api_version),
        };
        // The innermost mount containing the path is the one that backs it.
        let backing = mounts
            .iter()
            .filter_map(|mount| {
                path.strip_prefix(&mount.destination)
                    .ok()
                    .map(|remainder| (mount, remainder))
            })
            .max_by_key(|(mount, _)| mount.destination.components().count());
        let Some((mount, remainder)) = backing else {
            return Err(DaemonMountError::NotOnAMount {
                path: path.display().to_string(),
                container: container.clone(),
            });
        };
        match &mount.source {
            OwnMountSource::HostPath(host_path) => {
                Ok(DaemonMountSource::HostPath(host_path.join(remainder)))
            }
            OwnMountSource::Volume(volume) => {
                let subpath = volume_subpath(remainder);
                if subpath.is_some() && !volume_subpath_supported {
                    return Err(DaemonMountError::VolumeSubpathUnsupported {
                        path: path.display().to_string(),
                        volume: volume.clone(),
                        api_version: api_version.clone(),
                    });
                }
                Ok(DaemonMountSource::VolumeSubpath {
                    volume: volume.clone(),
                    subpath,
                })
            }
        }
    }
}

/// `remainder` as a volume subpath: `/`-separated, `None` for the volume root.
fn volume_subpath(remainder: &Path) -> Option<String> {
    let parts: Vec<String> = remainder
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// The ID of the container whose mount namespace `mountinfo` describes.
///
/// Docker binds `/etc/hostname`, `/etc/hosts` and `/etc/resolv.conf` from
/// `<docker root>/containers/<id>/`, which makes the ID readable from inside
/// under both cgroup versions (cgroup v2 hides it from `/proc/self/cgroup`).
fn container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        // Field 4 is the root of the mount within its filesystem.
        let root = line.split_whitespace().nth(3)?;
        let (_, after) = root.split_once("/containers/")?;
        let id = after.split('/').next()?;
        let is_container_id = id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
        is_container_id.then(|| id.to_string())
    })
}

/// Whether `api_version` (`"1.45"`) accepts a volume subpath. An unreadable
/// version is treated as too old: the daemon would reject the mount anyway,
/// with a less useful message.
fn supports_volume_subpath(api_version: &str) -> bool {
    let mut parts = api_version.trim().split('.');
    let parsed = (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    );
    match parsed {
        (Some(major), Some(minor)) => (major, minor) >= VOLUME_SUBPATH_API_VERSION,
        _ => false,
    }
}

/// The mounts of an inspected container that the daemon can mount again.
fn own_mounts(mounts: Vec<bollard::models::MountPoint>) -> Vec<OwnMount> {
    mounts
        .into_iter()
        .filter_map(|mount| {
            let destination = PathBuf::from(mount.destination?);
            let source = match mount.typ.as_deref() {
                Some("bind") => OwnMountSource::HostPath(PathBuf::from(mount.source?)),
                Some("volume") => OwnMountSource::Volume(mount.name?),
                // tmpfs, image and pipe mounts cannot be given to a workload.
                _ => return None,
            };
            Some(OwnMount {
                destination,
                source,
            })
        })
        .collect()
}

async fn discover_container_view(
    docker: &Docker,
) -> Result<DaemonFilesystemView, DaemonMountError> {
    let from_mountinfo = tokio::fs::read_to_string("/proc/self/mountinfo")
        .await
        .ok()
        .and_then(|mountinfo| container_id_from_mountinfo(&mountinfo));
    // A container's hostname defaults to its short ID; an operator can set
    // another, in which case only the mount table identifies the container.
    // Read from the file Docker writes rather than the environment: this is
    // the container's identity, not configuration.
    let from_hostname = match from_mountinfo {
        Some(_) => None,
        None => tokio::fs::read_to_string("/etc/hostname")
            .await
            .ok()
            .map(|hostname| hostname.trim().to_string())
            .filter(|hostname| !hostname.is_empty()),
    };
    let Some(container) = from_mountinfo.or(from_hostname) else {
        return Err(DaemonMountError::OwnContainerUnknown {
            reason: "/proc/self/mountinfo names no Docker container and /etc/hostname is empty"
                .to_string(),
        });
    };
    let inspected = docker
        .inspect_container(
            &container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .map_err(|error| DaemonMountError::InspectFailed {
            container: container.clone(),
            reason: error.to_string(),
        })?;
    let version = docker
        .version()
        .await
        .map_err(|error| DaemonMountError::VersionFailed {
            reason: error.to_string(),
        })?;
    let api_version = version.api_version.unwrap_or_default();
    Ok(DaemonFilesystemView::Container {
        container: inspected
            .name
            .map(|name| name.trim_start_matches('/').to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or(container),
        mounts: own_mounts(inspected.mounts.unwrap_or_default()),
        volume_subpath_supported: supports_volume_subpath(&api_version),
        api_version,
    })
}

/// Translates paths of this process for the Docker daemon it creates
/// workloads on.
///
/// Cheap to clone and to construct: the daemon is asked once, on the first
/// path a containerized process needs translated. A native process never
/// asks.
#[derive(Debug, Clone)]
pub struct DaemonMounts {
    execution_environment: ExecutionEnvironment,
    docker: Option<Arc<Docker>>,
    view: Arc<OnceCell<DaemonFilesystemView>>,
}

impl DaemonMounts {
    /// For a process running in `execution_environment` that creates
    /// workloads on `docker`.
    pub fn new(execution_environment: ExecutionEnvironment, docker: Arc<Docker>) -> Self {
        Self {
            execution_environment,
            docker: Some(docker),
            view: Arc::new(OnceCell::new()),
        }
    }

    /// For the environment this process was started in.
    pub fn for_process(docker: Arc<Docker>) -> Self {
        Self::new(
            crate::runtime::execution_environment_compatibility(),
            docker,
        )
    }

    /// With a known view, for a native process and for tests.
    pub fn with_view(view: DaemonFilesystemView) -> Self {
        Self {
            execution_environment: ExecutionEnvironment::Host,
            docker: None,
            view: Arc::new(OnceCell::new_with(Some(view))),
        }
    }

    /// A native process's translator: every path is its own answer.
    pub fn shared() -> Self {
        Self::with_view(DaemonFilesystemView::Shared)
    }

    async fn view(&self) -> Result<&DaemonFilesystemView, DaemonMountError> {
        self.view
            .get_or_try_init(|| async {
                match (self.execution_environment, &self.docker) {
                    (ExecutionEnvironment::Host, _) | (_, None) => Ok(DaemonFilesystemView::Shared),
                    (ExecutionEnvironment::Docker, Some(docker)) => {
                        discover_container_view(docker).await
                    }
                }
            })
            .await
    }

    /// What to give the daemon so a workload sees `path` of this process.
    pub async fn resolve(&self, path: &Path) -> Result<DaemonMountSource, DaemonMountError> {
        self.view().await?.resolve(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container_view(mounts: Vec<OwnMount>, api_version: &str) -> DaemonFilesystemView {
        DaemonFilesystemView::Container {
            container: "temps-app".to_string(),
            mounts,
            volume_subpath_supported: supports_volume_subpath(api_version),
            api_version: api_version.to_string(),
        }
    }

    fn volume(destination: &str, name: &str) -> OwnMount {
        OwnMount {
            destination: PathBuf::from(destination),
            source: OwnMountSource::Volume(name.to_string()),
        }
    }

    fn bind(destination: &str, host_path: &str) -> OwnMount {
        OwnMount {
            destination: PathBuf::from(destination),
            source: OwnMountSource::HostPath(PathBuf::from(host_path)),
        }
    }

    #[test]
    fn test_native_process_binds_the_path_it_sees() {
        let source = DaemonFilesystemView::Shared
            .resolve(Path::new("/var/lib/temps/secrets/web"))
            .unwrap();

        assert_eq!(
            source,
            DaemonMountSource::HostPath(PathBuf::from("/var/lib/temps/secrets/web"))
        );
    }

    #[test]
    fn test_path_in_a_named_volume_becomes_a_volume_subpath() {
        let view = container_view(vec![volume("/app/data", "temps_data")], "1.51");

        let source = view.resolve(Path::new("/app/data/secrets/web-1")).unwrap();

        assert_eq!(
            source,
            DaemonMountSource::VolumeSubpath {
                volume: "temps_data".to_string(),
                subpath: Some("secrets/web-1".to_string()),
            }
        );
    }

    #[test]
    fn test_volume_root_needs_no_subpath_even_on_an_old_daemon() {
        let view = container_view(vec![volume("/app/data", "temps_data")], "1.41");

        let source = view.resolve(Path::new("/app/data")).unwrap();

        assert_eq!(
            source,
            DaemonMountSource::VolumeSubpath {
                volume: "temps_data".to_string(),
                subpath: None,
            }
        );
    }

    #[test]
    fn test_path_in_a_host_bind_is_translated_to_the_host_path() {
        let view = container_view(vec![bind("/app/data", "/srv/temps")], "1.41");

        let source = view.resolve(Path::new("/app/data/secrets/web-1")).unwrap();

        assert_eq!(
            source,
            DaemonMountSource::HostPath(PathBuf::from("/srv/temps/secrets/web-1"))
        );
    }

    #[test]
    fn test_innermost_mount_backs_the_path() {
        let view = container_view(
            vec![
                volume("/app/data", "temps_data"),
                bind("/app/data/sandboxes", "/mnt/fast/sandboxes"),
            ],
            "1.51",
        );

        let source = view
            .resolve(Path::new("/app/data/sandboxes/sbx-1"))
            .unwrap();

        assert_eq!(
            source,
            DaemonMountSource::HostPath(PathBuf::from("/mnt/fast/sandboxes/sbx-1"))
        );
    }

    #[test]
    fn test_sibling_directory_with_a_shared_prefix_is_not_on_the_mount() {
        let view = container_view(vec![volume("/app/data", "temps_data")], "1.51");

        let error = view.resolve(Path::new("/app/database/x")).unwrap_err();

        assert!(matches!(
            error,
            DaemonMountError::NotOnAMount { ref path, ref container }
                if path == "/app/database/x" && container == "temps-app"
        ));
    }

    #[test]
    fn test_path_in_the_container_layer_is_refused_with_the_fix() {
        let view = container_view(vec![volume("/app/data", "temps_data")], "1.51");

        let error = view.resolve(Path::new("/tmp/work/sbx-1")).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("/tmp/work/sbx-1"), "{message}");
        assert!(message.contains("temps-app"), "{message}");
        assert!(
            message.contains("named volume or a host directory"),
            "{message}"
        );
    }

    #[test]
    fn test_volume_subpath_on_an_old_daemon_is_refused_with_the_fix() {
        let view = container_view(vec![volume("/app/data", "temps_data")], "1.44");

        let error = view
            .resolve(Path::new("/app/data/secrets/web-1"))
            .unwrap_err();

        assert!(matches!(
            error,
            DaemonMountError::VolumeSubpathUnsupported { ref volume, ref api_version, .. }
                if volume == "temps_data" && api_version == "1.44"
        ));
        assert!(error.to_string().contains("Docker Engine 26.0"));
    }

    #[test]
    fn test_relative_path_is_refused() {
        let error = DaemonFilesystemView::Shared
            .resolve(Path::new("secrets/web"))
            .unwrap_err();

        assert!(matches!(error, DaemonMountError::RelativePath { .. }));
    }

    #[test]
    fn test_volume_subpath_api_version_boundary() {
        assert!(!supports_volume_subpath("1.44"));
        assert!(supports_volume_subpath("1.45"));
        assert!(supports_volume_subpath("1.51"));
        assert!(supports_volume_subpath("2.0"));
        assert!(!supports_volume_subpath(""));
        assert!(!supports_volume_subpath("latest"));
    }

    #[test]
    fn test_container_id_is_read_from_the_mount_table() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mountinfo = format!(
            "1200 1100 0:55 / / rw,relatime - overlay overlay rw\n\
             1201 1200 0:60 / /proc rw - proc proc rw\n\
             1210 1200 8:1 /var/lib/docker/containers/{id}/resolv.conf /etc/resolv.conf rw - ext4 /dev/sda1 rw\n\
             1211 1200 8:1 /var/lib/docker/containers/{id}/hostname /etc/hostname rw - ext4 /dev/sda1 rw\n"
        );

        assert_eq!(
            container_id_from_mountinfo(&mountinfo),
            Some(id.to_string())
        );
    }

    #[test]
    fn test_mount_table_without_a_container_names_none() {
        let mountinfo = "25 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n\
                         26 25 8:1 /var/lib/containers/short/hostname /etc/hostname rw - ext4 /dev/sda1 rw\n";

        assert_eq!(container_id_from_mountinfo(mountinfo), None);
    }

    #[test]
    fn test_own_mounts_keep_only_binds_and_named_volumes() {
        let mounts = own_mounts(vec![
            bollard::models::MountPoint {
                typ: Some("volume".to_string()),
                name: Some("temps_data".to_string()),
                source: Some("/var/lib/docker/volumes/temps_data/_data".to_string()),
                destination: Some("/app/data".to_string()),
                ..Default::default()
            },
            bollard::models::MountPoint {
                typ: Some("bind".to_string()),
                source: Some("/var/run/docker.sock".to_string()),
                destination: Some("/var/run/docker.sock".to_string()),
                ..Default::default()
            },
            bollard::models::MountPoint {
                typ: Some("tmpfs".to_string()),
                destination: Some("/tmp".to_string()),
                ..Default::default()
            },
        ]);

        assert_eq!(
            mounts,
            vec![
                volume("/app/data", "temps_data"),
                bind("/var/run/docker.sock", "/var/run/docker.sock"),
            ]
        );
    }

    #[test]
    fn test_volume_subpath_mount_never_copies_image_content() {
        let mount = DaemonMountSource::VolumeSubpath {
            volume: "temps_data".to_string(),
            subpath: Some("secrets/web-1".to_string()),
        }
        .into_mount("/run/secrets", true);

        assert_eq!(mount.typ, Some(MountType::VOLUME));
        assert_eq!(mount.source.as_deref(), Some("temps_data"));
        assert_eq!(mount.target.as_deref(), Some("/run/secrets"));
        assert_eq!(mount.read_only, Some(true));
        let options = mount.volume_options.expect("volume options");
        assert_eq!(options.subpath.as_deref(), Some("secrets/web-1"));
        assert_eq!(options.no_copy, Some(true));
    }

    #[test]
    fn test_host_path_mount_is_a_bind() {
        let mount = DaemonMountSource::HostPath(PathBuf::from("/srv/temps/secrets/web-1"))
            .into_mount("/run/secrets", true);

        assert_eq!(mount.typ, Some(MountType::BIND));
        assert_eq!(mount.source.as_deref(), Some("/srv/temps/secrets/web-1"));
        assert_eq!(mount.read_only, Some(true));
        assert!(mount.volume_options.is_none());
    }

    #[tokio::test]
    async fn test_native_translator_never_needs_a_daemon() {
        let source = DaemonMounts::shared()
            .resolve(Path::new("/var/lib/temps/secrets/web"))
            .await
            .unwrap();

        assert_eq!(
            source,
            DaemonMountSource::HostPath(PathBuf::from("/var/lib/temps/secrets/web"))
        );
    }
}
