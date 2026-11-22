// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The eBPF datapath (network access plan, §4.15).
//!
//! - [`compile`]: policies and identities → map contents. Pure; tested on
//!   every platform.
//! - `loader` (Linux only): loads the embedded programs, pins their maps and
//!   links under [`temps_netpol_ebpf_common::PIN_DIR`], attaches them to the
//!   workload slice hierarchy, and applies a [`DatapathPlan`].
//! - [`slice_unit`] / [`slice_cgroup_path`]: where a workload's containers
//!   run, so the cgroup id the programs read names exactly one workload.
//! - [`cgroup_target`]: which cgroup identifies a running container, and
//!   whether the programs still have to be attached to it.
//! - [`CgroupDatapath`]: what the node enforcer drives, so the enforcer is
//!   the same code on every platform and in tests.

use std::path::{Path, PathBuf};

use temps_netpol_ebpf_common::{Sample, PIN_DIR};
use temps_workload::WorkloadRef;

use crate::enforcer::NodeMode;

pub mod compile;
#[cfg(target_os = "linux")]
pub mod loader;

pub use compile::{
    compile_datapath, describe_identity, identity_of, DatapathCompileError, DatapathInput,
    DatapathPlan, INTERNAL_RANGES, MAX_EXPANDED_PORTS, METADATA_ADDRESSES,
};

/// The compiled programs, when this build embeds them (`ebpf` feature).
#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub static PROGRAMS: Option<&[u8]> = Some(aya::include_bytes_aligned!(concat!(
    env!("OUT_DIR"),
    "/temps-netpol-ebpf.o"
)));
/// The compiled programs, when this build embeds them (`ebpf` feature).
#[cfg(not(all(target_os = "linux", feature = "ebpf")))]
pub static PROGRAMS: Option<&[u8]> = None;

/// Where the programs attach and how their maps are sized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatapathConfig {
    pub pin_dir: PathBuf,
    /// The cgroup every workload slice lives under (`temps.slice`). The
    /// programs are attached to it when it exists; containers created
    /// outside it get the programs on their own scope instead.
    pub attach_cgroup: PathBuf,
    /// Deepest ancestor level the programs look at for a registered cgroup.
    pub max_level: u32,
    /// `nodes.netpol_ipcache_max`.
    pub ipcache_max: u32,
    /// `nodes.netpol_policy_max`.
    pub policy_max: u32,
    pub sample_interval_us: u32,
}

impl Default for DatapathConfig {
    fn default() -> Self {
        Self {
            pin_dir: PathBuf::from(PIN_DIR),
            attach_cgroup: PathBuf::from(CGROUP_ROOT).join(WORKLOAD_ROOT_SLICE),
            max_level: 8,
            ipcache_max: 65_536,
            policy_max: 65_536,
            sample_interval_us: 200_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DatapathError {
    #[error("this build of temps does not contain the network policy eBPF programs; it was built without the `ebpf` feature of temps-workload-netpol")]
    NotBuiltIn,
    #[error("cannot enforce network policy: {requirement}. {fix}")]
    Requirement { requirement: String, fix: String },
    #[error("{operation} {path}: {reason}")]
    Io {
        operation: &'static str,
        path: String,
        reason: String,
    },
    #[error("loading the network policy programs failed: {reason}")]
    Load { reason: String },
    #[error("network policy program {program}: {operation} failed: {reason}")]
    Program {
        program: &'static str,
        operation: String,
        reason: String,
    },
    #[error("network policy map {map}: {operation} failed: {reason}")]
    Map {
        map: &'static str,
        operation: String,
        reason: String,
    },
    #[error("network policy map {map} needs {needed} entries but holds at most {max}; raise the node's limit")]
    Capacity {
        map: &'static str,
        needed: usize,
        max: u32,
    },
}

/// What one apply changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub endpoints_set: usize,
    pub endpoints_removed: usize,
    pub addresses_set: usize,
    pub addresses_removed: usize,
    pub policy_entries: usize,
    /// Scopes the programs were attached to by this apply.
    pub scopes_attached: usize,
    /// Scope attachments released because their container is gone.
    pub scopes_released: usize,
    /// Local workloads in the plan with no running cgroup yet.
    pub without_cgroup: Vec<WorkloadRef>,
}

/// Summed over CPUs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub egress_allow: u64,
    pub egress_deny: u64,
    pub ingress_allow: u64,
    pub ingress_deny: u64,
    pub would_block: u64,
    pub unknown_cgroup: u64,
    pub samples_dropped: u64,
    pub samples_rate_limited: u64,
}

/// A cgroup that identifies instances of one workload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CgroupTarget {
    /// The cgroup id (inode of its directory).
    pub id: u64,
    pub workload: WorkloadRef,
    /// The directory to attach the programs to, for a container outside
    /// `temps.slice`; `None` when an attach point above already covers it.
    pub attach: Option<PathBuf>,
}

