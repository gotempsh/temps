// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which build a staged guest agent (`temps-vm-agent`, `temps-pty-agent`) is.
//!
//! The VM agent speaks a wire protocol with the host, and both agents are
//! replaced independently of `temps` (`temps upgrade`, the installer, a manual
//! copy), so they can silently drift behind it. Each agent embeds a marker
//! with the commit it was built from; the host reads that marker straight out
//! of the file -- nothing is executed -- and compares it with the commit its
//! own copy of the agent crate was built from.

use std::path::Path;

use temps_vm_agent::{BUILD_COMMIT, BUILD_MARKER_PREFIX};

/// Agents are a few MiB; refuse to slurp anything wildly larger.
const MAX_AGENT_BYTES: u64 = 64 * 1024 * 1024;

/// Commit this `temps` build was made from (`unknown` without a git checkout).
pub fn host_build() -> &'static str {
    BUILD_COMMIT
}

/// How a staged agent's build relates to the host's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBuildStatus {
    /// Same commit as the host.
    Current,
    /// A different commit, or built before agents recorded one (`agent` is
    /// `None`), so it is older than anything that can be compared.
    Outdated { agent: Option<String>, host: String },
    /// One side has no commit information (built outside a git checkout).
    Unknown,
}

/// Compare an agent's recorded build with the host's.
pub fn classify(agent: Option<&str>, host: &str) -> AgentBuildStatus {
    if host == "unknown" {
        return AgentBuildStatus::Unknown;
    }
    match agent {
        None => AgentBuildStatus::Outdated {
            agent: None,
            host: host.to_string(),
        },
        Some("unknown") => AgentBuildStatus::Unknown,
        Some(agent) if agent == host => AgentBuildStatus::Current,
        Some(agent) => AgentBuildStatus::Outdated {
            agent: Some(agent.to_string()),
            host: host.to_string(),
        },
    }
}

/// The build recorded in the agent binary at `path`, or `None` when it has no
/// marker (an agent from before this was recorded, or not an agent at all).
pub fn read_agent_build(path: &Path) -> std::io::Result<Option<String>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_AGENT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {len} bytes, more than the {MAX_AGENT_BYTES} an agent binary can be",
                path.display()
            ),
        ));
    }
    Ok(find_build(&std::fs::read(path)?))
}

/// First well-formed marker in `bytes`. The prefix can also occur as a plain
/// string elsewhere in a binary, so every occurrence is tried and only one
/// followed by a commit-shaped value and a NUL counts.
fn find_build(bytes: &[u8]) -> Option<String> {
    let prefix = BUILD_MARKER_PREFIX.as_bytes();
    let mut from = 0;
    while let Some(pos) = bytes[from..]
        .windows(prefix.len())
        .position(|window| window == prefix)
    {
        let value_start = from + pos + prefix.len();
        let rest = &bytes[value_start..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let value = &rest[..end];
        let well_formed = !value.is_empty()
            && end < rest.len()
            && value.len() <= 40
            && (value == b"unknown" || value.iter().all(u8::is_ascii_hexdigit));
        if well_formed {
            return String::from_utf8(value.to_vec()).ok();
        }
        from = value_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_marker(commit: &str) -> Vec<u8> {
        let mut bytes = b"\x7fELFpadding".to_vec();
        bytes.extend_from_slice(BUILD_MARKER_PREFIX.as_bytes());
        bytes.extend_from_slice(commit.as_bytes());
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(b"trailing");
        bytes
    }

    #[test]
    fn reads_the_embedded_commit() {
        assert_eq!(
            find_build(&with_marker("0a1b2c3")).as_deref(),
            Some("0a1b2c3")
        );
        assert_eq!(
            find_build(&with_marker("unknown")).as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn a_binary_without_a_marker_has_no_build() {
        assert_eq!(find_build(b"\x7fELF no marker in here"), None);
        // The prefix alone, not followed by a commit and a NUL, is not a marker.
        let mut bytes = BUILD_MARKER_PREFIX.as_bytes().to_vec();
        bytes.extend_from_slice(b"not-hex-and-unterminated");
        assert_eq!(find_build(&bytes), None);
    }

    #[test]
    fn a_stray_prefix_does_not_hide_the_real_marker() {
        let mut bytes = BUILD_MARKER_PREFIX.as_bytes().to_vec();
        bytes.extend_from_slice(b" documentation text\n");
        bytes.extend_from_slice(&with_marker("deadbee"));
        assert_eq!(find_build(&bytes).as_deref(), Some("deadbee"));
    }

    #[test]
    fn this_crate_embeds_a_marker_the_scanner_understands() {
        // The marker static is what the agent binaries embed; the scanner
        // must read back exactly the commit the crate reports.
        let mut bytes = b"\x7fELF".to_vec();
        bytes.extend_from_slice(&temps_vm_agent::BUILD_MARKER);
        assert_eq!(find_build(&bytes).as_deref(), Some(BUILD_COMMIT));
    }

    #[test]
    fn classification_distinguishes_current_outdated_and_unknown() {
        assert_eq!(
            classify(Some("abc1234"), "abc1234"),
            AgentBuildStatus::Current
        );
        assert_eq!(
            classify(Some("abc1234"), "def5678"),
            AgentBuildStatus::Outdated {
                agent: Some("abc1234".into()),
                host: "def5678".into()
            }
        );
        assert_eq!(
            classify(None, "def5678"),
            AgentBuildStatus::Outdated {
                agent: None,
                host: "def5678".into()
            }
        );
        // Without commit information on either side there is nothing to say.
        assert_eq!(
            classify(Some("unknown"), "def5678"),
            AgentBuildStatus::Unknown
        );
        assert_eq!(
            classify(Some("abc1234"), "unknown"),
            AgentBuildStatus::Unknown
        );
        assert_eq!(classify(None, "unknown"), AgentBuildStatus::Unknown);
    }

    #[test]
    fn reading_a_missing_file_is_an_error_not_a_missing_marker() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_agent_build(&dir.path().join("absent")).is_err());
        let file = dir.path().join("agent");
        std::fs::write(&file, with_marker("0a1b2c3")).unwrap();
        assert_eq!(read_agent_build(&file).unwrap().as_deref(), Some("0a1b2c3"));
    }
}
