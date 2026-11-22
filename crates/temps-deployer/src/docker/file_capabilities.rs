// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read the Linux file capabilities (`security.capability`) of one file in a
//! container, without starting it.
//!
//! Docker's archive endpoint (`GET /containers/{id}/archive?path=…`) works on
//! a stopped or restarting container and writes the file's
//! `security.capability` xattr as a PAX record
//! (`SCHILY.xattr.security.capability`) before the file's own header. Only
//! those leading headers are read: the stream is dropped as soon as the
//! file's header has been seen, and never more than [`MAX_HEAD_BYTES`] are
//! buffered, so a large binary is never downloaded. A symlink (common for
//! entrypoints, `/usr/bin/app -> /opt/app/bin/app`) is followed up to
//! [`MAX_SYMLINK_HOPS`] times, resolving `..` in its target like the kernel
//! ([`resolve_link`]).

use std::collections::HashMap;

/// The most a single read buffers: the PAX header and the file's header,
/// which are a few KiB even with long paths.
pub(crate) const MAX_HEAD_BYTES: usize = 256 * 1024;
/// Symlinks followed before giving up.
pub(crate) const MAX_SYMLINK_HOPS: usize = 8;
/// Longest path accepted.
pub(crate) const MAX_PATH_BYTES: usize = 4096;

const BLOCK: usize = 512;
const XATTR_KEY: &str = "SCHILY.xattr.security.capability";

/// What the leading headers of a one-file archive say about that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TarHead {
    /// PAX records that applied to the entry (values are raw bytes).
    pub pax: HashMap<String, Vec<u8>>,
    /// The entry's type flag (`b'0'` file, `b'2'` symlink, …).
    pub typeflag: u8,
    /// Symlink target, for a symlink entry.
    pub link_name: Option<String>,
}

impl TarHead {
    /// The raw `security.capability` value, when the entry has one.
    pub fn capability_xattr(&self) -> Option<&[u8]> {
        self.pax.get(XATTR_KEY).map(Vec::as_slice)
    }
}

/// Why the leading headers could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TarHeadError {
    /// More bytes are needed before the entry header is complete.
    Incomplete,
    /// The bytes are not a tar archive Temps can read.
    Malformed(&'static str),
    /// The archive ended without an entry.
    Empty,
}

fn octal(field: &[u8]) -> Result<usize, TarHeadError> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        // Base-256 is only used for sizes over 8 GiB: never a header we read.
        return Err(TarHeadError::Malformed("base-256 size"));
    }
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(text, 8).map_err(|_| TarHeadError::Malformed("size is not octal"))
}

fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn parse_pax(mut records: &[u8], into: &mut HashMap<String, Vec<u8>>) -> Result<(), TarHeadError> {
    // "<len> <key>=<value>\n", <len> counting the whole record.
    while !records.is_empty() {
        let space = records
            .iter()
            .position(|b| *b == b' ')
            .ok_or(TarHeadError::Malformed("PAX record without a length"))?;
        let len: usize = std::str::from_utf8(&records[..space])
            .ok()
            .and_then(|digits| digits.parse().ok())
            .ok_or(TarHeadError::Malformed("PAX record length"))?;
        if len <= space + 1 || len > records.len() {
            return Err(TarHeadError::Malformed("PAX record length out of range"));
        }
        let record = &records[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        let equals = record
            .iter()
            .position(|b| *b == b'=')
            .ok_or(TarHeadError::Malformed("PAX record without '='"))?;
        let key = String::from_utf8_lossy(&record[..equals]).into_owned();
        into.insert(key, record[equals + 1..].to_vec());
        records = &records[len..];
    }
    Ok(())
}

/// Parse the headers in front of the first real entry of `buf`.
pub(crate) fn parse_tar_head(buf: &[u8]) -> Result<TarHead, TarHeadError> {
    let mut offset = 0;
    let mut pax = HashMap::new();
    loop {
        let header = buf
            .get(offset..offset + BLOCK)
            .ok_or(TarHeadError::Incomplete)?;
        if header.iter().all(|b| *b == 0) {
            return Err(TarHeadError::Empty);
        }
        let size = octal(&header[124..136])?;
        let typeflag = header[156];
        let padded = size.div_ceil(BLOCK) * BLOCK;
        match typeflag {
            // PAX extended header for the next entry, a global one, and the
            // GNU long-name/long-link entries: metadata, not the file.
            b'x' | b'g' | b'L' | b'K' => {
                let body = buf
                    .get(offset + BLOCK..offset + BLOCK + size)
                    .ok_or(TarHeadError::Incomplete)?;
                if typeflag == b'x' {
                    parse_pax(body, &mut pax)?;
                }
                offset += BLOCK + padded;
            }
            _ => {
                let link_name = (typeflag == b'2').then(|| {
                    pax.get("linkpath")
                        .map(|value| String::from_utf8_lossy(value).into_owned())
                        .unwrap_or_else(|| c_string(&header[157..257]))
                });
                return Ok(TarHead {
                    pax,
                    typeflag,
                    link_name,
                });
            }
        }
    }
}

/// Why [`resolve_link`] could not say where a symlink leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveLinkError<E> {
    /// Reading whether a directory on the way is itself a symlink failed.
    Lookup { path: String, error: E },
    /// More than [`MAX_PARENT_LOOKUPS`] directories had to be checked: a
    /// symlink loop, or a target no real entrypoint has.
    TooManyLookups,
}

