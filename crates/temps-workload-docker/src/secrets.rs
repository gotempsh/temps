// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Secret files delivered at `/run/secrets` (mode 0700, files 0400, owned
//! by the image's `USER`). Shared by the legacy deployer and
//! [`crate::DockerDriver`] so both deliver secrets the same way.
//!
//! There are two ways to put them there, and which one works depends on
//! whether the Docker daemon sees this process's filesystem
//! ([`SecretDelivery`]):
//!
//! - a read-only bind of a per-container directory this process wrote. The
//!   bind source is resolved by the daemon, so this needs a shared
//!   filesystem: a native process;
//! - an upload into the created container through the daemon
//!   ([`upload_secrets`]). The daemon writes the files and sets their owner,
//!   so it works wherever the daemon is and needs no privilege here: a
//!   process running in a container, whose paths the daemon cannot see and
//!   which usually may not `chown`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bollard::Docker;
use tracing::warn;

/// Resolve the persistent root directory for materialized secret files.
///
/// Order of preference, mirroring the rest of the codebase
/// (`temps-sandbox`, agent session storage):
///   1. `$TEMPS_DATA_DIR/secrets`
///   2. `$HOME/.temps/secrets`
///   3. `./.temps/secrets` (last-resort fallback for headless container
///      runtimes where neither env var is set)
///
/// Crucially, NONE of these resolve to `std::env::temp_dir()`. On most
/// Linux distros `/tmp` is mounted as tmpfs and wiped at boot, which
/// caused container `/run/secrets` mounts to point at empty/missing
/// directories after a host reboot and forced a redeploy.
pub fn default_secrets_root() -> PathBuf {
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: TEMPS_DATA_DIR (with host lookup of HOME as fallback) locates the data directory and is fixed before the database exists"
    )]
    let base = std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".temps")
        });
    base.join("secrets")
}

/// How secret files reach a workload's `/run/secrets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretDelivery {
    /// Written to `<root>/<instance>` and bound read-only. The root must be
    /// a directory the Docker daemon sees at the same path, and must survive
    /// a reboot: the bind lasts the container's whole life.
    HostDirectory(PathBuf),
    /// Uploaded into the created container through the daemon, before it
    /// starts. Nothing is written on this process's side.
    Upload,
}

/// Where uploaded secrets are extracted. `/` exists in every image, and the
/// daemon creates the missing parents of an archive entry, so `/run/secrets`
/// appears even in an image built from `scratch`.
const UPLOAD_EXTRACT_DIR: &str = "/";

/// The directory of the secret files, relative to [`UPLOAD_EXTRACT_DIR`].
const UPLOAD_SECRETS_DIR: &str = "run/secrets";

/// A secret's name becomes a file name: it must be one path component.
fn validate_secret_key(key: &str) -> std::io::Result<()> {
    if key.is_empty()
        || key == "."
        || key == ".."
        || key.contains('/')
        || key.contains('\\')
        || key.contains('\0')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid secret key '{}': contains path-separator characters",
                key
            ),
        ));
    }
    Ok(())
}

/// A tar archive that extracts, at the root of a container, to
/// `/run/secrets` (mode 0700) holding one file per secret (mode 0400), all
/// owned by `owner`.
///
/// Entries are ordered by name, and carry no timestamps, so the same secrets
/// always produce the same archive.
pub fn secrets_archive(
    secrets: &HashMap<String, String>,
    owner: (u32, u32),
) -> std::io::Result<Vec<u8>> {
    let (uid, gid) = owner;
    let mut archive = tar::Builder::new(Vec::new());

    let mut directory = tar::Header::new_gnu();
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_mode(0o700);
    directory.set_uid(u64::from(uid));
    directory.set_gid(u64::from(gid));
    directory.set_size(0);
    directory.set_mtime(0);
    archive.append_data(
        &mut directory,
        format!("{UPLOAD_SECRETS_DIR}/"),
        std::io::empty(),
    )?;

    let mut keys: Vec<&String> = secrets.keys().collect();
    keys.sort();
    for key in keys {
        validate_secret_key(key)?;
        let value = secrets[key].as_bytes();
        let mut file = tar::Header::new_gnu();
        file.set_entry_type(tar::EntryType::Regular);
        file.set_mode(0o400);
        file.set_uid(u64::from(uid));
        file.set_gid(u64::from(gid));
        file.set_size(value.len() as u64);
        file.set_mtime(0);
        archive.append_data(&mut file, format!("{UPLOAD_SECRETS_DIR}/{key}"), value)?;
    }
    archive.into_inner()
}

