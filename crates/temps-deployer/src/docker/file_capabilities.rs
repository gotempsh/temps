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
//! [`MAX_SYMLINK_HOPS`] times.

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

/// Where a symlink at `path` pointing at `target` leads, normalised.
pub(crate) fn resolve_link(path: &str, target: &str) -> String {
    let base = if target.starts_with('/') {
        String::new()
    } else {
        match path.rfind('/') {
            Some(index) => path[..index].to_string(),
            None => String::new(),
        }
    };
    let joined = format!("{base}/{target}");
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
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
        assert_eq!(
            resolve_link("/usr/bin/seqsvr", "../../seqsvr/Seq"),
            "/seqsvr/Seq"
        );
        assert_eq!(
            resolve_link("/usr/bin/app", "/opt/app/bin/app"),
            "/opt/app/bin/app"
        );
        assert_eq!(resolve_link("/usr/bin/app", "./real"), "/usr/bin/real");
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