/// Directories [`resolve_link`] checks before giving up.
pub(crate) const MAX_PARENT_LOOKUPS: usize = 32;

/// Where a symlink at `path` pointing at `target` leads, resolving `..` the
/// way the kernel does rather than lexically: when `..` leaves a directory
/// that is itself a symlink (`/app/entry -> dir/../real` with
/// `/app/dir -> /opt/bin` leads to `/opt/real`, not `/app/real`), the
/// symlink is replaced by its target first. `read_link` returns the target
/// of the symlink at an absolute path, or `None` for anything else.
///
/// Only the directories a `..` leaves are checked: leaving a real directory
/// lands in its parent whatever that resolves to, and Docker's archive
/// endpoint resolves the remaining directory symlinks itself. It cleans the
/// path lexically first, so a `..` must never reach it.
pub(crate) async fn resolve_link<F, Fut, E>(
    path: &str,
    target: &str,
    mut read_link: F,
) -> Result<String, ResolveLinkError<E>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, E>>,
{
    let mut resolved: Vec<String> = Vec::new();
    if !target.starts_with('/') {
        if let Some(index) = path.rfind('/') {
            resolved.extend(
                path[..index]
                    .split('/')
                    .filter(|part| !part.is_empty() && *part != ".")
                    .map(str::to_string),
            );
        }
    }
    let mut pending: std::collections::VecDeque<String> =
        target.split('/').map(str::to_string).collect();
    let mut lookups = 0;
    while let Some(part) = pending.pop_front() {
        match part.as_str() {
            "" | "." => {}
            ".." => {
                if resolved.is_empty() {
                    continue;
                }
                lookups += 1;
                if lookups > MAX_PARENT_LOOKUPS {
                    return Err(ResolveLinkError::TooManyLookups);
                }
                let directory = format!("/{}", resolved.join("/"));
                let link = read_link(directory.clone()).await.map_err(|error| {
                    ResolveLinkError::Lookup {
                        path: directory,
                        error,
                    }
                })?;
                resolved.pop();
                if let Some(link) = link {
                    // Leave the symlink's target instead: splice it in and
                    // apply this `..` again to its last component.
                    if link.starts_with('/') {
                        resolved.clear();
                    }
                    pending.push_front("..".to_string());
                    for component in link.split('/').rev() {
                        pending.push_front(component.to_string());
                    }
                }
            }
            other => resolved.push(other.to_string()),
        }
    }
    Ok(format!("/{}", resolved.join("/")))
}

