// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! microsandbox microVM sandbox backend (ADR-050). Experimental.
//!
//! Each sandbox is a libkrun microVM driven through the `microsandbox` Rust
//! SDK, linked as a library. The SDK pulls OCI images natively (no Docker),
//! boots the guest with its own bundled kernel (`libkrunfw`), and talks to
//! an in-guest agent for exec and filesystem operations. Networking is the
//! SDK's userspace stack with a host-side policy engine — no TAP devices,
//! no root setup.
//!
//! The SDK runs each VM in a separate `msb` runtime process (the VMM). That
//! process is resolved from — and installed into, by `temps microsandbox
//! setup` — a Temps-owned state directory, never from the user's
//! `~/.microsandbox`:
//!
//!   <data_dir>/microsandbox/          SDK home (MSB_HOME equivalent), 0700
//!     config.json                     SDK config (Temps-owned; absent = defaults)
//!     bin/msb, lib/libkrunfw.*        pinned runtime pair
//!     db/, sandboxes/, cache/, ...    SDK-managed state
//!
//! Every SDK call is bound to an explicitly-built [`LocalBackend`] — the
//! SDK's ambient default backend also honours `MSB_BACKEND` / `MSB_API_KEY`
//! / `MSB_PROFILE`, any of which could otherwise route a sandbox to a remote
//! service. Nothing here reads those.
//!
//! This backend complements Firecracker (ADR-029); it does not replace it.
//! libkrun runs the guest and the VMM in one security context, so a guest
//! that escapes into the VMM holds whatever the VMM process can reach. See
//! ADR-050 for the confinement applied today and what is deferred.
//!
//! The SDK-backed provider lives in [`sdk`] and is compiled for every target
//! except musl, where the SDK's VMM crate does not build; there the backend
//! reports itself unavailable with that reason. Everything here — status,
//! host probing, configuration — is target-independent.

use std::path::{Path, PathBuf};

use crate::error::AgentError;

#[cfg(not(target_env = "musl"))]
mod sdk;
#[cfg(not(target_env = "musl"))]
pub use sdk::MicrosandboxSandboxProvider;

/// VM name prefix — the routing provider dispatches recovery on this, and it
/// keeps Temps-owned sandboxes distinguishable in the SDK's own registry.
pub const MSB_SANDBOX_NAME_PREFIX: &str = "temps-msbsandbox-";

/// Provider name used in logs and error messages.
pub const PROVIDER_NAME: &str = "microsandbox";

/// SDK crate version this backend is built and tested against. The runtime
/// pair (`msb` + `libkrunfw`) installed by `temps microsandbox setup` is the
/// matching release; the SDK refuses a mismatched pair.
pub const MICROSANDBOX_VERSION: &str = "0.7.7";

/// Console page that shows backend status and setup instructions.
pub const SETUP_PATH: &str = "/agent-sandbox/sandbox";

/// Command that installs the runtime pair for this backend.
pub const SETUP_COMMAND: &str = "temps microsandbox setup";

/// Working directory inside the guest. Shared with the Firecracker backend
/// (`temps_vm_agent::WORK_DIR`) so callers see one layout for both microVM
/// backends.
pub const WORK_DIR: &str = temps_vm_agent::WORK_DIR;

/// Why the microsandbox backend can't run on this host. Every variant names
/// what was checked so the console can render an actionable onboarding
/// state instead of a bare "unavailable".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MicrosandboxUnavailable {
    #[error(
        "microsandbox is not supported on {os}/{arch}: it requires Linux with KVM or macOS on Apple Silicon"
    )]
    UnsupportedPlatform { os: String, arch: String },

    #[error("hardware virtualization is unavailable on this host: {detail}")]
    HypervisorUnavailable { detail: String },

    #[error(
        "microsandbox runtime v{version} (msb + libkrunfw) is not installed under {home}: {detail}. Run `{SETUP_COMMAND}`"
    )]
    RuntimeNotInstalled {
        home: String,
        version: String,
        detail: String,
    },

    #[error("microsandbox configuration under {home} could not be loaded: {detail}")]
    InvalidConfiguration { home: String, detail: String },

    #[error(
        "this build of temps targets {target} libc, which the microsandbox runtime does not support; run the glibc Linux or macOS release binary instead of the container image"
    )]
    UnsupportedBuild { target: String },
}

impl From<MicrosandboxUnavailable> for AgentError {
    fn from(error: MicrosandboxUnavailable) -> Self {
        AgentError::SandboxProviderUnavailable {
            provider: PROVIDER_NAME.to_string(),
            reason: error.to_string(),
        }
    }
}

