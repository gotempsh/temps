// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolving an OCI image's `USER` the way Docker does.
//!
//! A container runtime turns `USER nextjs` into a uid and gid by reading the
//! *image's* `/etc/passwd` and `/etc/group`. Two callers need that answer
//! before anything runs: the microVM driver, whose guest agent only takes
//! numbers, and the deploy pipeline, which warns when a deployment's image
//! runs as root. Both use this module, so they can never disagree.
//!
//! - [`resolve_image_user`] is pure: a `USER` string plus the text of the two
//!   files in, a [`ResolvedUser`] out. Its rules were taken from what Docker
//!   itself reports for each form (see the table on that function), so a
//!   microVM replica runs as the same identity a container of the same image
//!   would. [`classify_image_user`] reduces that to root, non-root, or
//!   unresolved.
//! - [`read_identity_files`] fetches the files through any source of path
//!   archives (bounded, symlink-safe, reading only what the `USER` needs);
//!   [`read_image_identity`] is that over the Docker daemon's archive API,
//!   from a container that is created and never started, which is also how
//!   the rootfs conversion materialises an image, so no new host tool and no
//!   parsing of an untrusted filesystem is involved.

use std::collections::BTreeSet;
use std::io::Read;

use bollard::Docker;
use futures::StreamExt;

/// The most a single identity file may weigh. `/etc/passwd` of a real image
/// is a few hundred bytes; a megabyte is orders of magnitude of slack and
/// keeps a hostile image from making the control plane buffer something huge.
pub const MAX_IDENTITY_FILE_BYTES: usize = 1 << 20;

/// Tar framing around a single file: its header block plus padding. Added to
/// [`MAX_IDENTITY_FILE_BYTES`] when capping the archive stream.
const ARCHIVE_OVERHEAD_BYTES: usize = 16 * 1024;

/// How many symlinks are followed to reach `/etc/passwd` or `/etc/group`
/// (some immutable distributions point them into `/usr/etc`). Bounded so a
/// link cycle fails instead of looping.
const MAX_SYMLINK_HOPS: usize = 8;

const PASSWD_PATH: &str = "/etc/passwd";
const GROUP_PATH: &str = "/etc/group";

/// The identity a replica's process runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUser {
    pub uid: u32,
    /// The primary group.
    pub gid: u32,
    /// Every group Docker would put the process in: the primary group plus,
    /// for a user given without an explicit group, each group in
    /// `/etc/group` that lists the user as a member. Sorted, no duplicates.
    pub groups: Vec<u32>,
    /// The user's home directory from `/etc/passwd`; `/` when the user has no
    /// entry, which is what Docker sets.
    pub home: String,
}

/// Why a `USER` could not be resolved. Every message names the spec and the
/// image file it was looked up in.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ImageUserError {
    #[error(
        "USER '{spec}' is not a valid user specification: {reason}; use `name`, `uid`, \
         `name:group`, `uid:gid`, or the like"
    )]
    Malformed { spec: String, reason: &'static str },

    #[error(
        "USER '{spec}': no user '{user}' in the image's /etc/passwd ({passwd}); use a numeric \
         `uid` or `uid:gid`, or add the user to the image"
    )]
    UserNotFound {
        spec: String,
        user: String,
        passwd: &'static str,
    },

    #[error(
        "USER '{spec}': no group '{group}' in the image's /etc/group ({group_file}); use a \
         numeric gid, or add the group to the image"
    )]
    GroupNotFound {
        spec: String,
        group: String,
        group_file: &'static str,
    },
}

/// Why an image's identity files could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageFileError {
    #[error("reading {path} from image {image}: {reason}")]
    Docker {
        image: String,
        path: String,
        reason: String,
    },

    #[error(
        "{path} in image {image} is larger than {limit} bytes; refusing to read an identity \
         file that size"
    )]
    TooLarge {
        image: String,
        path: String,
        limit: usize,
    },

    #[error("{path} in image {image} is not a readable file: {reason}")]
    NotReadable {
        image: String,
        path: String,
        reason: String,
    },

    #[error("{path} in image {image} is behind more than {limit} symbolic links")]
    SymlinkLoop {
        image: String,
        path: String,
        limit: usize,
    },
}

/// The text of an image's `/etc/passwd` and `/etc/group`; `None` when the
/// image has no such file (a `scratch` image, a static binary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageIdentity {
    pub passwd: Option<String>,
    pub group: Option<String>,
}

struct PasswdEntry<'a> {
    name: &'a str,
    uid: u32,
    gid: u32,
    home: &'a str,
}

struct GroupEntry<'a> {
    name: &'a str,
    gid: u32,
    members: Vec<&'a str>,
}