/// Put `secrets` at `/run/secrets` of the created, not yet started
/// `container`, owned by `owner`.
///
/// The daemon extracts the archive as root and applies the owner and modes
/// it carries, so this process needs neither a filesystem the daemon can see
/// nor the right to `chown`. The files live in the container's writable
/// layer: they survive a restart and a reboot, and go when the container is
/// removed.
pub async fn upload_secrets(
    docker: &Docker,
    container: &str,
    secrets: &HashMap<String, String>,
    owner: (u32, u32),
) -> std::io::Result<()> {
    let archive = secrets_archive(secrets, owner)?;
    docker
        .upload_to_container(
            container,
            Some(bollard::query_parameters::UploadToContainerOptions {
                path: UPLOAD_EXTRACT_DIR.to_string(),
                ..Default::default()
            }),
            bollard::body_full(archive.into()),
        )
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "upload {} secret file(s) into container '{}': {}",
                secrets.len(),
                container,
                error
            ))
        })
}

pub fn validate_secret_dir_name(name: &str) -> std::io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid container name '{}': must be a single path component",
                name
            ),
        ));
    }

    Ok(())
}

/// Writes secrets as files into a per-container host directory for Docker
/// to bind-mount into `/run/secrets`. The directory is created (or recreated)
/// fresh on each call so stale entries from a previous deployment of the same
/// container name don't leak through. Directory mode 0700, file mode 0400.
///
/// When `owner` is provided, the directory and every file inside are
/// `chown`ed to that (uid, gid). Combined with 0700/0400 this means only
/// that UID inside the container can read the secrets — matching the
/// image's declared `USER` so distroless/nonroot images work without
/// world-readable files.
///
/// Rejects keys that would escape the directory (path separators, `.`, `..`)
/// to defend against a maliciously-crafted secret name.
#[cfg_attr(not(unix), allow(unused_variables))]
pub fn write_secrets_to_host_dir(
    dir: &Path,
    secrets: &HashMap<String, String>,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    // Ensure the parent (secrets root) exists. The runtime's `new()` already
    // best-effort creates it, but a hostile chmod or fresh install between
    // restarts could have left it missing — recreate so we don't fail with
    // ENOENT. We deliberately do NOT chmod the parent here: that's the
    // runtime's job, and on shared hosts the parent may be intentionally
    // owned by a different user.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }

    // Recreate fresh — wipes any stale files from a previous container with
    // the same name (rolling deploys, retries after failure, etc.).
    //
    // After a host reboot we may inherit a leaf directory whose contents
    // were chowned to the previous container's USER (e.g. nonroot uid
    // 65532) while the parent is owned by the temps user. `remove_dir_all`
    // on such a tree fails with EACCES because we can't unlink files we
    // don't own inside a directory we *do* own (sticky-bit semantics on
    // some filesystems). Reset the leaf's perms to 0700 owned-by-us first
    // so the recursive remove can succeed.
    if dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: if this chmod fails (we don't own the dir), the
            // remove below will still surface the real error.
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        if let Err(e) = fs::remove_dir_all(dir) {
            // If the leaf itself is owned by another uid (e.g. previous
            // container's USER) and we can't traverse into it, fall back
            // to renaming it aside so the deploy can proceed. The orphan
            // is logged for operator cleanup; it's not in /tmp anymore so
            // it won't auto-vanish on reboot, but it's also not blocking
            // the deploy.
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                let orphan = dir.with_extension(format!(
                    "orphan-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                fs::rename(dir, &orphan).map_err(|rename_err| {
                    std::io::Error::new(
                        rename_err.kind(),
                        format!(
                            "failed to clear stale secrets dir {} \
                             (rename to {} also failed: {}; original error: {})",
                            dir.display(),
                            orphan.display(),
                            rename_err,
                            e
                        ),
                    )
                })?;
                warn!(
                    "Renamed unowned stale secrets dir to {} so deploy could proceed; \
                     please remove manually",
                    orphan.display()
                );
            } else {
                return Err(e);
            }
        }
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    for (key, value) in secrets {
        // Keys must be plain identifiers — defense-in-depth even though
        // SecretService::validate_secret_key already enforces this.
        validate_secret_key(key)?;
        let path = dir.join(key);
        let mut f = fs::File::create(&path)?;
        f.write_all(value.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
        }
    }

    // Chown after files are written so we don't have to re-chown on every
    // write. Dir chown happens last so we still have permission to create
    // files in it while writing (when running as non-root, chown-to-self
    // is a no-op; when running as root we own it either way).
    //
    // chown(2) requires root on every sensible OS, or chown-to-self. When
    // Temps itself runs unprivileged (local macOS dev is the common case)
    // and the image runs as a non-root uid like 1000, the chown will fail
    // with EPERM. Rather than breaking the deploy, fall back to
    // world-readable permissions (0755 dir, 0444 files) so the container
    // can read its secrets. The container still has `cap_drop: ALL` and
    // `no-new-privileges`, and the bind mount is read-only — the trust
    // boundary is still the container itself.
    #[cfg(unix)]
    if let Some((uid, gid)) = owner {
        use std::os::unix::fs::{chown, PermissionsExt};

        let chown_result: std::io::Result<()> = (|| {
            for key in secrets.keys() {
                chown(dir.join(key), Some(uid), Some(gid))?;
            }
            chown(dir, Some(uid), Some(gid))?;
            Ok(())
        })();

        if let Err(e) = chown_result {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                warn!(
                    "chown of secrets dir {} to {}:{} denied (Temps is not root); \
                     falling back to world-readable mode so nonroot containers can read. \
                     Run Temps as root for strict per-uid ownership.",
                    dir.display(),
                    uid,
                    gid
                );
                // World-readable fallback. Parent dir already restricts
                // access on the host side (only the Temps user can traverse
                // into the secrets root under $TEMPS_DATA_DIR/secrets);
                // these bits are what the container sees.
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
                for key in secrets.keys() {
                    fs::set_permissions(dir.join(key), fs::Permissions::from_mode(0o444))?;
                }
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Parses a Docker image `USER` spec into a numeric `(uid, gid)` pair.
///
/// Handles the forms Docker accepts in a Dockerfile `USER` directive and
/// in `Config.User`: `""` (root), `"0"`, `"1000"`, `"1000:1000"`,
/// `"1000:gname"`, `":1000"`. Named users/groups cannot be resolved
/// without consulting the image's `/etc/passwd` — those return `None`
/// so the caller can decide whether to look them up or fall back.
///
/// When only a uid is given, gid defaults to the same value (matches
/// Docker's behavior: `USER 1000` runs as uid=1000, gid=1000).
pub fn parse_numeric_user_spec(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "root" {
        return Some((0, 0));
    }

    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let uid = if user_part.is_empty() {
        0
    } else {
        user_part.parse::<u32>().ok()?
    };

    let gid = match group_part {
        Some(g) if !g.is_empty() => g.parse::<u32>().ok()?,
        _ => uid,
    };

    Some((uid, gid))
}

/// The numeric `(uid, gid)` image `image` runs as, from its `Config.User`,
/// so its secret files can be owned by that user.
///
/// `(0, 0)` (Docker's default) when the image cannot be inspected or names
/// a user that only the image's `/etc/passwd` could resolve; both cases are
/// logged, since a nonroot image may then be unable to read its secrets.
pub async fn image_owner(docker: &Docker, image: &str) -> (u32, u32) {
    let inspect = match docker.inspect_image(image).await {
        Ok(inspect) => inspect,
        Err(e) => {
            warn!(
                "Failed to inspect image '{}' for secrets chown; defaulting to root: {}",
                image, e
            );
            return (0, 0);
        }
    };
    let user_spec = inspect
        .config
        .as_ref()
        .and_then(|c| c.user.as_deref())
        .unwrap_or("");
    match parse_numeric_user_spec(user_spec) {
        Some(pair) => pair,
        None => {
            warn!(
                "Image '{}' declares USER='{}' which is not numeric; \
                 secrets will be owned by root and may be unreadable. \
                 Use numeric UIDs (e.g. USER 1000:1000) for secrets \
                 to work with this image.",
                image, user_spec
            );
            (0, 0)
        }
    }
}

/// Largest `/etc/passwd` or `/etc/group` read from a container to resolve a
/// named `USER`. Real account files are a few kilobytes; anything larger is
/// not read, and the owner falls back as for an unresolvable name.
const MAX_ACCOUNT_FILE_BYTES: usize = 1024 * 1024;

/// `(uid, primary gid)` of account `name` in passwd-format `passwd`.
fn passwd_lookup(passwd: &str, name: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        let _password = fields.next()?;
        let uid = fields.next()?.trim().parse().ok()?;
        let gid = fields.next()?.trim().parse().ok()?;
        Some((uid, gid))
    })
}

/// Primary gid of the account with `uid` in passwd-format `passwd`.
fn passwd_primary_gid(passwd: &str, uid: u32) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':').skip(2);
        let entry_uid: u32 = fields.next()?.trim().parse().ok()?;
        if entry_uid != uid {
            return None;
        }
        fields.next()?.trim().parse().ok()
    })
}