/// The node enforcer's view of the eBPF datapath.
pub trait CgroupDatapath: Send + Sync {
    fn set_mode(&mut self, mode: NodeMode) -> Result<(), DatapathError>;
    /// Makes the maps hold exactly `plan`, with each target's cgroup
    /// carrying its workload's endpoint.
    fn apply(
        &mut self,
        plan: &DatapathPlan,
        targets: &[CgroupTarget],
    ) -> Result<ApplyStats, DatapathError>;
    fn counters(&self) -> Result<Counters, DatapathError>;
    /// Up to `max` blocked-flow samples, oldest first.
    fn drain_samples(&mut self, max: usize) -> Vec<Sample>;
}

/// Opens the datapath the first time a pass needs it. Kept lazy so a node
/// in `legacy` never loads a program.
pub type DatapathOpener =
    Box<dyn Fn() -> Result<Box<dyn CgroupDatapath>, DatapathError> + Send + Sync>;

/// Where cgroup v2 is mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// The cgroup that identifies a container whose init process is in
/// `cgroup` (the `0::` line of `/proc/<pid>/cgroup`), relative to
/// `cgroup_root`.
///
/// - Under `temps.slice`: the workload slice three levels down, which the
///   programs attached to `temps.slice` already cover.
/// - Anywhere else: the container's Docker scope (`docker-<id>.scope`), or
///   the cgroup itself when there is none, to which the programs must be
///   attached. Taking the scope rather than the leaf keeps a container that
///   creates cgroups of its own (systemd inside it) one identity.
///
/// `None` for the root cgroup, which is never a container's.
pub fn cgroup_target(cgroup_root: &Path, cgroup: &str) -> Option<(PathBuf, bool)> {
    let parts: Vec<&str> = cgroup
        .trim()
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    if parts.is_empty() {
        return None;
    }
    if parts[0] == WORKLOAD_ROOT_SLICE {
        if parts.len() < 3 {
            // In temps.slice but not in a workload slice: covered by the
            // attach point and refused while unidentified.
            return None;
        }
        let dir = parts[..3]
            .iter()
            .fold(cgroup_root.to_path_buf(), |dir, p| dir.join(p));
        return Some((dir, false));
    }
    let end = parts
        .iter()
        .position(|p| p.starts_with("docker-") && p.ends_with(".scope"))
        .map(|i| i + 1)
        .unwrap_or(parts.len());
    let dir = parts[..end]
        .iter()
        .fold(cgroup_root.to_path_buf(), |dir, p| dir.join(p));
    Some((dir, true))
}

/// What the enforcer reads from the host to place containers: their
/// cgroups and the node's own addresses. Behind a trait so passes can be
/// tested without a Linux host.
pub trait CgroupHost: Send + Sync {
    /// The unified cgroup path of process `pid`.
    fn cgroup_of(&self, pid: i64) -> Option<String>;
    /// The id of a cgroup directory, `None` when it is gone.
    fn cgroup_id(&self, dir: &Path) -> Option<u64>;
    /// Addresses on this host's interfaces, loopback excluded.
    fn addresses(&self) -> Vec<std::net::IpAddr>;
}

