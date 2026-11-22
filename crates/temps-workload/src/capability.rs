// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Linux capabilities a project may opt back into, and the decoder for the
//! file capabilities an image's entrypoint carries.
//!
//! Every project container starts with `cap_drop: ALL` and
//! `no-new-privileges`. Some images ship an entrypoint marked with
//! `setcap cap_net_bind_service=+ep` so a non-root process can listen on
//! port 80. The kernel refuses to `execve` such a "capability-dumb" binary
//! when a capability it names is missing from the bounding set, so the
//! container dies before printing anything of its own (`Operation not
//! permitted`, exit 126 through a shell).
//!
//! [`ContainerCapability`] is the **allow-list** of capabilities a project
//! may add back. It is closed on purpose: a capability is added here only
//! with a written justification, never from user input. `SYS_ADMIN`,
//! `privileged` and arbitrary names are not representable.
//!
//! Adding `NET_BIND_SERVICE` keeps `no-new-privileges`: the capability only
//! enters the bounding set, which is what the exec check needs. With
//! `no-new-privileges` the process is not granted it; binding a low port
//! still works because Docker sets `net.ipv4.ip_unprivileged_port_start=0`
//! in every non-host network namespace.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A Linux capability a project may add back to its containers.
///
/// Wire and Docker name are the same: `NET_BIND_SERVICE`, without the
/// `CAP_` prefix, the form Docker's `cap_add` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ContainerCapability {
    /// Bind a socket to a port below 1024. Images that `setcap` it on their
    /// entrypoint (to listen on 80/443 as a non-root user) cannot start
    /// without it.
    #[serde(rename = "NET_BIND_SERVICE")]
    NetBindService,
}

impl ContainerCapability {
    /// Every capability a project may add, in display order.
    pub const ALL: &'static [ContainerCapability] = &[ContainerCapability::NetBindService];

    /// The Docker `cap_add` name, which is also the wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            ContainerCapability::NetBindService => "NET_BIND_SERVICE",
        }
    }

    /// The capability's bit in the kernel's capability sets.
    pub const fn linux_bit(self) -> u8 {
        match self {
            ContainerCapability::NetBindService => 10,
        }
    }

    /// The allow-listed capability with kernel bit `bit`, if any.
    pub fn from_linux_bit(bit: u8) -> Option<Self> {
        Self::ALL.iter().copied().find(|cap| cap.linux_bit() == bit)
    }

    /// Parse a name a person or client sent: `NET_BIND_SERVICE`,
    /// `CAP_NET_BIND_SERVICE` or the lower-case `setcap` spelling.
    ///
    /// A real Linux capability that is not on the allow-list is
    /// [`CapabilityError::NotAllowed`]; anything else (including `ALL` and
    /// `privileged`) is [`CapabilityError::Unknown`] or `NotAllowed`.
    pub fn parse(name: &str) -> Result<Self, CapabilityError> {
        let trimmed = name.trim();
        let upper = trimmed.to_ascii_uppercase();
        let bare = upper.strip_prefix("CAP_").unwrap_or(&upper);
        if let Some(cap) = Self::ALL.iter().copied().find(|cap| cap.as_str() == bare) {
            return Ok(cap);
        }
        if bare == "ALL" || bare == "PRIVILEGED" || LINUX_CAPABILITIES.contains(&bare) {
            return Err(CapabilityError::NotAllowed {
                name: bounded(trimmed),
            });
        }
        Err(CapabilityError::Unknown {
            name: bounded(trimmed),
        })
    }

    /// Parse a whole list, rejecting the first invalid entry. Duplicates
    /// collapse; the result is sorted.
    pub fn parse_set<S: AsRef<str>>(
        names: &[S],
    ) -> Result<BTreeSet<ContainerCapability>, CapabilityError> {
        names
            .iter()
            .map(|name| Self::parse(name.as_ref()))
            .collect()
    }

    /// Parse stored values, dropping (fail closed) any this build does not
    /// know, e.g. a value written by a newer version before a downgrade.
    /// Returns the kept set and the dropped names.
    pub fn from_stored<S: AsRef<str>>(names: &[S]) -> (BTreeSet<ContainerCapability>, Vec<String>) {
        let mut kept = BTreeSet::new();
        let mut dropped = Vec::new();
        for name in names {
            match Self::parse(name.as_ref()) {
                Ok(cap) => {
                    kept.insert(cap);
                }
                Err(_) => dropped.push(bounded(name.as_ref())),
            }
        }
        (kept, dropped)
    }
}

impl fmt::Display for ContainerCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a capability name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error(
        "Linux capability '{name}' cannot be granted to project containers; only {allowed} may be allowed",
        allowed = allowed_list()
    )]
    NotAllowed { name: String },
    #[error(
        "'{name}' is not a Linux capability; only {allowed} may be allowed",
        allowed = allowed_list()
    )]
    Unknown { name: String },
}