/// `name:password:uid:gid:gecos:home:shell`. Blank lines, comments and
/// malformed lines are skipped, as `getpwent` does.
fn parse_passwd(text: &str) -> Vec<PasswdEntry<'_>> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next()?;
            let uid = fields.next()?.parse().ok()?;
            let gid = fields.next()?.parse().ok()?;
            let _gecos = fields.next()?;
            let home = fields.next()?;
            (!name.is_empty()).then_some(PasswdEntry {
                name,
                uid,
                gid,
                home,
            })
        })
        .collect()
}

/// `name:password:gid:member,member`.
fn parse_group(text: &str) -> Vec<GroupEntry<'_>> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next()?;
            let gid = fields.next()?.parse().ok()?;
            let members = fields
                .next()
                .unwrap_or_default()
                .split(',')
                .filter(|member| !member.is_empty())
                .collect();
            (!name.is_empty()).then_some(GroupEntry { name, gid, members })
        })
        .collect()
}

fn presence(file: Option<&str>) -> &'static str {
    if file.is_some() {
        "not listed"
    } else {
        "the image has no such file"
    }
}

/// Resolve a Docker `USER` string against an image's identity files.
///
/// What Docker reports for each form (checked against a real daemon, and
/// what the tests below pin):
///
/// | `USER`                         | uid / gid                        | groups                                   | `HOME`           |
/// |--------------------------------|----------------------------------|------------------------------------------|------------------|
/// | `name`, or a `uid` in passwd   | passwd uid / primary gid         | primary + every group listing the user   | passwd home      |
/// | `uid` not in passwd            | uid / `0`                        | `0`                                      | `/`              |
/// | `name:group`, `uid:gid`, ...   | as given (group looked up by name or number) | just that gid (no membership groups) | passwd home, else `/` |
///
/// A numeric user is always a uid and a numeric group always a gid, looked
/// up by id only (an entry literally *named* `7` never matches `USER 7`), and
/// kept as a bare number when the file has no such entry. Any other user or
/// group is a name that must exist in the image's file. `root` resolves to
/// `0:0` even when the image ships no passwd.
pub fn resolve_image_user(
    spec: &str,
    passwd: Option<&str>,
    group: Option<&str>,
) -> Result<ResolvedUser, ImageUserError> {
    let trimmed = spec.trim();
    let malformed = |reason| ImageUserError::Malformed {
        spec: spec.to_string(),
        reason,
    };
    let (user_part, group_part) = match trimmed.split_once(':') {
        Some((user, group)) => (user, Some(group)),
        None => (trimmed, None),
    };
    if user_part.is_empty() {
        return Err(malformed("the user part is empty"));
    }
    if group_part.is_some_and(str::is_empty) {
        return Err(malformed("the group part after ':' is empty"));
    }
    if group_part.is_some_and(|group| group.contains(':')) {
        return Err(malformed("it has more than one ':'"));
    }

    let passwd_entries = passwd.map(parse_passwd).unwrap_or_default();
    let group_entries = group.map(parse_group).unwrap_or_default();

    let numeric_user = user_part.parse::<u32>().ok();
    let entry = match numeric_user {
        Some(uid) => passwd_entries.iter().find(|entry| entry.uid == uid),
        None => passwd_entries.iter().find(|entry| entry.name == user_part),
    };
    let uid = match (entry, numeric_user) {
        (Some(entry), _) => entry.uid,
        (None, Some(uid)) => uid,
        (None, None) if user_part == "root" => 0,
        (None, None) => {
            return Err(ImageUserError::UserNotFound {
                spec: spec.to_string(),
                user: user_part.to_string(),
                passwd: presence(passwd),
            })
        }
    };

    let (gid, groups) = match group_part {
        Some(group_name) => {
            let gid = match group_name.parse::<u32>() {
                Ok(gid) => gid,
                Err(_) => group_entries
                    .iter()
                    .find(|entry| entry.name == group_name)
                    .map(|entry| entry.gid)
                    .ok_or_else(|| ImageUserError::GroupNotFound {
                        spec: spec.to_string(),
                        group: group_name.to_string(),
                        group_file: presence(group),
                    })?,
            };
            (gid, vec![gid])
        }
        None => {
            let gid = entry.map_or(0, |entry| entry.gid);
            let mut groups = BTreeSet::from([gid]);
            if let Some(entry) = entry {
                groups.extend(
                    group_entries
                        .iter()
                        .filter(|group| group.members.contains(&entry.name))
                        .map(|group| group.gid),
                );
            }
            (gid, groups.into_iter().collect())
        }
    };

    let home = entry
        .map(|entry| entry.home)
        .filter(|home| !home.is_empty())
        .unwrap_or("/")
        .to_string();
    Ok(ResolvedUser {
        uid,
        gid,
        groups,
        home,
    })
}