/// Backend readiness for the settings/status API. Always reported: when the
/// backend is not configured it says why and where to set it up, so the
/// console can onboard instead of hiding the option.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct MicrosandboxCapability {
    /// Whether sandboxes can be created on this backend right now.
    pub configured: bool,
    /// Why the backend is unavailable, when `configured` is false.
    pub reason: Option<String>,
    /// Console path that shows status and setup instructions.
    pub setup_path: Option<String>,
    /// Shell command that installs the runtime, when installing it is the fix.
    pub setup_command: Option<String>,
    /// Runtime version this build of Temps expects.
    pub runtime_version: String,
}

impl MicrosandboxCapability {
    fn from_probe(probe: Result<(), MicrosandboxUnavailable>) -> Self {
        match probe {
            Ok(()) => Self {
                configured: true,
                reason: None,
                setup_path: None,
                setup_command: None,
                runtime_version: MICROSANDBOX_VERSION.to_string(),
            },
            Err(error) => {
                // Only a missing runtime is fixed by the setup command; a host
                // without a hypervisor needs a different machine or BIOS/VM
                // setting, which the reason explains.
                let setup_command = matches!(
                    error,
                    MicrosandboxUnavailable::RuntimeNotInstalled { .. }
                        | MicrosandboxUnavailable::InvalidConfiguration { .. }
                )
                .then(|| SETUP_COMMAND.to_string());
                Self {
                    configured: false,
                    reason: Some(error.to_string()),
                    setup_path: Some(SETUP_PATH.to_string()),
                    setup_command,
                    runtime_version: MICROSANDBOX_VERSION.to_string(),
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MicrosandboxSandboxConfig {
    /// Temps data directory (`$TEMPS_DATA_DIR` / `~/.temps`).
    pub data_dir: PathBuf,
    pub default_vcpus: u8,
    pub default_memory_mib: u32,
}

impl MicrosandboxSandboxConfig {
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            default_vcpus: 1,
            default_memory_mib: 512,
        }
    }

    /// SDK home: runtime pair, image cache, SDK database and VM state.
    pub fn home(&self) -> PathBuf {
        self.data_dir.join("microsandbox")
    }
}

/// What the hypervisor probe saw on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hypervisor {
    Available,
    Unavailable(String),
}

/// Decide whether `os`/`arch` with the given hypervisor state can run
/// microsandbox. Pure so every branch is unit-tested.
fn classify_host(
    os: &str,
    arch: &str,
    hypervisor: Hypervisor,
) -> Result<(), MicrosandboxUnavailable> {
    let supported = matches!((os, arch), ("linux", _) | ("macos", "aarch64"));
    if !supported {
        return Err(MicrosandboxUnavailable::UnsupportedPlatform {
            os: os.to_string(),
            arch: arch.to_string(),
        });
    }
    match hypervisor {
        Hypervisor::Available => Ok(()),
        Hypervisor::Unavailable(detail) => {
            Err(MicrosandboxUnavailable::HypervisorUnavailable { detail })
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_hypervisor() -> Hypervisor {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    {
        Ok(_) => Hypervisor::Available,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Hypervisor::Unavailable(
            "/dev/kvm does not exist (enable KVM, or nested virtualization on a VM host)"
                .to_string(),
        ),
        Err(e) => Hypervisor::Unavailable(format!(
            "/dev/kvm is not readable and writable by this user ({}); add the user to the kvm group",
            e
        )),
    }
}

#[cfg(target_os = "macos")]
fn probe_hypervisor() -> Hypervisor {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated literal and `value`/`size`
    // describe a correctly sized, writable c_int buffer.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.hv_support".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && value == 1 {
        Hypervisor::Available
    } else {
        Hypervisor::Unavailable(
            "Hypervisor.framework is not available (sysctl kern.hv_support != 1)".to_string(),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_hypervisor() -> Hypervisor {
    Hypervisor::Unavailable("no supported hypervisor on this platform".to_string())
}

pub(crate) fn host_virtualization() -> Result<(), MicrosandboxUnavailable> {
    classify_host(
        std::env::consts::OS,
        std::env::consts::ARCH,
        probe_hypervisor(),
    )
}

/// Readiness of the microsandbox backend for `data_dir`, without
/// constructing the full provider stack — used by the settings status
/// endpoint and by the standalone sandbox API to explain a rejected request.
pub fn microsandbox_capability(data_dir: &Path) -> MicrosandboxCapability {
    let config = MicrosandboxSandboxConfig::from_data_dir(data_dir.to_path_buf());
    MicrosandboxCapability::from_probe(host_virtualization().and_then(|()| runtime_probe(&config)))
}

/// Whether the pinned runtime pair is installed under the config's home.
#[cfg(not(target_env = "musl"))]
fn runtime_probe(config: &MicrosandboxSandboxConfig) -> Result<(), MicrosandboxUnavailable> {
    sdk::runtime_probe(config)
}

/// musl builds (the Alpine container image) don't link the SDK: its VMM
/// crate does not compile against musl's `pthread_t`.
#[cfg(target_env = "musl")]
fn runtime_probe(_config: &MicrosandboxSandboxConfig) -> Result<(), MicrosandboxUnavailable> {
    Err(MicrosandboxUnavailable::UnsupportedBuild {
        target: "musl".to_string(),
    })
}

/// Temps data directory: `TEMPS_DATA_DIR` (a bootstrap value, read before
/// any database exists), else `~/.temps`. Same resolution the agents plugin
/// uses when it registers sandbox backends.
pub fn host_data_dir() -> PathBuf {
    std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".temps")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_classification_covers_every_platform_branch() {
        assert!(classify_host("linux", "x86_64", Hypervisor::Available).is_ok());
        assert!(classify_host("linux", "aarch64", Hypervisor::Available).is_ok());
        assert!(classify_host("macos", "aarch64", Hypervisor::Available).is_ok());

        assert_eq!(
            classify_host("macos", "x86_64", Hypervisor::Available),
            Err(MicrosandboxUnavailable::UnsupportedPlatform {
                os: "macos".into(),
                arch: "x86_64".into()
            })
        );
        assert!(matches!(
            classify_host("freebsd", "x86_64", Hypervisor::Available),
            Err(MicrosandboxUnavailable::UnsupportedPlatform { .. })
        ));
        assert_eq!(
            classify_host("linux", "x86_64", Hypervisor::Unavailable("no kvm".into())),
            Err(MicrosandboxUnavailable::HypervisorUnavailable {
                detail: "no kvm".into()
            })
        );
    }

    #[test]
    fn unavailable_reasons_name_what_was_checked() {
        let e = MicrosandboxUnavailable::RuntimeNotInstalled {
            home: "/data/microsandbox".into(),
            version: MICROSANDBOX_VERSION.into(),
            detail: "msb not found".into(),
        };
        let text = e.to_string();
        assert!(text.contains("/data/microsandbox"), "{text}");
        assert!(text.contains(MICROSANDBOX_VERSION), "{text}");
        assert!(text.contains(SETUP_COMMAND), "{text}");

        match AgentError::from(e) {
            AgentError::SandboxProviderUnavailable { provider, reason } => {
                assert_eq!(provider, "microsandbox");
                assert!(reason.contains("msb not found"), "{reason}");
            }
            other => panic!("expected SandboxProviderUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn capability_onboards_instead_of_hiding() {
        let ready = MicrosandboxCapability::from_probe(Ok(()));
        assert!(ready.configured);
        assert!(ready.reason.is_none());

        let missing =
            MicrosandboxCapability::from_probe(Err(MicrosandboxUnavailable::RuntimeNotInstalled {
                home: "/d".into(),
                version: MICROSANDBOX_VERSION.into(),
                detail: "absent".into(),
            }));
        assert!(!missing.configured);
        assert_eq!(missing.setup_path.as_deref(), Some(SETUP_PATH));
        assert_eq!(missing.setup_command.as_deref(), Some(SETUP_COMMAND));

        // Installing the runtime can't fix a host without a hypervisor; the
        // reason explains it and no misleading command is offered.
        let no_hv = MicrosandboxCapability::from_probe(Err(
            MicrosandboxUnavailable::HypervisorUnavailable {
                detail: "no kvm".into(),
            },
        ));
        assert!(!no_hv.configured);
        assert!(no_hv.reason.unwrap().contains("no kvm"));
        assert_eq!(no_hv.setup_path.as_deref(), Some(SETUP_PATH));
        assert!(no_hv.setup_command.is_none());
    }

    #[test]
    fn capability_reports_missing_runtime_for_an_empty_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cap = microsandbox_capability(dir.path());
        assert!(!cap.configured);
        let reason = cap.reason.unwrap();
        // Either the host can't virtualize, or (more commonly) the runtime
        // isn't installed under this fresh data dir — never "configured".
        assert!(
            reason.contains("not installed")
                || reason.contains("virtualization")
                || reason.contains("not supported"),
            "{reason}"
        );
    }

    #[test]
    fn unsupported_build_offers_no_setup_command() {
        let cap =
            MicrosandboxCapability::from_probe(Err(MicrosandboxUnavailable::UnsupportedBuild {
                target: "musl".into(),
            }));
        assert!(!cap.configured);
        let reason = cap.reason.unwrap();
        assert!(reason.contains("musl"), "{reason}");
        assert!(reason.contains("glibc"), "{reason}");
        assert_eq!(cap.setup_path.as_deref(), Some(SETUP_PATH));
        assert!(cap.setup_command.is_none());
    }
}