/// gid of group `name` in group-format `group`.
fn group_lookup(group: &str, name: &str) -> Option<u32> {
    group.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        let _password = fields.next()?;
        fields.next()?.trim().parse().ok()
    })
}

/// Resolve a `USER` spec to `(uid, gid)` the way the container runtime
/// does, using the image's own account files for names.
///
/// A user given by name takes its uid and primary group from `passwd`; a
/// user given by number takes its primary group from `passwd` when listed
/// there, else the uid. A group given by name is looked up in `group`.
/// `None` when a name is not listed (or its file could not be read).
pub fn resolve_user_spec(
    spec: &str,
    passwd: Option<&str>,
    group: Option<&str>,
) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Some((0, 0));
    }
    let (user_part, group_part) = match spec.split_once(':') {
        Some((user, group)) => (user, Some(group)),
        None => (spec, None),
    };

    let (uid, primary_gid) = if user_part.is_empty() {
        (0, 0)
    } else if let Ok(uid) = user_part.parse::<u32>() {
        let gid = passwd
            .and_then(|passwd| passwd_primary_gid(passwd, uid))
            .unwrap_or(uid);
        (uid, gid)
    } else if user_part == "root" {
        (0, 0)
    } else {
        passwd_lookup(passwd?, user_part)?
    };

    let gid = match group_part {
        None | Some("") => primary_gid,
        Some(name) => match name.parse::<u32>() {
            Ok(gid) => gid,
            Err(_) => group_lookup(group?, name)?,
        },
    };
    Some((uid, gid))
}