/// [`CgroupHost`] reading `/proc`, cgroupfs and the interfaces.
pub struct LocalCgroupHost;

impl CgroupHost for LocalCgroupHost {
    fn cgroup_of(&self, pid: i64) -> Option<String> {
        read_proc_cgroup(pid)
    }
    fn cgroup_id(&self, dir: &Path) -> Option<u64> {
        read_cgroup_id(dir)
    }
    fn addresses(&self) -> Vec<std::net::IpAddr> {
        host_addresses()
    }
}

/// The unified cgroup path of process `pid`, from `/proc/<pid>/cgroup`.
/// `None` when the process is gone or the host has no such file.
pub fn read_proc_cgroup(pid: i64) -> Option<String> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    unified_cgroup(&contents).map(str::to_string)
}

/// The cgroup id of a cgroup directory (its inode), `None` when it is gone.
pub fn read_cgroup_id(dir: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).ok().map(|m| m.ino())
}

/// Every address configured on this host's interfaces, loopback excluded.
#[cfg(target_os = "linux")]
pub fn host_addresses() -> Vec<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we free below; every
    // pointer is checked for null and the sockaddr is read according to its
    // family.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cursor = head;
        while !cursor.is_null() {
            let addr = (*cursor).ifa_addr;
            if !addr.is_null() {
                match i32::from((*addr).sa_family) {
                    libc::AF_INET => {
                        let sin = &*(addr as *const libc::sockaddr_in);
                        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        if !ip.is_loopback() {
                            out.push(IpAddr::V4(ip));
                        }
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(addr as *const libc::sockaddr_in6);
                        let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                        if !ip.is_loopback() {
                            out.push(IpAddr::V6(ip));
                        }
                    }
                    _ => {}
                }
            }
            cursor = (*cursor).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out.sort();
    out.dedup();
    out
}

/// Every address configured on this host's interfaces, loopback excluded.
#[cfg(not(target_os = "linux"))]
pub fn host_addresses() -> Vec<std::net::IpAddr> {
    Vec::new()
}