/// Whether `path` is one Temps will look up: absolute, bounded, no NUL.
pub(crate) fn valid_lookup_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MAX_PATH_BYTES && !path.contains('\0')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-entry tar stream as Docker's archive endpoint writes it.
    pub(crate) fn archive(name: &str, typeflag: u8, link: &str, pax: &[(&str, &[u8])]) -> Vec<u8> {
        fn header(name: &str, size: usize, typeflag: u8, link: &str) -> Vec<u8> {
            let mut h = vec![0u8; BLOCK];
            h[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{size:011o}\0");
            h[124..136].copy_from_slice(size.as_bytes());
            h[156] = typeflag;
            h[157..157 + link.len()].copy_from_slice(link.as_bytes());
            h[257..263].copy_from_slice(b"ustar\0");
            h
        }
        let mut out = Vec::new();
        if !pax.is_empty() {
            let mut body = Vec::new();
            for (key, value) in pax {
                let mut record = format!(" {key}=").into_bytes();
                record.extend_from_slice(value);
                record.push(b'\n');
                // The length counts its own digits.
                let mut len = record.len() + 1;
                while len != len.to_string().len() + record.len() {
                    len = len.to_string().len() + record.len();
                }
                let mut full = len.to_string().into_bytes();
                full.extend_from_slice(&record);
                body.extend_from_slice(&full);
            }
            out.extend(header("PaxHeaders/x", body.len(), b'x', ""));
            out.extend_from_slice(&body);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend(header(name, 0, typeflag, link));
        out
    }

    const NBS: &[u8] = &[
        0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn reads_the_capability_xattr_from_the_pax_header() {
        let tar = archive(
            "bind80",
            b'0',
            "",
            &[
                ("SCHILY.xattr.security.capability", NBS),
                ("path", b"bind80"),
            ],
        );
        let head = parse_tar_head(&tar).unwrap();
        assert_eq!(head.typeflag, b'0');
        assert_eq!(head.capability_xattr(), Some(NBS));
        let caps =
            temps_workload::decode_file_capabilities(head.capability_xattr().unwrap()).unwrap();
        assert_eq!(caps.permitted, vec!["NET_BIND_SERVICE".to_string()]);
    }

    #[test]
    fn a_file_without_a_pax_header_has_no_capabilities() {
        let head = parse_tar_head(&archive("app", b'0', "", &[])).unwrap();
        assert_eq!(head.capability_xattr(), None);
        assert_eq!(head.link_name, None);
    }

    #[test]
    fn a_symlink_reports_its_target() {
        let head = parse_tar_head(&archive("seqsvr", b'2', "../../seqsvr/Seq", &[])).unwrap();
        assert_eq!(head.link_name.as_deref(), Some("../../seqsvr/Seq"));
    }

    /// Resolve with `links` as the container's symlinks, recording the
    /// directories checked.
    async fn resolve(
        path: &str,
        target: &str,
        links: &[(&str, &str)],
    ) -> (Result<String, ResolveLinkError<()>>, Vec<String>) {
        let links: HashMap<String, String> = links
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect();
        let checked = std::sync::Mutex::new(Vec::new());
        let result = resolve_link(path, target, |directory| {
            checked.lock().unwrap().push(directory.clone());
            let link = links.get(&directory).cloned();
            async move { Ok(link) }
        })
        .await;
        (result, checked.into_inner().unwrap())
    }

    #[tokio::test]
    async fn ordinary_links_resolve_and_only_left_directories_are_checked() {
        assert_eq!(
            resolve("/usr/bin/seqsvr", "../../seqsvr/Seq", &[]).await,
            (
                Ok("/seqsvr/Seq".to_string()),
                vec!["/usr/bin".to_string(), "/usr".to_string()]
            )
        );
        assert_eq!(
            resolve("/usr/bin/app", "/opt/app/bin/app", &[]).await,
            (Ok("/opt/app/bin/app".to_string()), vec![])
        );
        assert_eq!(
            resolve("/usr/bin/app", "./real", &[]).await,
            (Ok("/usr/bin/real".to_string()), vec![])
        );
        assert_eq!(
            resolve("/app", "../../x", &[]).await.0,
            Ok("/x".to_string())
        );
    }

    #[tokio::test]
    async fn dotdot_leaves_a_directory_symlink_from_its_target() {
        // The kernel resolves /app/dir to /opt/bin before applying `..`.
        assert_eq!(
            resolve("/app/entry", "dir/../real", &[("/app/dir", "/opt/bin")])
                .await
                .0,
            Ok("/opt/real".to_string())
        );
        // A relative directory link. `/srv` is never left, so Docker
        // resolves it, and `/srv/real` is the same file as `/data/srv/real`.
        assert_eq!(
            resolve(
                "/app/entry",
                "dir/../real",
                &[("/app/dir", "../srv/bin"), ("/srv", "/data/srv")]
            )
            .await
            .0,
            Ok("/srv/real".to_string())
        );
        // The base directory of the link is a symlink too.
        assert_eq!(
            resolve("/bin/app", "../lib/app", &[("/bin", "usr/bin")])
                .await
                .0,
            Ok("/usr/lib/app".to_string())
        );
    }

    #[tokio::test]
    async fn a_directory_symlink_loop_gives_up_instead_of_spinning() {
        let (result, checked) = resolve("/a/entry", "../real", &[("/a", "/a/b/..")]).await;
        assert_eq!(result, Err(ResolveLinkError::TooManyLookups));
        assert_eq!(checked.len(), MAX_PARENT_LOOKUPS);
    }

    #[tokio::test]
    async fn a_failed_directory_check_is_reported_with_its_path() {
        let result = resolve_link("/usr/bin/app", "../lib/app", |_| async { Err("refused") }).await;
        assert_eq!(
            result,
            Err(ResolveLinkError::Lookup {
                path: "/usr/bin".to_string(),
                error: "refused"
            })
        );
    }

    #[test]
    fn truncated_and_malformed_input_is_reported_not_guessed() {
        let tar = archive(
            "bind80",
            b'0',
            "",
            &[("SCHILY.xattr.security.capability", NBS)],
        );
        assert_eq!(parse_tar_head(&tar[..100]), Err(TarHeadError::Incomplete));
        assert_eq!(
            parse_tar_head(&tar[..BLOCK + 10]),
            Err(TarHeadError::Incomplete)
        );
        assert_eq!(parse_tar_head(&[0u8; 1024]), Err(TarHeadError::Empty));
        let mut bad = tar.clone();
        bad[124..136].copy_from_slice(b"zzzzzzzzzzz\0");
        assert!(matches!(
            parse_tar_head(&bad),
            Err(TarHeadError::Malformed(_))
        ));
    }

    #[test]
    fn lookup_paths_are_absolute_and_bounded() {
        assert!(valid_lookup_path("/seqsvr/Seq"));
        assert!(!valid_lookup_path("seqsvr/Seq"));
        assert!(!valid_lookup_path("/a\0b"));
        assert!(!valid_lookup_path(&format!(
            "/{}",
            "a".repeat(MAX_PATH_BYTES)
        )));
    }
}