/// Contents of the regular file at `path` in `container` (created or
/// running), through the daemon's archive API. `None` when it is missing,
/// not a regular file, larger than [`MAX_ACCOUNT_FILE_BYTES`] or unreadable.
async fn read_container_file(docker: &Docker, container: &str, path: &str) -> Option<String> {
    use futures::StreamExt;
    use std::io::Read;

    let mut stream = docker.download_from_container(
        container,
        Some(bollard::query_parameters::DownloadFromContainerOptions {
            path: path.to_string(),
        }),
    );
    // The archive holds one entry: a header block, the file and padding.
    let limit = MAX_ACCOUNT_FILE_BYTES + 64 * 1024;
    let mut archive = Vec::new();
    while let Some(chunk) = stream.next().await {
        archive.extend_from_slice(&chunk.ok()?);
        if archive.len() > limit {
            return None;
        }
    }

    let mut reader = tar::Archive::new(archive.as_slice());
    let mut entry = reader.entries().ok()?.next()?.ok()?;
    if !entry.header().entry_type().is_file() {
        return None;
    }
    let mut contents = String::new();
    entry
        .by_ref()
        .take(MAX_ACCOUNT_FILE_BYTES as u64)
        .read_to_string(&mut contents)
        .ok()?;
    Some(contents)
}

