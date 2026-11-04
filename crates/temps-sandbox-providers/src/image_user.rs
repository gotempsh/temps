// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resolving an OCI image's `USER` the way Docker does.
//!
//! A container runtime turns `USER nextjs` into a uid and gid by reading the
//! *image's* `/etc/passwd` and `/etc/group`. The microVM driver starts the
//! image's process through a guest agent that only takes numbers, so the
//! lookup has to happen on the host, against the image, before the VM is
//! created.
//!
//! This module is that lookup, in two independent halves:
//!
//! - [`resolve_image_user`] is pure: a `USER` string plus the text of the two
//!   files in, a [`ResolvedUser`] out. Its rules were taken from what Docker
//!   itself reports for each form (see the table on that function), so a
//!   microVM replica runs as the same identity a container of the same image
//!   would.
//! - [`read_image_identity`] fetches those two files from a Docker image
//!   through the daemon's archive API, bounded and symlink-safe. It reads
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
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
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
/// A name is looked up first; a numeric string that is no name is looked up
/// as a uid, and kept as a bare number when passwd has no such entry. A group
/// that is not a number must exist in `/etc/group`. `root` resolves to
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
    let entry = passwd_entries
        .iter()
        .find(|entry| entry.name == user_part)
        .or_else(|| {
            numeric_user.and_then(|uid| passwd_entries.iter().find(|entry| entry.uid == uid))
        });
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
            let numeric_group = group_name.parse::<u32>().ok();
            let found = group_entries
                .iter()
                .find(|entry| entry.name == group_name)
                .or_else(|| {
                    numeric_group
                        .and_then(|gid| group_entries.iter().find(|entry| entry.gid == gid))
                })
                .map(|entry| entry.gid);
            let gid = match (found, numeric_group) {
                (Some(gid), _) | (None, Some(gid)) => gid,
                (None, None) => {
                    return Err(ImageUserError::GroupNotFound {
                        spec: spec.to_string(),
                        group: group_name.to_string(),
                        group_file: presence(group),
                    })
                }
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
    let mut contents = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut contents)
        .map_err(|e| not_readable(format!("reading its contents failed: {e}")))?;
    Ok(Some(ArchiveFile::Regular(contents)))
}

/// Where a symlink found at `from` points, as an absolute path inside the
/// image. `..` stops at the root, as in a chroot, so a link can never name
/// anything outside the image.
fn resolve_link_target(from: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut dir: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
        dir.pop();
        dir
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Fetch one file from a created container's filesystem, following symlinks.
async fn read_container_file(
    docker: &Docker,
    container: &str,
    image: &str,
    path: &str,
) -> Result<Option<String>, ImageFileError> {
    let docker_error = |reason: String| ImageFileError::Docker {
        image: image.to_string(),
        path: path.to_string(),
        reason,
    };
    let mut current = path.to_string();
    for _ in 0..=MAX_SYMLINK_HOPS {
        let mut stream = docker.download_from_container(
            container,
            Some(
                bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
                    .path(&current)
                    .build(),
            ),
        );
        let mut tar_bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    tar_bytes.extend_from_slice(&chunk);
                    if tar_bytes.len() > MAX_IDENTITY_FILE_BYTES + ARCHIVE_OVERHEAD_BYTES {
                        return Err(ImageFileError::TooLarge {
                            image: image.to_string(),
                            path: current,
                            limit: MAX_IDENTITY_FILE_BYTES,
                        });
                    }
                }
                // The daemon answers 404 for a path the image does not have.
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => return Ok(None),
                Err(e) => return Err(docker_error(format!("{current}: {e}"))),
            }
        }
        match first_archive_file(&tar_bytes, image, &current)? {
            None => return Ok(None),
            Some(ArchiveFile::Regular(contents)) => {
                return Ok(Some(String::from_utf8_lossy(&contents).into_owned()))
            }
            Some(ArchiveFile::Symlink(target)) => {
                current = resolve_link_target(&current, &target);
            }
        }
    }
    Err(ImageFileError::SymlinkLoop {
        image: image.to_string(),
        path: path.to_string(),
        limit: MAX_SYMLINK_HOPS,
    })
}