fn allowed_list() -> String {
    ContainerCapability::ALL
        .iter()
        .map(|cap| cap.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Names echoed into errors are bounded so a request cannot make an error
/// (or a log line) arbitrarily large.
fn bounded(name: &str) -> String {
    const MAX: usize = 64;
    if name.chars().count() <= MAX {
        name.to_string()
    } else {
        let mut cut: String = name.chars().take(MAX).collect();
        cut.push('…');
        cut
    }
}

/// Kernel capability names by bit (`include/uapi/linux/capability.h`),
/// without the `CAP_` prefix.
pub const LINUX_CAPABILITIES: &[&str] = &[
    "CHOWN",
    "DAC_OVERRIDE",
    "DAC_READ_SEARCH",
    "FOWNER",
    "FSETID",
    "KILL",
    "SETGID",
    "SETUID",
    "SETPCAP",
    "LINUX_IMMUTABLE",
    "NET_BIND_SERVICE",
    "NET_BROADCAST",
    "NET_ADMIN",
    "NET_RAW",
    "IPC_LOCK",
    "IPC_OWNER",
    "SYS_MODULE",
    "SYS_RAWIO",
    "SYS_CHROOT",
    "SYS_PTRACE",
    "SYS_PACCT",
    "SYS_ADMIN",
    "SYS_BOOT",
    "SYS_NICE",
    "SYS_RESOURCE",
    "SYS_TIME",
    "SYS_TTY_CONFIG",
    "MKNOD",
    "LEASE",
    "AUDIT_WRITE",
    "AUDIT_CONTROL",
    "SETFCAP",
    "MAC_OVERRIDE",
    "MAC_ADMIN",
    "SYSLOG",
    "WAKE_ALARM",
    "BLOCK_SUSPEND",
    "AUDIT_READ",
    "PERFMON",
    "BPF",
    "CHECKPOINT_RESTORE",
];

/// The file capabilities set on a binary (`security.capability`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileCapabilities {
    /// Permitted capability names, kernel spelling without `CAP_`, in bit
    /// order. A bit this table does not name is `BIT_<n>`.
    pub permitted: Vec<String>,
    /// The effective flag: the binary is "capability-dumb" and the kernel
    /// refuses to exec it unless every permitted capability is available.
    pub effective: bool,
}

impl FileCapabilities {
    /// The permitted capabilities a project could allow.
    pub fn allowable(&self) -> BTreeSet<ContainerCapability> {
        self.permitted
            .iter()
            .filter_map(|name| ContainerCapability::parse(name).ok())
            .collect()
    }

    /// The permitted capabilities no project can allow.
    pub fn not_allowable(&self) -> Vec<String> {
        self.permitted
            .iter()
            .filter(|name| ContainerCapability::parse(name).is_err())
            .cloned()
            .collect()
    }
}

const VFS_CAP_REVISION_MASK: u32 = 0xFF00_0000;
const VFS_CAP_FLAGS_EFFECTIVE: u32 = 0x0000_0001;
const VFS_CAP_REVISION_1: u32 = 0x0100_0000;
const VFS_CAP_REVISION_2: u32 = 0x0200_0000;
const VFS_CAP_REVISION_3: u32 = 0x0300_0000;

/// Decode a `security.capability` xattr value (`struct vfs_cap_data` /
/// `vfs_ns_cap_data`, little endian). `None` for a value that is not one.
pub fn decode_file_capabilities(raw: &[u8]) -> Option<FileCapabilities> {
    let word = |index: usize| -> Option<u32> {
        let start = index * 4;
        let bytes: [u8; 4] = raw.get(start..start + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    };
    let magic = word(0)?;
    let (words, expected_len) = match magic & VFS_CAP_REVISION_MASK {
        VFS_CAP_REVISION_1 => (1, 12),
        VFS_CAP_REVISION_2 => (2, 20),
        VFS_CAP_REVISION_3 => (2, 24),
        _ => return None,
    };
    if raw.len() != expected_len {
        return None;
    }
    let mut permitted = Vec::new();
    for index in 0..words {
        // Layout: magic, then (permitted, inheritable) per 32-bit word.
        let bits = word(1 + index * 2)?;
        for bit in 0..32u32 {
            if bits & (1 << bit) != 0 {
                let number = index as u32 * 32 + bit;
                permitted.push(
                    LINUX_CAPABILITIES
                        .get(number as usize)
                        .map(|name| (*name).to_string())
                        .unwrap_or_else(|| format!("BIT_{number}")),
                );
            }
        }
    }
    Some(FileCapabilities {
        permitted,
        effective: magic & VFS_CAP_FLAGS_EFFECTIVE != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_is_net_bind_service_only() {
        assert_eq!(
            ContainerCapability::ALL,
            &[ContainerCapability::NetBindService]
        );
        let cap = ContainerCapability::NetBindService;
        assert_eq!(cap.as_str(), "NET_BIND_SERVICE");
        assert_eq!(LINUX_CAPABILITIES[cap.linux_bit() as usize], cap.as_str());
        assert_eq!(
            serde_json::to_value(cap).unwrap(),
            serde_json::json!("NET_BIND_SERVICE")
        );
        assert_eq!(ContainerCapability::from_linux_bit(10), Some(cap));
        assert_eq!(ContainerCapability::from_linux_bit(21), None);
    }

    #[test]
    fn parse_accepts_the_spellings_people_use() {
        for name in [
            "NET_BIND_SERVICE",
            "CAP_NET_BIND_SERVICE",
            "cap_net_bind_service",
            " net_bind_service ",
        ] {
            assert_eq!(
                ContainerCapability::parse(name),
                Ok(ContainerCapability::NetBindService),
                "{name}"
            );
        }
    }

    #[test]
    fn parse_rejects_forbidden_and_unknown_capabilities() {
        for name in [
            "SYS_ADMIN",
            "CAP_SYS_ADMIN",
            "NET_ADMIN",
            "ALL",
            "privileged",
            "DAC_OVERRIDE",
        ] {
            let error = ContainerCapability::parse(name).unwrap_err();
            assert!(
                matches!(error, CapabilityError::NotAllowed { .. }),
                "{name}: {error:?}"
            );
            assert!(error.to_string().contains("only NET_BIND_SERVICE"));
        }
        for name in ["", "NET_BIND", "rm -rf /", "NET_BIND_SERVICE,SYS_ADMIN"] {
            assert!(
                matches!(
                    ContainerCapability::parse(name),
                    Err(CapabilityError::Unknown { .. })
                ),
                "{name}"
            );
        }
        // Unknown names are bounded before they reach an error message.
        let long = "X".repeat(500);
        let error = ContainerCapability::parse(&long).unwrap_err().to_string();
        assert!(error.len() < 200, "{error}");
    }

    #[test]
    fn serde_rejects_anything_off_the_allow_list() {
        assert!(serde_json::from_str::<ContainerCapability>(r#""SYS_ADMIN""#).is_err());
        assert!(serde_json::from_str::<ContainerCapability>(r#""net_bind_service""#).is_err());
    }

    #[test]
    fn parse_set_dedupes_and_fails_on_the_first_bad_entry() {
        let set =
            ContainerCapability::parse_set(&["NET_BIND_SERVICE", "cap_net_bind_service"]).unwrap();
        assert_eq!(set.len(), 1);
        assert!(ContainerCapability::parse_set(&["NET_BIND_SERVICE", "SYS_ADMIN"]).is_err());
        assert!(ContainerCapability::parse_set::<&str>(&[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stored_values_this_build_does_not_know_fail_closed() {
        let (kept, dropped) =
            ContainerCapability::from_stored(&["NET_BIND_SERVICE", "SOMETHING_NEWER"]);
        assert_eq!(kept, BTreeSet::from([ContainerCapability::NetBindService]));
        assert_eq!(dropped, vec!["SOMETHING_NEWER".to_string()]);
    }

    #[test]
    fn decodes_the_xattr_setcap_writes_for_net_bind_service() {
        // Captured from a `setcap cap_net_bind_service=+ep` binary through
        // Docker's archive API (PAX `SCHILY.xattr.security.capability`).
        let raw = [
            0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let caps = decode_file_capabilities(&raw).unwrap();
        assert_eq!(caps.permitted, vec!["NET_BIND_SERVICE".to_string()]);
        assert!(caps.effective);
        assert_eq!(
            caps.allowable(),
            BTreeSet::from([ContainerCapability::NetBindService])
        );
        assert!(caps.not_allowable().is_empty());
    }

    #[test]
    fn decodes_v3_and_high_bits_and_names_capabilities_it_cannot_allow() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(VFS_CAP_REVISION_3).to_le_bytes()); // not effective
        raw.extend_from_slice(&((1u32 << 21) | (1 << 10)).to_le_bytes()); // SYS_ADMIN, NBS
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&(1u32 << 7).to_le_bytes()); // bit 39 = BPF
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes()); // rootid
        let caps = decode_file_capabilities(&raw).unwrap();
        assert!(!caps.effective);
        assert_eq!(caps.permitted, vec!["NET_BIND_SERVICE", "SYS_ADMIN", "BPF"]);
        assert_eq!(caps.not_allowable(), vec!["SYS_ADMIN", "BPF"]);
    }

    #[test]
    fn rejects_values_that_are_not_capability_data() {
        assert_eq!(decode_file_capabilities(&[]), None);
        assert_eq!(decode_file_capabilities(&[0, 0, 0, 9, 0, 0, 0, 0]), None);
        // Right revision, wrong length.
        assert_eq!(decode_file_capabilities(&[1, 0, 0, 2, 0, 4, 0, 0]), None);
    }
}