/// The `(uid, gid)` created container `container` will run as, so secrets
/// uploaded into it can be owned by that user.
///
/// Unlike [`image_owner`], this resolves a named `USER` (`USER node`) from
/// the container's own `/etc/passwd` and `/etc/group`, which the daemon can
/// read before the container starts. Falls back to `(0, 0)`, logged, when
/// the container cannot be inspected or the name is not listed there.
pub async fn container_owner(docker: &Docker, container: &str) -> (u32, u32) {
    let inspect = match docker
        .inspect_container(
            container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(inspect) => inspect,
        Err(e) => {
            warn!(
                "Failed to inspect container '{}' for secrets ownership; defaulting to root: {}",
                container, e
            );
            return (0, 0);
        }
    };
    let user_spec = inspect
        .config
        .as_ref()
        .and_then(|c| c.user.clone())
        .unwrap_or_default();
    let (user_part, group_part) = match user_spec.trim().split_once(':') {
        Some((user, group)) => (user, Some(group)),
        None => (user_spec.trim(), None),
    };
    let numeric_group = group_part.is_some_and(|group| group.parse::<u32>().is_ok());
    if group_part.is_none() && (user_part.is_empty() || user_part == "root") {
        return (0, 0);
    }
    if user_part.parse::<u32>().is_ok() && numeric_group {
        // Fully numeric `uid:gid`: no account file needed.
        return resolve_user_spec(&user_spec, None, None).unwrap_or((0, 0));
    }

    let passwd = read_container_file(docker, container, "/etc/passwd").await;
    let group = match group_part {
        Some(group) if !group.is_empty() && !numeric_group => {
            read_container_file(docker, container, "/etc/group").await
        }
        _ => None,
    };
    match resolve_user_spec(&user_spec, passwd.as_deref(), group.as_deref()) {
        Some(owner) => owner,
        None => {
            warn!(
                "Container '{}' runs as USER='{}', which its /etc/passwd and /etc/group do \
                 not resolve; secrets will be owned by root and may be unreadable. Use a \
                 numeric USER (e.g. USER 1000:1000) for this image.",
                container, user_spec
            );
            (0, 0)
        }
    }
}

/// The bind that mounts `dir` read-only at `/run/secrets`.
pub fn secrets_bind(dir: &Path) -> String {
    format!("{}:/run/secrets:ro", dir.display())
}

