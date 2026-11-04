// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Unpacking a container image's exported filesystem into a directory the
//! microVM disk is built from.
//!
//! The archive is data an image author controls, unpacked by the server's
//! own user, so extraction is bounded (entries and bytes, so a tar bomb
//! cannot fill the data disk) and every refusal names the entry. The `tar`
//! crate refuses `..` components, writes through symlinks that leave the
//! destination and hard links to files outside it; the tests below pin
//! that, so an upgrade that loosens it fails here rather than in production.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Ceilings on one image's unpacked content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Most entries (files, directories, links) unpacked.
    pub max_entries: u64,
    /// Most bytes of regular-file content unpacked.
    pub max_bytes: u64,
}

/// Well above any real image (a few hundred thousand files, tens of GiB at
/// the extreme) and far below what would fill a small host's disk unnoticed.
pub const IMAGE_ARCHIVE_LIMITS: ArchiveLimits = ArchiveLimits {
    max_entries: 2_000_000,
    max_bytes: 32 * 1024 * 1024 * 1024,
};

#[derive(Debug, thiserror::Error)]
pub enum ImageArchiveError {
    #[error("image archive has more than {limit} entries (stopped at '{entry}')")]
    TooManyEntries { limit: u64, entry: String },

    #[error(
        "image archive exceeds {limit_bytes} bytes of file content (entry '{entry}' of \
         {entry_bytes} bytes would pass it)"
    )]
    TooLarge {
        limit_bytes: u64,
        entry: String,
        entry_bytes: u64,
    },

    #[error("image archive entry '{entry}' could not be unpacked into '{destination}': {reason}")]
    Entry {
        entry: String,
        destination: PathBuf,
        reason: String,
    },

    #[error("reading image archive '{path}': {reason}")]
    Read { path: PathBuf, reason: String },
}

/// What an extraction did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub entries: u64,
    pub bytes: u64,
    /// Entries left out: device nodes, FIFOs and the like (the guest mounts
    /// its own `/dev`), and paths with a `..` component.
    pub skipped: u64,
}