/// Read an image's `/etc/passwd` and `/etc/group` through the Docker daemon.
///
/// The image must already be present on the daemon. A container is created
/// from it (never started) to address its filesystem, and always removed,
/// with the anonymous volumes the daemon made for it, including on error.
pub async fn read_image_identity(
    docker: &Docker,
    image: &str,
) -> Result<ImageIdentity, ImageFileError> {
    let container = docker
        .create_container(
            None::<bollard::query_parameters::CreateContainerOptions>,
            bollard::models::ContainerCreateBody {
                image: Some(image.to_string()),
                cmd: Some(vec!["true".to_string()]),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| ImageFileError::Docker {
            image: image.to_string(),
            path: PASSWD_PATH.to_string(),
            reason: format!("creating a container to read it from: {e}"),
        })?;
    let identity = async {
        Ok(ImageIdentity {
            passwd: read_container_file(docker, &container.id, image, PASSWD_PATH).await?,
            group: read_container_file(docker, &container.id, image, GROUP_PATH).await?,
        })
    }
    .await;
    if let Err(e) = docker
        .remove_container(
            &container.id,
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
    {
        tracing::warn!(
            container_id = %container.id,
            image,
            error = %e,
            "could not remove the container used to read an image's identity files"
        );
    }
    identity
}

#[cfg(test)]
mod tests {
    use super::*;

    // An Alpine-shaped identity: the same lines the daemon-checked table in
    // `resolve_image_user` was derived from.
    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/sh
bin:x:1:1:bin:/bin:/sbin/nologin
daemon:x:2:2:daemon:/sbin:/sbin/nologin
nobody:x:65534:65534:nobody:/:/sbin/nologin
nextjs:x:1001:1001::/home/nextjs:/bin/sh
";
    const GROUP: &str = "\
root:x:0:root
bin:x:1:root,bin,daemon
daemon:x:2:root,bin,daemon
adm:x:4:root,adm,daemon
tty:x:5:
nobody:x:65534:
nodejs:x:1001:
";

    fn resolve(spec: &str) -> Result<ResolvedUser, ImageUserError> {
        resolve_image_user(spec, Some(PASSWD), Some(GROUP))
    }

    fn user(uid: u32, gid: u32, groups: &[u32], home: &str) -> ResolvedUser {
        ResolvedUser {
            uid,
            gid,
            groups: groups.to_vec(),
            home: home.to_string(),
        }
    }

    #[test]
    fn a_named_user_takes_its_primary_group_membership_groups_and_home() {
        assert_eq!(
            resolve("daemon").unwrap(),
            user(2, 2, &[1, 2, 4], "/sbin"),
            "primary group plus every group that lists the user"
        );
        assert_eq!(
            resolve("nextjs").unwrap(),
            user(1001, 1001, &[1001], "/home/nextjs")
        );
    }

    #[test]
    fn a_uid_that_passwd_knows_resolves_like_its_name() {
        assert_eq!(resolve("2").unwrap(), resolve("daemon").unwrap());
        assert_eq!(resolve("1001").unwrap(), resolve("nextjs").unwrap());
    }

    #[test]
    fn a_uid_passwd_does_not_know_runs_as_group_zero_with_home_slash() {
        assert_eq!(resolve("1234").unwrap(), user(1234, 0, &[0], "/"));
    }

    #[test]
    fn an_explicit_group_is_used_alone() {
        assert_eq!(
            resolve("daemon:daemon").unwrap(),
            user(2, 2, &[2], "/sbin"),
            "an explicit group drops the membership groups"
        );
        assert_eq!(resolve("daemon:root").unwrap(), user(2, 0, &[0], "/sbin"));
        assert_eq!(resolve("daemon:5").unwrap(), user(2, 5, &[5], "/sbin"));
        assert_eq!(resolve("nextjs:nodejs").unwrap().gid, 1001);
    }

    #[test]
    fn a_fully_numeric_pair_needs_no_entry_in_either_file() {
        assert_eq!(
            resolve("1234:5678").unwrap(),
            user(1234, 5678, &[5678], "/")
        );
        assert_eq!(
            resolve_image_user("1234:5678", None, None).unwrap(),
            user(1234, 5678, &[5678], "/")
        );
    }

    #[test]
    fn root_resolves_without_a_passwd_and_keeps_its_home_with_one() {
        assert_eq!(
            resolve_image_user("root", None, None).unwrap(),
            user(0, 0, &[0], "/")
        );
        assert_eq!(
            resolve("root").unwrap(),
            user(0, 0, &[0, 1, 2, 4], "/root"),
            "root is a member of bin, daemon and adm in this image"
        );
    }

    #[test]
    fn a_name_wins_over_a_number_and_surrounding_space_is_ignored() {
        let passwd = "\
a:x:7:7::/a:/bin/sh
7:x:8:8::/seven:/bin/sh
";
        assert_eq!(
            resolve_image_user("7", Some(passwd), None).unwrap().uid,
            8,
            "a user literally named 7 is found by name first"
        );
        assert_eq!(resolve("  nextjs ").unwrap().uid, 1001);
    }

    #[test]
    fn an_unknown_user_names_the_spec_and_whether_the_image_has_a_passwd() {
        let with_file = resolve("ghost").unwrap_err();
        assert_eq!(
            with_file,
            ImageUserError::UserNotFound {
                spec: "ghost".into(),
                user: "ghost".into(),
                passwd: "not listed",
            }
        );
        let message = with_file.to_string();
        assert!(message.contains("'ghost'"), "{message}");
        assert!(message.contains("/etc/passwd"), "{message}");

        let without_file = resolve_image_user("ghost", None, None).unwrap_err();
        assert!(
            without_file
                .to_string()
                .contains("the image has no such file"),
            "{without_file}"
        );
    }

    #[test]
    fn an_unknown_group_name_is_refused_but_an_unknown_gid_is_not() {
        let error = resolve("daemon:staff").unwrap_err();
        assert_eq!(
            error,
            ImageUserError::GroupNotFound {
                spec: "daemon:staff".into(),
                group: "staff".into(),
                group_file: "not listed",
            }
        );
        assert!(error.to_string().contains("/etc/group"), "{error}");
        assert_eq!(resolve("daemon:4242").unwrap().gid, 4242);
    }

    #[test]
    fn malformed_specs_say_what_is_wrong() {
        for (spec, fragment) in [
            (":5", "user part is empty"),
            ("", "user part is empty"),
            ("daemon:", "group part"),
            ("a:b:c", "more than one ':'"),
        ] {
            let error = resolve(spec).unwrap_err();
            assert!(
                matches!(error, ImageUserError::Malformed { .. }),
                "{spec}: {error}"
            );
            assert!(error.to_string().contains(fragment), "{spec}: {error}");
        }
    }

    #[test]
    fn identity_files_tolerate_comments_blank_and_broken_lines() {
        let passwd = "\
# a comment

broken-line-without-fields
nobody:x:notanumber:1:::
app:x:500:500::/srv/app:/bin/sh
";
        let group = "\
# comment
nogid:x:abc:
apps:x:500:app
";
        assert_eq!(
            resolve_image_user("app", Some(passwd), Some(group)).unwrap(),
            user(500, 500, &[500], "/srv/app")
        );
        assert!(resolve_image_user("nobody", Some(passwd), Some(group)).is_err());
    }

    #[test]
    fn an_empty_home_falls_back_to_the_root_directory() {
        let passwd = "svc:x:900:900:::/bin/false\n";
        assert_eq!(
            resolve_image_user("svc", Some(passwd), None).unwrap().home,
            "/"
        );
    }

    fn tar_with(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        builder.into_inner().unwrap()
    }

    fn regular(name: &str, contents: &[u8]) -> Vec<u8> {
        tar_with(|builder| {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, contents).unwrap();
        })
    }

    #[test]
    fn a_regular_file_is_read_back_whole() {
        let tar = regular("passwd", b"root:x:0:0::/root:/bin/sh\n");
        assert_eq!(
            first_archive_file(&tar, "img", "/etc/passwd").unwrap(),
            Some(ArchiveFile::Regular(
                b"root:x:0:0::/root:/bin/sh\n".to_vec()
            ))
        );
    }

    #[test]
    fn a_symlink_reports_its_target_instead_of_content() {
        let tar = tar_with(|builder| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder
                .append_link(&mut header, "passwd", "../usr/etc/passwd")
                .unwrap();
        });
        assert_eq!(
            first_archive_file(&tar, "img", "/etc/passwd").unwrap(),
            Some(ArchiveFile::Symlink("../usr/etc/passwd".to_string()))
        );
    }

    #[test]
    fn an_empty_archive_is_no_file_and_a_directory_is_refused() {
        assert_eq!(
            first_archive_file(&tar_with(|_| {}), "img", "/etc/passwd").unwrap(),
            None
        );
        let dir = tar_with(|builder| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "passwd/", std::io::empty())
                .unwrap();
        });
        let error = first_archive_file(&dir, "img", "/etc/passwd").unwrap_err();
        assert!(
            matches!(error, ImageFileError::NotReadable { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("/etc/passwd"), "{error}");
        assert!(error.to_string().contains("img"), "{error}");
    }

    #[test]
    fn a_file_over_the_cap_is_refused_before_it_is_read() {
        let big = vec![b'x'; MAX_IDENTITY_FILE_BYTES + 1];
        let error = first_archive_file(&regular("passwd", &big), "img", "/etc/passwd").unwrap_err();
        assert_eq!(
            error,
            ImageFileError::TooLarge {
                image: "img".into(),
                path: "/etc/passwd".into(),
                limit: MAX_IDENTITY_FILE_BYTES,
            }
        );
    }

    #[test]
    fn garbage_is_not_mistaken_for_an_archive_entry() {
        let result = first_archive_file(&[0xff; 1024], "img", "/etc/passwd");
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn symlink_targets_stay_inside_the_image() {
        assert_eq!(
            resolve_link_target("/etc/passwd", "/usr/etc/passwd"),
            "/usr/etc/passwd"
        );
        assert_eq!(
            resolve_link_target("/etc/passwd", "../usr/etc/passwd"),
            "/usr/etc/passwd"
        );
        assert_eq!(
            resolve_link_target("/etc/passwd", "passwd.real"),
            "/etc/passwd.real"
        );
        assert_eq!(
            resolve_link_target("/etc/passwd", "../../../../etc/shadow"),
            "/etc/shadow",
            "`..` stops at the image root"
        );
        assert_eq!(resolve_link_target("/etc/passwd", "./a//b/."), "/etc/a/b");
    }

    /// A client for the default daemon socket; `None` when there is none.
    /// The client connects lazily, so callers that need a live daemon still
    /// ping it.
    fn reachable_docker() -> Option<Docker> {
        let docker = Docker::connect_with_defaults().ok()?;
        std::path::Path::new(&std::env::var("DOCKER_HOST").ok().map_or_else(
            || "/var/run/docker.sock".to_string(),
            |host| host.trim_start_matches("unix://").to_string(),
        ))
        .exists()
        .then_some(docker)
    }

    /// Skip when no Docker daemon is reachable or the tiny image cannot be
    /// pulled: the pure tests above carry the logic, this one proves the
    /// archive API round trip against a real daemon.
    async fn docker_with_image(image: &str) -> Option<Docker> {
        let docker = reachable_docker()?;
        docker.ping().await.ok()?;
        if docker.inspect_image(image).await.is_err() {
            let mut pull = docker.create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(image.to_string()),
                    ..Default::default()
                }),
                None,
                None,
            );
            while let Some(item) = pull.next().await {
                item.ok()?;
            }
        }
        Some(docker)
    }

    #[tokio::test]
    async fn a_real_image_resolves_like_docker_does() {
        let Some(docker) = docker_with_image("alpine:3").await else {
            println!("Docker or the alpine image is unavailable, skipping");
            return;
        };
        let identity = read_image_identity(&docker, "alpine:3").await.unwrap();
        let passwd = identity.passwd.as_deref();
        let group = identity.group.as_deref();
        // The same answers `docker run --user <spec> alpine:3 id` gives.
        let daemon = resolve_image_user("daemon", passwd, group).unwrap();
        assert_eq!((daemon.uid, daemon.gid), (2, 2));
        assert_eq!(daemon.groups, vec![1, 2, 4]);
        assert_eq!(daemon.home, "/sbin");
        assert_eq!(
            resolve_image_user("nobody", passwd, group).unwrap().uid,
            65534
        );
        assert_eq!(
            resolve_image_user("1234", passwd, group).unwrap().gid,
            0,
            "an unknown uid runs as group 0"
        );
        assert!(resolve_image_user("nextjs", passwd, group).is_err());
    }

    /// Build a throwaway image from `dockerfile` with the docker CLI; `None`
    /// when the CLI or daemon cannot do it (the test then skips).
    fn build_image(tag: &str, dockerfile: &str) -> Option<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("docker")
            .args(["build", "-q", "-t", tag, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(dockerfile.as_bytes()).ok()?;
        child.wait().ok()?.success().then_some(())
    }

    #[tokio::test]
    async fn a_named_user_behind_a_symlinked_passwd_resolves() {
        let tag = format!("temps-image-user-test:{}", std::process::id());
        let dockerfile = "FROM alpine:3\n\
             RUN addgroup -g 1001 nodejs && adduser -D -u 1001 -G nodejs nextjs\n\
             RUN mkdir -p /usr/etc && mv /etc/passwd /usr/etc/passwd \
                 && ln -s ../usr/etc/passwd /etc/passwd\n\
             USER nextjs\n";
        let Some(docker) = docker_with_image("alpine:3").await else {
            println!("Docker or the alpine image is unavailable, skipping");
            return;
        };
        if build_image(&tag, dockerfile).is_none() {
            println!("the image build is unavailable, skipping");
            return;
        }
        let identity = read_image_identity(&docker, &tag).await;
        let _ = std::process::Command::new("docker")
            .args(["rmi", "-f", &tag])
            .output();
        let identity = identity.unwrap();
        assert!(
            identity
                .passwd
                .as_deref()
                .is_some_and(|p| p.contains("nextjs")),
            "the passwd behind the symlink was read: {identity:?}"
        );
        let resolved = resolve_image_user(
            "nextjs",
            identity.passwd.as_deref(),
            identity.group.as_deref(),
        )
        .unwrap();
        assert_eq!((resolved.uid, resolved.gid), (1001, 1001));
        assert_eq!(resolved.home, "/home/nextjs");
        let by_pair = resolve_image_user(
            "nextjs:nodejs",
            identity.passwd.as_deref(),
            identity.group.as_deref(),
        )
        .unwrap();
        assert_eq!((by_pair.uid, by_pair.gid), (1001, 1001));
    }

    #[tokio::test]
    async fn an_image_without_user_databases_resolves_numeric_users_only() {
        let tag = format!("temps-image-user-scratch:{}", std::process::id());
        let Some(docker) = reachable_docker() else {
            println!("Docker not available, skipping");
            return;
        };
        if build_image(&tag, "FROM scratch\nLABEL temps.test=1\n").is_none() {
            println!("the image build is unavailable, skipping");
            return;
        }
        let identity = read_image_identity(&docker, &tag).await;
        let _ = std::process::Command::new("docker")
            .args(["rmi", "-f", &tag])
            .output();
        let identity = identity.unwrap();
        assert_eq!(identity, ImageIdentity::default());
        let resolved = resolve_image_user("1000:1000", None, None).unwrap();
        assert_eq!((resolved.uid, resolved.gid), (1000, 1000));
        let error = resolve_image_user("app", None, None).unwrap_err();
        assert!(
            error.to_string().contains("the image has no such file"),
            "{error}"
        );
    }

    /// The names of the anonymous volumes on the daemon. A listing that
    /// fails fails the test: an empty set from a failed command would let the
    /// leak check pass without checking anything.
    fn anonymous_volumes() -> std::collections::BTreeSet<String> {
        let output = std::process::Command::new("docker")
            .args(["volume", "ls", "-q", "--filter", "dangling=true"])
            .output()
            .expect("run `docker volume ls`");
        assert!(
            output.status.success(),
            "`docker volume ls` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn reading_an_image_with_volumes_leaves_no_volume_behind() {
        let tag = format!("temps-image-user-volume:{}", std::process::id());
        let Some(docker) = docker_with_image("alpine:3").await else {
            println!("Docker or the alpine image is unavailable, skipping");
            return;
        };
        if build_image(&tag, "FROM alpine:3\nVOLUME /data /cache\n").is_none() {
            println!("the image build is unavailable, skipping");
            return;
        }
        let before = anonymous_volumes();
        let identity = read_image_identity(&docker, &tag).await;
        let after = anonymous_volumes();
        let _ = std::process::Command::new("docker")
            .args(["rmi", "-f", &tag])
            .output();
        identity.unwrap();
        let leaked: Vec<_> = after.difference(&before).collect();
        assert!(
            leaked.is_empty(),
            "reading an image's identity left anonymous volumes behind: {leaked:?}"
        );
    }

    #[tokio::test]
    async fn reading_the_identity_of_a_missing_image_names_the_image() {
        let Ok(docker) = Docker::connect_with_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let error = read_image_identity(&docker, "temps-no-such-image:never")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("temps-no-such-image:never"),
            "{error}"
        );
    }
}