/// `USER` split into its user and optional group part, trimmed.
fn split_spec(spec: &str) -> (&str, Option<&str>) {
    let trimmed = spec.trim();
    match trimmed.split_once(':') {
        Some((user, group)) => (user, Some(group)),
        None => (trimmed, None),
    }
}

/// Whether an image's `USER` makes its process the superuser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageUserVerdict {
    /// uid 0: the image declares no `USER` (Docker's default), `root`, `0`,
    /// `0:<any group>`, or a name the image's `/etc/passwd` maps to uid 0.
    Root,
    /// Any other uid.
    NonRoot { uid: u32 },
    /// A user or group the image's own files do not resolve, or a malformed
    /// `USER`. Not reported as root: the runtime refuses to start such an
    /// image, so what it would run as is unknown, not uid 0.
    Unresolved(ImageUserError),
}

impl ImageUserVerdict {
    pub fn is_root(&self) -> bool {
        matches!(self, Self::Root)
    }
}

/// Classify `spec` (an image's `Config.User`, `""` when it declares none)
/// against the identity files [`IdentityFilesNeeded::to_classify`] asked for.
///
/// Uses [`resolve_image_user`], the same lookup a microVM replica runs as,
/// so the verdict matches what the replica actually gets.
pub fn classify_image_user(spec: &str, identity: &ImageIdentity) -> ImageUserVerdict {
    if spec.trim().is_empty() {
        return ImageUserVerdict::Root;
    }
    let passwd = identity.passwd.as_deref();
    match resolve_image_user(spec, passwd, identity.group.as_deref()) {
        Ok(resolved) if resolved.uid == 0 => ImageUserVerdict::Root,
        Ok(resolved) => ImageUserVerdict::NonRoot { uid: resolved.uid },
        // The uid alone makes the superuser: `0:staff` or `root:staff` in an
        // image without a `staff` group is still uid 0 (unless the image's
        // passwd maps `root` elsewhere, which the user-only lookup reads).
        Err(error @ ImageUserError::GroupNotFound { .. }) => {
            let (user, _) = split_spec(spec);
            match resolve_image_user(user, passwd, None) {
                Ok(resolved) if resolved.uid == 0 => ImageUserVerdict::Root,
                _ => ImageUserVerdict::Unresolved(error),
            }
        }
        Err(error) => ImageUserVerdict::Unresolved(error),
    }
}

/// What the first entry of a `docker cp`-style archive is.
#[derive(Debug, PartialEq, Eq)]
enum ArchiveFile {
    Regular(Vec<u8>),
    /// A symbolic link, with its target as written.
    Symlink(String),
}

/// Read the first entry of a tar archive holding one path. `Ok(None)` for an
/// empty archive. Content over [`MAX_IDENTITY_FILE_BYTES`] is refused before
/// it is read.
fn first_archive_file(
    tar_bytes: &[u8],
    image: &str,
    path: &str,
) -> Result<Option<ArchiveFile>, ImageFileError> {
    let not_readable = |reason: String| ImageFileError::NotReadable {
        image: image.to_string(),
        path: path.to_string(),
        reason,
    };
    let mut archive = tar::Archive::new(tar_bytes);
    let mut entries = archive
        .entries()
        .map_err(|e| not_readable(format!("the archive is unreadable: {e}")))?;
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    let mut entry = entry.map_err(|e| not_readable(format!("the archive is unreadable: {e}")))?;
    let kind = entry.header().entry_type();
    if kind.is_symlink() {
        let target = entry
            .link_name()
            .map_err(|e| not_readable(format!("its link target is unreadable: {e}")))?
            .ok_or_else(|| not_readable("it is a symbolic link with no target".to_string()))?;
        return Ok(Some(ArchiveFile::Symlink(
            target.to_string_lossy().into_owned(),
        )));
    }
    if !kind.is_file() {
        return Err(not_readable(format!(
            "it is not a regular file (tar entry type {kind:?})"
        )));
    }
    if entry.size() > MAX_IDENTITY_FILE_BYTES as u64 {
        return Err(ImageFileError::TooLarge {
            image: image.to_string(),
            path: path.to_string(),
            limit: MAX_IDENTITY_FILE_BYTES,
        });
    }
    let declared = entry.size();
    let mut contents = Vec::with_capacity(declared as usize);
    entry
        .read_to_end(&mut contents)
        .map_err(|e| not_readable(format!("reading its contents failed: {e}")))?;
    // A truncated archive can end inside the payload without the reader
    // reporting it; a short file would read as a passwd missing the user.
    if contents.len() as u64 != declared {
        return Err(not_readable(format!(
            "the archive holds {} of its {declared} bytes",
            contents.len()
        )));
    }
    Ok(Some(ArchiveFile::Regular(contents)))
}