/// The eBPF datapath, when this build embeds the programs and this host can
/// run them; otherwise why not, for the node's status.
pub fn available_datapath(config: DatapathConfig) -> Result<DatapathOpener, DatapathError> {
    #[cfg(target_os = "linux")]
    {
        let object = PROGRAMS.ok_or(DatapathError::NotBuiltIn)?;
        loader::check_requirements()?;
        Ok(Box::new(move || {
            ensure_root_slice(&config.attach_cgroup);
            let datapath = loader::EbpfDatapath::open(config.clone(), object)?;
            Ok(Box::new(datapath) as Box<dyn CgroupDatapath>)
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(DatapathError::Requirement {
            requirement: "the eBPF datapath only runs on Linux".into(),
            fix: "Run the node on a Linux host".into(),
        })
    }
}

/// Starts `temps.slice` so the programs can be attached to it before any
/// workload runs there. Without systemd it stays absent, and every
/// container is covered through its own scope instead.
#[cfg(target_os = "linux")]
fn ensure_root_slice(attach_cgroup: &Path) {
    if attach_cgroup.is_dir() {
        return;
    }
    match std::process::Command::new("systemctl")
        .args(["start", WORKLOAD_ROOT_SLICE])
        .output()
    {
        Ok(out) if out.status.success() => {
            tracing::info!(slice = WORKLOAD_ROOT_SLICE, "started the workload slice");
        }
        Ok(out) => tracing::warn!(
            slice = WORKLOAD_ROOT_SLICE,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "could not start the workload slice; containers are covered through their own scopes"
        ),
        Err(e) => tracing::warn!(
            slice = WORKLOAD_ROOT_SLICE,
            error = %e,
            "systemctl is not available; containers are covered through their own scopes"
        ),
    }
}

/// The unified-hierarchy path in the contents of `/proc/<pid>/cgroup`.
pub fn unified_cgroup(proc_cgroup: &str) -> Option<&str> {
    proc_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
}

/// The parent slice of every workload slice.
pub const WORKLOAD_ROOT_SLICE: &str = "temps.slice";

/// The systemd slice a workload's containers run under, passed to Docker as
/// `HostConfig.cgroup_parent`: `temps-sandbox-7.slice`.
pub fn slice_unit(workload: &WorkloadRef) -> String {
    temps_workload::labels::workload_slice(*workload)
}

/// The cgroup directory of [`slice_unit`] under `cgroup_root`. systemd nests
/// a slice under the slices named by its dash-separated prefixes:
/// `temps.slice/temps-sandbox.slice/temps-sandbox-7.slice`.
pub fn slice_cgroup_path(cgroup_root: &Path, workload: &WorkloadRef) -> PathBuf {
    let kind = workload.kind.as_str();
    cgroup_root
        .join(WORKLOAD_ROOT_SLICE)
        .join(format!("temps-{kind}.slice"))
        .join(slice_unit(workload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    #[test]
    fn a_container_in_a_workload_slice_is_identified_by_the_slice() {
        let root = Path::new("/sys/fs/cgroup");
        assert_eq!(
            cgroup_target(
                root,
                "/temps.slice/temps-sandbox.slice/temps-sandbox-7.slice/docker-ab.scope"
            ),
            Some((
                PathBuf::from(
                    "/sys/fs/cgroup/temps.slice/temps-sandbox.slice/temps-sandbox-7.slice"
                ),
                false
            ))
        );
        assert_eq!(
            cgroup_target(root, "/temps.slice/temps-sandbox.slice"),
            None
        );
    }

    #[test]
    fn a_container_elsewhere_is_identified_by_its_scope() {
        let root = Path::new("/sys/fs/cgroup");
        assert_eq!(
            cgroup_target(root, "/system.slice/docker-ab.scope"),
            Some((
                PathBuf::from("/sys/fs/cgroup/system.slice/docker-ab.scope"),
                true
            ))
        );
        assert_eq!(
            cgroup_target(root, "/system.slice/docker-ab.scope/init.scope"),
            Some((
                PathBuf::from("/sys/fs/cgroup/system.slice/docker-ab.scope"),
                true
            )),
            "a cgroup the container made itself is still the container"
        );
        assert_eq!(
            cgroup_target(root, "/docker/ab"),
            Some((PathBuf::from("/sys/fs/cgroup/docker/ab"), true)),
            "cgroupfs driver"
        );
        assert_eq!(cgroup_target(root, "/"), None);
        assert_eq!(
            cgroup_target(root, "/system.slice/../../etc"),
            Some((PathBuf::from("/sys/fs/cgroup/system.slice/etc"), true)),
            "dot segments are never followed"
        );
    }

    #[test]
    fn reads_the_unified_line() {
        assert_eq!(
            unified_cgroup("12:pids:/x\n0::/system.slice/docker-ab.scope\n"),
            Some("/system.slice/docker-ab.scope")
        );
        assert_eq!(unified_cgroup("12:pids:/x\n"), None);
    }

    #[test]
    fn slices_follow_systemd_nesting() {
        let sandbox = WorkloadRef::new(WorkloadKind::Sandbox, 7);
        assert_eq!(slice_unit(&sandbox), "temps-sandbox-7.slice");
        assert_eq!(
            slice_cgroup_path(Path::new("/sys/fs/cgroup"), &sandbox),
            PathBuf::from("/sys/fs/cgroup/temps.slice/temps-sandbox.slice/temps-sandbox-7.slice")
        );
    }
}
