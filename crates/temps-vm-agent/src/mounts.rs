// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Argument preparation for the boot-time `mount(2)` calls.
//!
//! The syscall itself is Linux-only and lives in the agent binary; building
//! its NUL-terminated arguments is plain Rust, so it is kept here where it
//! compiles — and is tested — on every platform.

use std::ffi::CString;

use thiserror::Error;

/// Why a mount could not be attempted or did not succeed.
#[derive(Debug, Error)]
pub enum MountError {
    /// A mount argument contained an interior NUL byte and so cannot be
    /// passed to the kernel as a C string.
    #[error("mount of {fstype} on {target}: {field} contains a NUL byte")]
    InvalidArgument {
        target: String,
        fstype: String,
        field: &'static str,
    },
    /// The kernel rejected the mount.
    #[error("mount of {fstype} on {target} failed: {source}")]
    Syscall {
        target: String,
        fstype: String,
        #[source]
        source: std::io::Error,
    },
}

/// The C-string arguments of one `mount(2)` call.
#[derive(Debug)]
pub struct MountArgs {
    pub source: CString,
    pub target: CString,
    pub fstype: CString,
    pub data: Option<CString>,
}

impl MountArgs {
    /// Convert the four string arguments, naming the offending field and the
    /// mount target if any contains a NUL byte.
    pub fn new(
        source: &str,
        target: &str,
        fstype: &str,
        data: Option<&str>,
    ) -> Result<Self, MountError> {
        let convert = |value: &str, field: &'static str| {
            CString::new(value).map_err(|_| MountError::InvalidArgument {
                target: target.to_string(),
                fstype: fstype.to_string(),
                field,
            })
        };
        Ok(Self {
            source: convert(source, "source")?,
            target: convert(target, "target")?,
            fstype: convert(fstype, "fstype")?,
            data: data.map(|d| convert(d, "data")).transpose()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_convert() {
        let args = MountArgs::new("devpts", "/dev/pts", "devpts", Some("mode=0620"))
            .expect("plain strings convert");
        assert_eq!(args.target.to_str().unwrap(), "/dev/pts");
        assert_eq!(args.data.unwrap().to_str().unwrap(), "mode=0620");
    }

    #[test]
    fn data_is_optional() {
        let args = MountArgs::new("proc", "/proc", "proc", None).expect("converts");
        assert!(args.data.is_none());
    }

    #[test]
    fn interior_nul_names_the_field_and_mount_target() {
        let err = MountArgs::new("tmpfs", "/run\0evil", "tmpfs", None).unwrap_err();
        let message = err.to_string();
        assert!(matches!(
            err,
            MountError::InvalidArgument {
                field: "target",
                ..
            }
        ));
        assert!(message.contains("tmpfs"), "{message}");
        assert!(message.contains("target"), "{message}");
    }

    #[test]
    fn nul_in_mount_data_is_reported_as_data() {
        let err = MountArgs::new("devpts", "/dev/pts", "devpts", Some("a\0b")).unwrap_err();
        assert!(matches!(
            err,
            MountError::InvalidArgument { field: "data", .. }
        ));
    }
}