/// Where a symlink found at `from` points, as an absolute path inside the
/// image. `..` stops at the root, as in a chroot, so a link can never name
/// anything outside the image.
///
/// `None` for a target that steps back out of a directory it names itself
/// (`a/link/../passwd`): the runtime resolves `link` before applying `..`,
/// and the archive exports only take clean paths, so collapsing it here
/// could read a different file than the container would. Leading `..`
/// (`../usr/etc/passwd`, the usual layout) is resolved.
fn resolve_link_target(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut dir: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
        dir.pop();
        dir
    };
    let mut named_a_directory = false;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." if named_a_directory => return None,
            ".." => {
                parts.pop();
            }
            name => {
                named_a_directory = true;
                parts.push(name);
            }
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// What fetching one path out of an image produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchedPath {
    /// The image has nothing at this path.
    Missing,
    /// The `docker cp`-style tar archive of the path, at most
    /// [`MAX_IDENTITY_ARCHIVE_BYTES`] long.
    Archive(Vec<u8>),
}

/// The most a fetched archive of one identity file may weigh: the file cap
/// plus its tar framing.
pub const MAX_IDENTITY_ARCHIVE_BYTES: usize = MAX_IDENTITY_FILE_BYTES + ARCHIVE_OVERHEAD_BYTES;

/// Collect a path archive from `stream`, refusing it once it passes
/// [`MAX_IDENTITY_ARCHIVE_BYTES`] instead of buffering the rest.
///
/// For a fetcher handed to [`read_identity_files`] whose bytes arrive as a
/// stream (the daemon's archive API, a worker agent's path export).
pub async fn collect_identity_archive<S, B, E>(
    mut stream: S,
    image: &str,
    path: &str,
) -> Result<Vec<u8>, ImageFileError>
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    let mut tar_bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ImageFileError::Docker {
            image: image.to_string(),
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        let chunk = chunk.as_ref();
        // Checked before copying, so one oversized chunk is never buffered.
        if chunk.len() > MAX_IDENTITY_ARCHIVE_BYTES - tar_bytes.len() {
            return Err(ImageFileError::TooLarge {
                image: image.to_string(),
                path: path.to_string(),
                limit: MAX_IDENTITY_FILE_BYTES,
            });
        }
        tar_bytes.extend_from_slice(chunk);
    }
    Ok(tar_bytes)
}

/// Read one file of `image` through `fetch`, following symlinks inside the
/// image. `Ok(None)` when the image has no such file.
async fn read_image_file<F, Fut>(
    image: &str,
    path: &str,
    fetch: &mut F,
) -> Result<Option<String>, ImageFileError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<FetchedPath, ImageFileError>>,
{
    let mut current = path.to_string();
    for _ in 0..=MAX_SYMLINK_HOPS {
        let tar_bytes = match fetch(current.clone()).await? {
            FetchedPath::Missing => return Ok(None),
            FetchedPath::Archive(bytes) => bytes,
        };
        if tar_bytes.len() > MAX_IDENTITY_ARCHIVE_BYTES {
            return Err(ImageFileError::TooLarge {
                image: image.to_string(),
                path: current,
                limit: MAX_IDENTITY_FILE_BYTES,
            });
        }
        match first_archive_file(&tar_bytes, image, &current)? {
            None => return Ok(None),
            Some(ArchiveFile::Regular(contents)) => {
                return Ok(Some(String::from_utf8_lossy(&contents).into_owned()))
            }
            Some(ArchiveFile::Symlink(target)) => {
                current = resolve_link_target(&current, &target).ok_or_else(|| {
                    ImageFileError::NotReadable {
                        image: image.to_string(),
                        path: current.clone(),
                        reason: format!(
                            "it links to '{target}', which steps back out of a directory it \
                             names; resolving that needs the image's own symlinks"
                        ),
                    }
                })?;
            }
        }
    }
    Err(ImageFileError::SymlinkLoop {
        image: image.to_string(),
        path: path.to_string(),
        limit: MAX_SYMLINK_HOPS,
    })
}

/// Which identity files resolving a `USER` needs. Reading neither is the
/// common case: no `USER`, `root`, or a numeric `uid[:gid]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentityFilesNeeded {
    pub passwd: bool,
    pub group: bool,
}