/// Unpack `archive` into `destination`, which must exist.
pub fn extract_image_archive(
    archive: impl Read,
    source_label: &Path,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<ExtractReport, ImageArchiveError> {
    let read_error = |e: std::io::Error| ImageArchiveError::Read {
        path: source_label.to_path_buf(),
        reason: e.to_string(),
    };
    let mut archive = tar::Archive::new(archive);
    let mut report = ExtractReport::default();
    for entry in archive.entries().map_err(read_error)? {
        let mut entry = entry.map_err(read_error)?;
        let name = entry.path_bytes();
        let name = String::from_utf8_lossy(&name).into_owned();
        let kind = entry.header().entry_type();
        if !matches!(
            kind,
            tar::EntryType::Regular
                | tar::EntryType::Directory
                | tar::EntryType::Symlink
                | tar::EntryType::Link
        ) {
            report.skipped += 1;
            continue;
        }
        if report.entries >= limits.max_entries {
            return Err(ImageArchiveError::TooManyEntries {
                limit: limits.max_entries,
                entry: name,
            });
        }
        let size = if kind == tar::EntryType::Regular {
            entry.header().size().map_err(read_error)?
        } else {
            0
        };
        if report.bytes.saturating_add(size) > limits.max_bytes {
            return Err(ImageArchiveError::TooLarge {
                limit_bytes: limits.max_bytes,
                entry: name,
                entry_bytes: size,
            });
        }
        let unpacked = entry
            .unpack_in(destination)
            .map_err(|e| ImageArchiveError::Entry {
                entry: name,
                destination: destination.to_path_buf(),
                reason: e.to_string(),
            })?;
        if unpacked {
            report.entries += 1;
            report.bytes += size;
        } else {
            report.skipped += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ArchiveLimits = ArchiveLimits {
        max_entries: 1000,
        max_bytes: 1024 * 1024,
    };

    /// A raw tar header, which `tar::Builder` would otherwise refuse to make
    /// (`..` paths are exactly what a hostile archive contains).
    fn raw_entry(
        out: &mut Vec<u8>,
        path: &str,
        kind: tar::EntryType,
        link: Option<&str>,
        content: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        {
            let bytes = header.as_old_mut();
            bytes.name[..path.len()].copy_from_slice(path.as_bytes());
        }
        header.set_entry_type(kind);
        header.set_size(content.len() as u64);
        header.set_mode(if kind == tar::EntryType::Directory {
            0o755
        } else {
            0o644
        });
        if let Some(link) = link {
            let bytes = header.as_old_mut();
            bytes.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(content);
        let padding = (512 - content.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, padding));
    }

    fn finish(mut archive: Vec<u8>) -> Vec<u8> {
        archive.extend(std::iter::repeat_n(0u8, 1024));
        archive
    }

    fn extract(
        archive: Vec<u8>,
        limits: ArchiveLimits,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        Result<ExtractReport, ImageArchiveError>,
    ) {
        // `outside` is a sibling of the root: where an escape would land.
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("root");
        let outside = sandbox.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"host data").unwrap();
        let result = extract_image_archive(
            std::io::Cursor::new(archive),
            Path::new("test.tar"),
            &root,
            limits,
        );
        (sandbox, root, outside, result)
    }

    fn outside_is_untouched(outside: &Path) {
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"host data");
        let names: Vec<_> = std::fs::read_dir(outside)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "something was written outside: {names:?}");
    }

    #[test]
    fn an_ordinary_image_is_unpacked_and_counted() {
        let mut tar = Vec::new();
        raw_entry(&mut tar, "etc/", tar::EntryType::Directory, None, b"");
        raw_entry(
            &mut tar,
            "etc/hostname",
            tar::EntryType::Regular,
            None,
            b"vm",
        );
        raw_entry(
            &mut tar,
            "bin/sh",
            tar::EntryType::Symlink,
            Some("busybox"),
            b"",
        );
        let (_s, root, _o, report) = extract(finish(tar), LIMITS);
        let report = report.expect("unpacked");
        assert_eq!(report.entries, 3);
        assert_eq!(report.bytes, 2);
        assert_eq!(std::fs::read(root.join("etc/hostname")).unwrap(), b"vm");
    }

    #[test]
    fn a_parent_directory_path_is_skipped_not_followed() {
        let mut tar = Vec::new();
        raw_entry(
            &mut tar,
            "../outside/planted",
            tar::EntryType::Regular,
            None,
            b"x",
        );
        raw_entry(
            &mut tar,
            "a/../../outside/planted",
            tar::EntryType::Regular,
            None,
            b"x",
        );
        raw_entry(&mut tar, "ok", tar::EntryType::Regular, None, b"fine");
        let (_s, root, outside, report) = extract(finish(tar), LIMITS);
        let report = report.expect("hostile entries are skipped, the rest unpacked");
        assert_eq!(report.skipped, 2);
        assert!(root.join("ok").exists());
        outside_is_untouched(&outside);
    }

    #[test]
    fn an_absolute_path_lands_inside_the_root() {
        let mut tar = Vec::new();
        raw_entry(
            &mut tar,
            "/etc-planted",
            tar::EntryType::Regular,
            None,
            b"x",
        );
        let (_s, root, _o, report) = extract(finish(tar), LIMITS);
        report.expect("unpacked");
        assert!(root.join("etc-planted").exists());
    }

    #[test]
    fn a_file_cannot_be_written_through_a_symlink_that_leaves_the_root() {
        let mut tar = Vec::new();
        // `escape` points at the sibling directory; `escape/planted` would
        // then be written outside the root.
        raw_entry(
            &mut tar,
            "escape",
            tar::EntryType::Symlink,
            Some("../outside"),
            b"",
        );
        raw_entry(
            &mut tar,
            "escape/planted",
            tar::EntryType::Regular,
            None,
            b"x",
        );
        let (_s, _root, outside, report) = extract(finish(tar), LIMITS);
        let error = report.expect_err("the write through the link is refused");
        assert!(matches!(error, ImageArchiveError::Entry { .. }), "{error}");
        assert!(error.to_string().contains("escape/planted"), "{error}");
        outside_is_untouched(&outside);
    }

    #[test]
    fn a_file_cannot_be_written_through_an_absolute_symlink() {
        let sandbox = tempfile::tempdir().unwrap();
        let target = sandbox.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let mut tar = Vec::new();
        raw_entry(
            &mut tar,
            "abs",
            tar::EntryType::Symlink,
            Some(target.to_str().unwrap()),
            b"",
        );
        raw_entry(&mut tar, "abs/planted", tar::EntryType::Regular, None, b"x");
        let root = sandbox.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let error = extract_image_archive(
            std::io::Cursor::new(finish(tar)),
            Path::new("test.tar"),
            &root,
            LIMITS,
        )
        .expect_err("refused");
        assert!(matches!(error, ImageArchiveError::Entry { .. }), "{error}");
        assert!(!target.join("planted").exists());
    }

    #[test]
    fn a_hard_link_to_a_file_outside_the_root_is_refused() {
        let mut tar = Vec::new();
        raw_entry(
            &mut tar,
            "link",
            tar::EntryType::Link,
            Some("../outside/secret"),
            b"",
        );
        let (_s, root, outside, report) = extract(finish(tar), LIMITS);
        let error = report.expect_err("refused");
        assert!(matches!(error, ImageArchiveError::Entry { .. }), "{error}");
        assert!(!root.join("link").exists());
        outside_is_untouched(&outside);
    }

    #[test]
    fn device_nodes_and_fifos_are_left_out() {
        let mut tar = Vec::new();
        raw_entry(&mut tar, "dev/null", tar::EntryType::Char, None, b"");
        raw_entry(&mut tar, "dev/sda", tar::EntryType::Block, None, b"");
        raw_entry(&mut tar, "run/fifo", tar::EntryType::Fifo, None, b"");
        raw_entry(&mut tar, "kept", tar::EntryType::Regular, None, b"1");
        let (_s, root, _o, report) = extract(finish(tar), LIMITS);
        let report = report.expect("unpacked");
        assert_eq!(report.skipped, 3);
        assert!(!root.join("dev/null").exists());
        assert!(root.join("kept").exists());
    }

    #[test]
    fn too_many_entries_stop_the_extraction_naming_the_entry() {
        let mut tar = Vec::new();
        for i in 0..5 {
            raw_entry(
                &mut tar,
                &format!("f{i}"),
                tar::EntryType::Regular,
                None,
                b"",
            );
        }
        let limits = ArchiveLimits {
            max_entries: 3,
            max_bytes: 1024,
        };
        let (_s, root, _o, report) = extract(finish(tar), limits);
        let error = report.expect_err("over the entry ceiling");
        assert!(
            matches!(&error, ImageArchiveError::TooManyEntries { limit: 3, entry } if entry == "f3"),
            "{error}"
        );
        assert!(
            !root.join("f4").exists(),
            "nothing past the ceiling is written"
        );
    }

    #[test]
    fn a_file_that_passes_the_byte_ceiling_is_refused_before_it_is_written() {
        let mut tar = Vec::new();
        raw_entry(
            &mut tar,
            "small",
            tar::EntryType::Regular,
            None,
            &[1u8; 100],
        );
        raw_entry(
            &mut tar,
            "huge",
            tar::EntryType::Regular,
            None,
            &[2u8; 2000],
        );
        let limits = ArchiveLimits {
            max_entries: 10,
            max_bytes: 1000,
        };
        let (_s, root, _o, report) = extract(finish(tar), limits);
        let error = report.expect_err("over the byte ceiling");
        assert!(
            matches!(&error, ImageArchiveError::TooLarge { entry, entry_bytes: 2000, .. } if entry == "huge"),
            "{error}"
        );
        assert!(root.join("small").exists());
        assert!(!root.join("huge").exists());
    }

    #[test]
    fn a_truncated_archive_is_a_read_error_naming_the_file() {
        let mut tar = Vec::new();
        raw_entry(&mut tar, "big", tar::EntryType::Regular, None, &[7u8; 4096]);
        tar.truncate(700);
        let (_s, _r, _o, report) = extract(tar, LIMITS);
        let error = report.expect_err("truncated");
        assert!(
            error.to_string().contains("test.tar") || error.to_string().contains("big"),
            "{error}"
        );
    }
}
