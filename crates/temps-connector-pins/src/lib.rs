// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The connector builds Temps runs for its ingress providers (ADR-117 D7),
//! pinned by checksum, one module per provider.
//!
//! The control plane (`temps-ingress`) puts these in the specs it hands to
//! nodes, and the node agent (`temps-node-agent`) refuses anything not listed
//! here. Both read this one crate, so the two lists cannot drift apart. The
//! agent still runs only what *its own* build pins: a newer control plane
//! cannot make an older agent run a binary it was not built with.
//!
//! No dependencies, so the agent takes on nothing from the control plane.

/// One downloadable connector binary, trusted only on this platform, at
/// this exact URL, with this exact checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedBinary {
    /// `std::env::consts::OS` of the node that may run it.
    pub os: &'static str,
    /// `std::env::consts::ARCH` of the node that may run it.
    pub arch: &'static str,
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the file at `url`: of the archive itself when
    /// `archive_entry` is set.
    pub sha256: &'static str,
    /// The file to run from inside a gzipped tarball download; `None` when
    /// the download is the binary.
    pub archive_entry: Option<&'static str>,
}

/// Every pinned binary of every provider: what an agent agrees to download
/// and execute.
pub fn binaries() -> impl Iterator<Item = &'static PinnedBinary> {
    cloudflared::BINARIES.iter()
}

/// Cloudflare Tunnel's connector.
pub mod cloudflared {
    use super::PinnedBinary;

    pub const VERSION: &str = "2026.10.1";

    pub const BINARIES: &[PinnedBinary] = &[
        PinnedBinary {
            os: "linux",
            arch: "x86_64",
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-linux-amd64",
            sha256: "3bfc539f45d2620c71debb7a86be0efa737ed5ccbad9eb85c787116900e5c586",
            archive_entry: None,
        },
        PinnedBinary {
            os: "linux",
            arch: "aarch64",
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-linux-arm64",
            sha256: "2c65dfabf616c40efc7545d107d7a57ad5e21377e2964b7e88af19a238074a46",
            archive_entry: None,
        },
        // cloudflared publishes macOS builds only as `.tgz` archives holding
        // the `cloudflared` binary; these pin the archives.
        PinnedBinary {
            os: "macos",
            arch: "x86_64",
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-darwin-amd64.tgz",
            sha256: "3b0dbaf378337e9d7fb6cd235a76d1d6a7dee434c2d8edf9281164fb6a3837ab",
            archive_entry: Some("cloudflared"),
        },
        PinnedBinary {
            os: "macos",
            arch: "aarch64",
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-darwin-arm64.tgz",
            sha256: "647615df99d49cd179ea5e13ef760b9bbfb33630f6ae62f87e33dfadda60adcc",
            archive_entry: Some("cloudflared"),
        },
    ];

    /// The container image, by digest, for the container driver.
    pub const IMAGE: &str = "cloudflare/cloudflared:2026.10.1@sha256:a15a3b50b8dabd57ac2090e9484c0247dba9dfa1f0df4d298d4317f9d69eb2d5";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binary_is_https_with_a_full_lowercase_sha256() {
        for binary in binaries() {
            assert!(binary.url.starts_with("https://"), "{}", binary.url);
            assert_eq!(binary.sha256.len(), 64, "{}", binary.url);
            assert!(
                binary
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{} has a checksum that is not lowercase hex",
                binary.url
            );
        }
    }

    #[test]
    fn an_archive_and_only_an_archive_names_the_file_to_run() {
        for binary in binaries() {
            assert_eq!(
                binary.url.ends_with(".tgz"),
                binary.archive_entry.is_some(),
                "{}",
                binary.url
            );
            if let Some(entry) = binary.archive_entry {
                assert!(
                    !entry.is_empty() && !entry.contains('/') && !entry.contains(".."),
                    "{} runs {entry:?}, which is not a plain file name",
                    binary.url
                );
            }
        }
    }

    #[test]
    fn no_platform_is_pinned_twice_for_one_provider() {
        let mut seen = std::collections::HashSet::new();
        for binary in cloudflared::BINARIES {
            assert!(
                seen.insert((binary.os, binary.arch)),
                "{}/{} pinned twice",
                binary.os,
                binary.arch
            );
        }
    }

    #[test]
    fn cloudflared_binaries_and_image_are_the_pinned_version() {
        let release = format!(
            "https://github.com/cloudflare/cloudflared/releases/download/{}/",
            cloudflared::VERSION
        );
        for binary in cloudflared::BINARIES {
            assert!(binary.url.starts_with(&release), "{}", binary.url);
        }
        assert!(
            cloudflared::IMAGE.starts_with(&format!(
                "cloudflare/cloudflared:{}@sha256:",
                cloudflared::VERSION
            )),
            "{}",
            cloudflared::IMAGE
        );
    }
}