impl IdentityFilesNeeded {
    /// Both files, for a caller that needs the full identity (supplementary
    /// groups and the home directory), as a microVM replica does.
    pub const ALL: Self = Self {
        passwd: true,
        group: true,
    };

    /// The files [`classify_image_user`] reads for `spec`: `/etc/passwd`
    /// only for a user given by name (`root` included: the image's passwd
    /// decides its uid, as it does for the runtime), `/etc/group` only for a
    /// group given by name. A numeric uid is never looked up to decide root,
    /// and uid `0` is root whatever its group, so `0:staff` reads nothing.
    pub fn to_classify(spec: &str) -> Self {
        let (user, group) = split_spec(spec);
        let named = |part: &str| !part.is_empty() && part.parse::<u32>().is_err();
        // Any spelling of uid 0 (`0`, `00`) is root whatever its group.
        let uid_zero = user.parse::<u32>() == Ok(0);
        Self {
            passwd: named(user),
            group: !uid_zero && group.is_some_and(named),
        }
    }
}

/// Read the identity files `needed` of `image` through `fetch`, which hands
/// back the archive of one absolute path inside the image (the daemon's
/// archive API, or a worker's path export). Symlinks are followed within the
/// image, and every file is bounded by [`MAX_IDENTITY_FILE_BYTES`].
pub async fn read_identity_files<F, Fut>(
    image: &str,
    needed: IdentityFilesNeeded,
    mut fetch: F,
) -> Result<ImageIdentity, ImageFileError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<FetchedPath, ImageFileError>>,
{
    let passwd = if needed.passwd {
        read_image_file(image, PASSWD_PATH, &mut fetch).await?
    } else {
        None
    };
    let group = if needed.group {
        read_image_file(image, GROUP_PATH, &mut fetch).await?
    } else {
        None
    };
    Ok(ImageIdentity { passwd, group })
}

/// The scratch container [`read_image_identity`] created could not be
/// removed. Handed back for the caller to log: this crate does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftoverContainer {
    pub container_id: String,
    pub reason: String,
}

/// What [`read_image_identity`] read, and whether it cleaned up after itself.
#[derive(Debug)]
pub struct ImageIdentityRead {
    pub identity: Result<ImageIdentity, ImageFileError>,
    /// `Some` when the never-started container used to address the image's
    /// filesystem is still on the daemon.
    pub leftover: Option<LeftoverContainer>,
}

/// Read an image's `/etc/passwd` and `/etc/group` through the Docker daemon.
///
/// The image must already be present on the daemon. A container is created
/// from it (never started) to address its filesystem, and always removed,
/// with the anonymous volumes the daemon made for it, including on error.
pub async fn read_image_identity(docker: &Docker, image: &str) -> ImageIdentityRead {
    let container = match docker
        .create_container(
            None::<bollard::query_parameters::CreateContainerOptions>,
            bollard::models::ContainerCreateBody {
                image: Some(image.to_string()),
                cmd: Some(vec!["true".to_string()]),
                ..Default::default()
            },
        )
        .await
    {
        Ok(container) => container,
        Err(e) => {
            return ImageIdentityRead {
                identity: Err(ImageFileError::Docker {
                    image: image.to_string(),
                    path: PASSWD_PATH.to_string(),
                    reason: format!("creating a container to read it from: {e}"),
                }),
                leftover: None,
            }
        }
    };
    let container_id = container.id.as_str();
    let identity = read_identity_files(image, IdentityFilesNeeded::ALL, |path| async move {
        let stream = docker.download_from_container(
            container_id,
            Some(
                bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
                    .path(&path)
                    .build(),
            ),
        );
        let mut stream = std::pin::pin!(stream);
        // The daemon answers 404 for a path the image does not have; it
        // arrives as the stream's first item.
        let first = match stream.next().await {
            None => return Ok(FetchedPath::Archive(Vec::new())),
            Some(Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })) => return Ok(FetchedPath::Missing),
            Some(first) => first,
        };
        let rest = futures::stream::once(async { first }).chain(stream);
        collect_identity_archive(std::pin::pin!(rest), image, &path)
            .await
            .map(FetchedPath::Archive)
    })
    .await;
    let leftover = docker
        .remove_container(
            container_id,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                // The daemon creates an anonymous volume per `VOLUME` of the
                // image when the container is created; without this each
                // read would leave them behind.
                v: true,
                ..Default::default()
            }),
        )
        .await
        .err()
        .map(|e| LeftoverContainer {
            container_id: container.id.clone(),
            reason: e.to_string(),
        });
    ImageIdentityRead { identity, leftover }
}

#[cfg(test)]
mod tests;