/// Remove `dir` (a container's secrets directory) if it exists. Best
/// effort: a failure is logged and left for the next deploy of the same
/// name, which recreates the directory fresh.
pub fn remove_secrets_dir(dir: &Path) {
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            warn!(
                "Failed to clean up secrets host dir {}: {}",
                dir.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// (path, is_dir, mode, uid, gid, contents) of every archive entry.
    fn archive_entries(archive: &[u8]) -> Vec<(String, bool, u32, u64, u64, String)> {
        use std::io::Read;

        let mut reader = tar::Archive::new(archive);
        reader
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let header = entry.header().clone();
                let mut contents = String::new();
                entry.read_to_string(&mut contents).unwrap();
                (
                    entry.path().unwrap().to_string_lossy().into_owned(),
                    header.entry_type().is_dir(),
                    header.mode().unwrap(),
                    header.uid().unwrap(),
                    header.gid().unwrap(),
                    contents,
                )
            })
            .collect()
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\n\
                          daemon:x:1:1::/usr/sbin:/usr/sbin/nologin\n\
                          node:x:1000:1000::/home/node:/bin/sh\n\
                          app:x:10001:2000::/app:/bin/sh\n";
    const GROUP: &str = "root:x:0:\nnode:x:1000:\napps:x:2000:app\nshared:x:3000:node,app\n";

    #[test]
    fn test_named_user_resolves_to_uid_and_primary_group() {
        assert_eq!(
            resolve_user_spec("node", Some(PASSWD), None),
            Some((1000, 1000))
        );
        assert_eq!(
            resolve_user_spec("app", Some(PASSWD), None),
            Some((10001, 2000))
        );
    }

    #[test]
    fn test_numeric_user_takes_primary_group_from_passwd_when_listed() {
        assert_eq!(
            resolve_user_spec("10001", Some(PASSWD), None),
            Some((10001, 2000))
        );
        // Not listed: the group defaults to the uid, as before.
        assert_eq!(
            resolve_user_spec("4242", Some(PASSWD), None),
            Some((4242, 4242))
        );
        assert_eq!(resolve_user_spec("4242", None, None), Some((4242, 4242)));
    }

    #[test]
    fn test_explicit_group_by_name_or_number_overrides_primary_group() {
        assert_eq!(
            resolve_user_spec("node:shared", Some(PASSWD), Some(GROUP)),
            Some((1000, 3000))
        );
        assert_eq!(
            resolve_user_spec("node:55", Some(PASSWD), None),
            Some((1000, 55))
        );
        assert_eq!(
            resolve_user_spec("1000:1000", None, None),
            Some((1000, 1000))
        );
    }

    #[test]
    fn test_root_and_empty_user_are_root() {
        assert_eq!(resolve_user_spec("", None, None), Some((0, 0)));
        assert_eq!(resolve_user_spec("root", None, None), Some((0, 0)));
    }

    #[test]
    fn test_unlisted_names_or_missing_files_do_not_resolve() {
        assert_eq!(resolve_user_spec("ghost", Some(PASSWD), None), None);
        assert_eq!(resolve_user_spec("node", None, None), None);
        assert_eq!(
            resolve_user_spec("node:nogroup", Some(PASSWD), Some(GROUP)),
            None
        );
        assert_eq!(resolve_user_spec("node:shared", Some(PASSWD), None), None);
    }

    #[test]
    fn test_malformed_account_lines_are_skipped() {
        let passwd = "node\nnode:x:notanumber:1\nnode:x:1000:1000::/home/node:/bin/sh\n";

        assert_eq!(
            resolve_user_spec("node", Some(passwd), None),
            Some((1000, 1000))
        );
        assert_eq!(
            resolve_user_spec("1000", Some(passwd), None),
            Some((1000, 1000))
        );
    }

    #[tokio::test]
    async fn test_container_owner_falls_back_to_root_when_docker_is_unreachable() {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap();

        assert_eq!(container_owner(&docker, "missing").await, (0, 0));
    }

    #[test]
    fn test_secrets_archive_holds_a_private_directory_owned_by_the_image_user() {
        let secrets = HashMap::from([
            ("DB_PASSWORD".to_string(), "s3cret".to_string()),
            ("API_KEY".to_string(), "abc\ndef".to_string()),
        ]);

        let archive = secrets_archive(&secrets, (65532, 65532)).unwrap();

        assert_eq!(
            archive_entries(&archive),
            vec![
                (
                    "run/secrets/".to_string(),
                    true,
                    0o700,
                    65532,
                    65532,
                    String::new()
                ),
                (
                    "run/secrets/API_KEY".to_string(),
                    false,
                    0o400,
                    65532,
                    65532,
                    "abc\ndef".to_string()
                ),
                (
                    "run/secrets/DB_PASSWORD".to_string(),
                    false,
                    0o400,
                    65532,
                    65532,
                    "s3cret".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_secrets_archive_is_the_same_for_the_same_secrets() {
        let secrets = HashMap::from([
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
            ("C".to_string(), "3".to_string()),
        ]);

        assert_eq!(
            secrets_archive(&secrets, (0, 0)).unwrap(),
            secrets_archive(&secrets.clone(), (0, 0)).unwrap()
        );
    }

    #[test]
    fn test_secrets_archive_refuses_a_key_that_escapes_the_directory() {
        for key in ["../etc/passwd", "a/b", "", ".", ".."] {
            let secrets = HashMap::from([(key.to_string(), "v".to_string())]);

            let error = secrets_archive(&secrets, (0, 0)).unwrap_err();

            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{key:?}");
            assert!(error.to_string().contains("invalid secret key"), "{key:?}");
        }
    }

    #[test]
    fn test_secrets_archive_of_no_secrets_still_creates_the_directory() {
        let archive = secrets_archive(&HashMap::new(), (1000, 1000)).unwrap();

        let entries = archive_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "run/secrets/");
        assert!(entries[0].1);
    }

    #[test]
    fn test_write_secrets_to_host_dir_creates_files_with_correct_perms() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c1");

        let mut secrets = HashMap::new();
        secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());
        secrets.insert("API_KEY".to_string(), "abc\ndef".to_string());

        write_secrets_to_host_dir(&dir, &secrets, None).expect("write");

        let db = std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap();
        assert_eq!(db, "s3cret");
        let api = std::fs::read_to_string(dir.join("API_KEY")).unwrap();
        assert_eq!(api, "abc\ndef");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_overwrites_stale_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c2");

        let mut first = HashMap::new();
        first.insert("OLD".to_string(), "old".to_string());
        write_secrets_to_host_dir(&dir, &first, None).unwrap();
        assert!(dir.join("OLD").exists());

        let mut second = HashMap::new();
        second.insert("NEW".to_string(), "new".to_string());
        write_secrets_to_host_dir(&dir, &second, None).unwrap();
        // Stale file from first call must be gone
        assert!(!dir.join("OLD").exists());
        assert!(dir.join("NEW").exists());
    }

    #[test]
    fn test_default_secrets_root_uses_temps_data_dir() {
        // Smoke test the env precedence. We can't unset HOME without
        // breaking other tests, so verify TEMPS_DATA_DIR wins when set.
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::set_var("TEMPS_DATA_DIR", tmp.path());
        let root = default_secrets_root();
        assert_eq!(root, tmp.path().join("secrets"));
        // Critically, this must NOT live under /tmp (unless TEMPS_DATA_DIR
        // points there explicitly) — that was the bug we just fixed.
        match prev {
            Some(v) => std::env::set_var("TEMPS_DATA_DIR", v),
            None => std::env::remove_var("TEMPS_DATA_DIR"),
        }
    }

    #[test]
    fn test_default_secrets_root_is_persistent_path() {
        // Without TEMPS_DATA_DIR, the resolved root must live under
        // $HOME/.temps/secrets — i.e. NOT under std::env::temp_dir(),
        // which is tmpfs on most Linux distros and was the source of
        // the post-reboot redeploy regression.
        let prev_data = std::env::var("TEMPS_DATA_DIR").ok();
        std::env::remove_var("TEMPS_DATA_DIR");
        let root = default_secrets_root();
        assert!(
            !root.starts_with(std::env::temp_dir()),
            "secrets root unexpectedly resolved to a tmpfs path: {}",
            root.display()
        );
        if let Some(v) = prev_data {
            std::env::set_var("TEMPS_DATA_DIR", v);
        }
    }

    #[test]
    fn test_validate_secret_dir_name_rejects_path_traversal_components() {
        for bad in [
            "",
            ".",
            "..",
            "../escaped/deployment-1",
            "a/b",
            "a\\b",
            "with\0null",
        ] {
            let err = validate_secret_dir_name(bad).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "name={}", bad);
        }

        validate_secret_dir_name("project-1").unwrap();
    }

    #[test]
    fn test_write_secrets_to_host_dir_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c3");

        for bad in ["..", ".", "../escape", "a/b", "a\\b", "with\0null"] {
            let mut secrets = HashMap::new();
            secrets.insert(bad.to_string(), "v".to_string());
            let err = write_secrets_to_host_dir(&dir, &secrets, None).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "key={}", bad);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_empty_creates_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("c4");
        let secrets: HashMap<String, String> = HashMap::new();
        write_secrets_to_host_dir(&dir, &secrets, None).unwrap();
        assert!(dir.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_numeric_user_spec() {
        // Empty / root → (0, 0)
        assert_eq!(parse_numeric_user_spec(""), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("  "), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("root"), Some((0, 0)));
        assert_eq!(parse_numeric_user_spec("0"), Some((0, 0)));

        // Uid only → gid defaults to uid (Docker's behavior)
        assert_eq!(parse_numeric_user_spec("1000"), Some((1000, 1000)));
        assert_eq!(parse_numeric_user_spec("65532"), Some((65532, 65532)));

        // uid:gid
        assert_eq!(parse_numeric_user_spec("1000:2000"), Some((1000, 2000)));
        assert_eq!(parse_numeric_user_spec("65532:65532"), Some((65532, 65532)));

        // :gid (uid defaults to 0 — matches Docker)
        assert_eq!(parse_numeric_user_spec(":1000"), Some((0, 1000)));

        // uid: (trailing colon → gid falls back to uid)
        assert_eq!(parse_numeric_user_spec("1000:"), Some((1000, 1000)));

        // Named users/groups are not numeric — caller falls back
        assert_eq!(parse_numeric_user_spec("node"), None);
        assert_eq!(parse_numeric_user_spec("node:node"), None);
        assert_eq!(parse_numeric_user_spec("1000:node"), None);
        assert_eq!(parse_numeric_user_spec("node:1000"), None);

        // Malformed
        assert_eq!(parse_numeric_user_spec("abc"), None);
        assert_eq!(parse_numeric_user_spec("-1"), None);
        assert_eq!(parse_numeric_user_spec("1:2:3"), None);
    }

    #[test]
    fn test_write_secrets_to_host_dir_chown_to_self_is_noop() {
        // chown(2) only succeeds when chown-ing to yourself (unless root).
        // We verify the happy path by chown-ing to our own uid/gid: the
        // function must succeed and leave the mode bits intact. This guards
        // against the chown being accidentally reordered before the file
        // writes (which would fail) or changing the permission bits.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let dir = tmp.path().join("c5");

            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            let my_gid = std::fs::metadata(tmp.path()).unwrap().gid();

            write_secrets_to_host_dir(&dir, &secrets, Some((my_uid, my_gid))).unwrap();

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o400);
        }
    }

    #[test]
    fn test_write_secrets_to_host_dir_falls_back_to_world_readable_on_eperm() {
        // When Temps runs unprivileged and is asked to chown to a uid that
        // isn't its own, chown returns EPERM. The function must not fail
        // the deploy — it must downgrade permissions so the container can
        // still read its secrets. Only runs as non-root (skipped under sudo).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            use std::os::unix::fs::PermissionsExt;

            let tmp = TempDir::new().unwrap();
            let my_uid = std::fs::metadata(tmp.path()).unwrap().uid();
            if my_uid == 0 {
                eprintln!("running as root; skipping EPERM fallback test");
                return;
            }

            let dir = tmp.path().join("c6");
            let mut secrets = HashMap::new();
            secrets.insert("DB_PASSWORD".to_string(), "s3cret".to_string());

            // Ask for a uid/gid we definitely don't own — forces EPERM.
            write_secrets_to_host_dir(&dir, &secrets, Some((65532, 65532)))
                .expect("fallback must succeed, not error");

            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o755, "dir must be world-traversable on fallback");

            let file_mode = std::fs::metadata(dir.join("DB_PASSWORD"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o444, "file must be world-readable on fallback");

            // Content still intact.
            assert_eq!(
                std::fs::read_to_string(dir.join("DB_PASSWORD")).unwrap(),
                "s3cret"
            );
        }
    }
}
