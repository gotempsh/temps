// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Loads, attaches and feeds the network policy programs (Linux only).
//!
//! - **Everything is pinned** under the configured directory (by default
//!   [`PIN_DIR`]): maps by name, the `temps.slice` links as
//!   `link_<program>`, per-container scope links as
//!   `scope_<cgroup id>_<program>`. Enforcement therefore outlives the agent;
//!   a restarted agent reopens the pins and carries on.
//! - **Attaching is idempotent.** `CONFIG[CFG_PROGRAM_DIGEST]` records which
//!   object the pinned links run. The same object is not attached twice; a
//!   different one (an upgrade) is attached first and the old links are
//!   released after, so there is no moment without a program. They are
//!   BPF links, which the kernel always attaches as one of several programs
//!   (it refuses attach flags on them), so they coexist with programs
//!   systemd or other tools put on the same cgroups; with several programs
//!   on a path the kernel lets an operation through only if every one does.
//! - **Applying a plan**: identities and addresses are updated in place,
//!   removals first; the policy goes into the inactive map and one write to
//!   `CONFIG[CFG_ACTIVE_POLICY]` switches to it, so a program never sees half
//!   a policy. A scope is given its identity before the programs are
//!   attached to it, so its first judged operation already has one.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use aya::maps::lpm_trie::{Key, LpmTrie};
use aya::maps::{Array, HashMap, Map, MapData, PerCpuArray, RingBuf};
use aya::programs::links::{FdLink, PinnedLink};
use aya::programs::{CgroupAttachMode, CgroupSkb, CgroupSkbAttachType, CgroupSockAddr};
use aya::{Ebpf, EbpfLoader};
use temps_netpol_ebpf_common::*;
use tracing::{debug, info, warn};

use super::compile::{prefix_bytes_v4, prefix_bytes_v6, DatapathPlan};
use super::{ApplyStats, CgroupDatapath, CgroupTarget, Counters, DatapathConfig, DatapathError};
use crate::enforcer::NodeMode;
use crate::rule::Cidr;

type IpcacheEntry<'a> = (&'a Cidr, &'a u32);

const SCOPE_PIN_PREFIX: &str = "scope_";

fn map_err(
    map: &'static str,
    operation: impl Into<String>,
) -> impl FnOnce(aya::maps::MapError) -> DatapathError {
    let operation = operation.into();
    move |e| DatapathError::Map {
        map,
        operation,
        reason: e.to_string(),
    }
}

/// The cgroup id of a cgroup v2 directory (its inode number).
pub fn cgroup_id(path: &Path) -> Result<u64, DatapathError> {
    std::fs::metadata(path)
        .map(|m| m.ino())
        .map_err(|e| DatapathError::Io {
            operation: "reading the cgroup id of",
            path: path.display().to_string(),
            reason: e.to_string(),
        })
}

/// Linux capability numbers (`linux/capability.h`).
const CAP_NET_ADMIN: u32 = 12;
const CAP_SYS_ADMIN: u32 = 21;
const CAP_BPF: u32 = 39;

/// The effective capability set, from `/proc/self/status`.
fn effective_capabilities() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let hex = status.lines().find_map(|l| l.strip_prefix("CapEff:"))?;
    u64::from_str_radix(hex.trim(), 16).ok()
}

/// Whether the process may load programs and attach them to cgroups:
/// `CAP_SYS_ADMIN`, or `CAP_BPF` with `CAP_NET_ADMIN`.
fn may_load_programs(caps: u64) -> bool {
    let has = |cap: u32| caps & (1u64 << cap) != 0;
    has(CAP_SYS_ADMIN) || (has(CAP_BPF) && has(CAP_NET_ADMIN))
}

/// Checks what the datapath needs from the host (§4.15, "Requirements").
pub fn check_requirements() -> Result<(), DatapathError> {
    if !effective_capabilities().is_some_and(may_load_programs) {
        return Err(DatapathError::Requirement {
            requirement: "this process may not load eBPF programs (it needs CAP_SYS_ADMIN, or CAP_BPF and CAP_NET_ADMIN)".into(),
            fix: "Run the service as root, or add `AmbientCapabilities=CAP_BPF CAP_NET_ADMIN CAP_SYS_RESOURCE` to its systemd unit".into(),
        });
    }
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return Err(DatapathError::Requirement {
            requirement: "the host does not use cgroup v2".into(),
            fix: "Boot with `systemd.unified_cgroup_hierarchy=1` (the default on current distributions)".into(),
        });
    }
    if !Path::new("/sys/kernel/btf/vmlinux").exists() {
        return Err(DatapathError::Requirement {
            requirement: "the kernel has no BTF (/sys/kernel/btf/vmlinux is missing)".into(),
            fix: "Use a kernel built with CONFIG_DEBUG_INFO_BTF=y; every current distribution kernel is".into(),
        });
    }
    if !is_bpffs(Path::new("/sys/fs/bpf")) {
        return Err(DatapathError::Requirement {
            requirement: "the BPF filesystem is not mounted at /sys/fs/bpf".into(),
            fix: "Run `mount -t bpf bpf /sys/fs/bpf` (systemd mounts it by default)".into(),
        });
    }
    Ok(())
}

fn is_bpffs(path: &Path) -> bool {
    const BPF_FS_MAGIC: i64 = 0xcafe_4a11;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: statfs writes into a zeroed struct we own; the path is a valid
    // NUL-terminated string.
    unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        libc::statfs(c_path.as_ptr(), &mut stat) == 0 && stat.f_type as i64 == BPF_FS_MAGIC
    }
}

const PROGRAMS: [&str; 5] = [
    PROG_CONNECT4,
    PROG_CONNECT6,
    PROG_SENDMSG4,
    PROG_SENDMSG6,
    PROG_INGRESS,
];

fn object_digest(object: &[u8]) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    object.hash(&mut hasher);
    // Never 0, which means "nothing recorded".
    (hasher.finish() as u32).max(1)
}

pub struct EbpfDatapath {
    config: DatapathConfig,
    // Keeps program and map descriptors alive for the lifetime of the
    // datapath, and attaches scopes; enforcement itself lives in the pins.
    ebpf: Ebpf,
    identity: HashMap<MapData, u64, Endpoint>,
    ipcache_v4: LpmTrie<MapData, Ipv4Key, u32>,
    ipcache_v6: LpmTrie<MapData, Ipv6Key, u32>,
    policy_a: HashMap<MapData, PolicyKey, PolicyValue>,
    policy_b: HashMap<MapData, PolicyKey, PolicyValue>,
    cfg: Array<MapData, u32>,
    counters: PerCpuArray<MapData, u64>,
    samples: RingBuf<MapData>,
    /// Scope links pinned by a different object (an upgrade): the next
    /// apply attaches this object's programs next to them, then releases
    /// them.
    replace_scopes: bool,
}

fn take<T>(ebpf: &mut Ebpf, name: &'static str) -> Result<T, DatapathError>
where
    T: TryFrom<Map, Error = aya::maps::MapError>,
{
    let map = ebpf.take_map(name).ok_or_else(|| DatapathError::Map {
        map: name,
        operation: "finding it in the object".into(),
        reason: "missing".into(),
    })?;
    T::try_from(map).map_err(map_err(name, "opening"))
}

impl EbpfDatapath {
    /// Loads `object`, reusing pinned maps, and makes sure its programs are
    /// attached to `config.attach_cgroup`, when it exists, exactly once.
    pub fn open(config: DatapathConfig, object: &[u8]) -> Result<Self, DatapathError> {
        check_requirements()?;
        std::fs::create_dir_all(&config.pin_dir).map_err(|e| DatapathError::Io {
            operation: "creating",
            path: config.pin_dir.display().to_string(),
            reason: e.to_string(),
        })?;
        let mut ebpf = EbpfLoader::new()
            .default_map_pin_directory(&config.pin_dir)
            .map_max_entries(MAP_IPCACHE_V4, config.ipcache_max)
            .map_max_entries(MAP_IPCACHE_V6, config.ipcache_max)
            .map_max_entries(MAP_POLICY_A, config.policy_max)
            .map_max_entries(MAP_POLICY_B, config.policy_max)
            .load(object)
            .map_err(|e| DatapathError::Load {
                reason: e.to_string(),
            })?;
        for program in PROGRAMS {
            load_program(&mut ebpf, program)?;
        }

        let mut cfg: Array<MapData, u32> = take(&mut ebpf, MAP_CONFIG)?;
        let digest = object_digest(object);
        let recorded = cfg.get(&CFG_PROGRAM_DIGEST, 0).unwrap_or(0);
        if config.attach_cgroup.is_dir() {
            let links_present = PROGRAMS
                .iter()
                .all(|p| config.pin_dir.join(format!("link_{p}")).exists());
            if recorded == digest && links_present {
                debug!(pin_dir = %config.pin_dir.display(), "network policy programs already attached; reusing the pins");
            } else {
                attach_pinned(&mut ebpf, &config.attach_cgroup, &config.pin_dir, "link_")?;
                info!(
                    cgroup = %config.attach_cgroup.display(),
                    pin_dir = %config.pin_dir.display(),
                    replaced = recorded != 0,
                    "attached the network policy programs"
                );
            }
        } else {
            info!(
                cgroup = %config.attach_cgroup.display(),
                "the workload slice does not exist yet; programs are attached to each workload container's scope"
            );
        }
        cfg.set(CFG_LEVEL, config.max_level, 0)
            .map_err(map_err(MAP_CONFIG, "setting the deepest ancestor level"))?;
        cfg.set(CFG_SAMPLE_INTERVAL_US, config.sample_interval_us, 0)
            .map_err(map_err(MAP_CONFIG, "setting the sample interval"))?;
        cfg.set(CFG_PROGRAM_DIGEST, digest, 0)
            .map_err(map_err(MAP_CONFIG, "recording the program digest"))?;

        Ok(Self {
            identity: take(&mut ebpf, MAP_CGROUP_IDENTITY)?,
            ipcache_v4: take(&mut ebpf, MAP_IPCACHE_V4)?,
            ipcache_v6: take(&mut ebpf, MAP_IPCACHE_V6)?,
            policy_a: take(&mut ebpf, MAP_POLICY_A)?,
            policy_b: take(&mut ebpf, MAP_POLICY_B)?,
            counters: take(&mut ebpf, MAP_COUNTERS)?,
            samples: take(&mut ebpf, MAP_SAMPLES)?,
            cfg,
            config,
            ebpf,
            replace_scopes: recorded != 0 && recorded != digest,
        })
    }

    pub fn config(&self) -> &DatapathConfig {
        &self.config
    }

    pub fn set_mode(&mut self, mode: NodeMode) -> Result<(), DatapathError> {
        let value = match mode {
            NodeMode::Legacy => MODE_LEGACY,
            NodeMode::Shadow => MODE_SHADOW,
            NodeMode::Enforce => MODE_ENFORCE,
        };
        self.cfg
            .set(CFG_MODE, value, 0)
            .map_err(map_err(MAP_CONFIG, format!("setting the mode to {mode}")))
    }

    pub fn mode(&self) -> Result<NodeMode, DatapathError> {
        let value = self
            .cfg
            .get(&CFG_MODE, 0)
            .map_err(map_err(MAP_CONFIG, "reading the mode"))?;
        Ok(match value {
            MODE_SHADOW => NodeMode::Shadow,
            MODE_ENFORCE => NodeMode::Enforce,
            _ => NodeMode::Legacy,
        })
    }

    /// Makes the maps hold exactly `plan`. Each target's cgroup gets its
    /// workload's endpoint; a local workload with no target gets no identity
    /// yet and is reported in [`ApplyStats::without_cgroup`]. Targets with
    /// an attach directory get the programs on it, and scope links of
    /// cgroups no longer targeted are released.
    pub fn apply(
        &mut self,
        plan: &DatapathPlan,
        targets: &[CgroupTarget],
    ) -> Result<ApplyStats, DatapathError> {
        let mut stats = ApplyStats::default();
        if plan.policy.len() > self.config.policy_max as usize {
            return Err(DatapathError::Capacity {
                map: MAP_POLICY_A,
                needed: plan.policy.len(),
                max: self.config.policy_max,
            });
        }
        let (v4, v6): (Vec<IpcacheEntry>, Vec<IpcacheEntry>) =
            plan.ipcache.iter().partition(|(c, _)| c.is_ipv4());
        for (map, needed) in [(MAP_IPCACHE_V4, v4.len()), (MAP_IPCACHE_V6, v6.len())] {
            if needed > self.config.ipcache_max as usize {
                return Err(DatapathError::Capacity {
                    map,
                    needed,
                    max: self.config.ipcache_max,
                });
            }
        }

        // Identities: removals first, then additions and changes.
        let mut wanted: BTreeMap<u64, Endpoint> = BTreeMap::new();
        let mut covered = BTreeSet::new();
        for target in targets {
            if let Some(endpoint) = plan.endpoints.get(&target.workload) {
                wanted.insert(target.id, *endpoint);
                covered.insert(target.workload);
            }
        }
        stats.without_cgroup = plan
            .endpoints
            .keys()
            .filter(|w| !covered.contains(*w))
            .copied()
            .collect();
        let current: Vec<u64> = self.identity.keys().filter_map(Result::ok).collect();
        for id in current {
            if !wanted.contains_key(&id) {
                self.identity.remove(&id).map_err(map_err(
                    MAP_CGROUP_IDENTITY,
                    format!("removing cgroup {id}"),
                ))?;
                stats.endpoints_removed += 1;
            }
        }
        for (id, endpoint) in &wanted {
            if self.identity.get(id, 0).ok().as_ref() != Some(endpoint) {
                self.identity
                    .insert(id, endpoint, 0)
                    .map_err(map_err(MAP_CGROUP_IDENTITY, format!("setting cgroup {id}")))?;
                stats.endpoints_set += 1;
            }
        }

        // Addresses, the same way.
        let want_v4: BTreeMap<(u32, Ipv4Key), u32> = v4
            .iter()
            .filter_map(|(c, id)| prefix_bytes_v4(c).map(|b| ((u32::from(c.prefix()), b), **id)))
            .collect();
        let want_v6: BTreeMap<(u32, Ipv6Key), u32> = v6
            .iter()
            .filter_map(|(c, id)| prefix_bytes_v6(c).map(|b| ((u32::from(c.prefix()), b), **id)))
            .collect();
        sync_lpm(&mut self.ipcache_v4, MAP_IPCACHE_V4, &want_v4, &mut stats)?;
        sync_lpm(&mut self.ipcache_v6, MAP_IPCACHE_V6, &want_v6, &mut stats)?;

        // Policy: fill the inactive copy, switch, empty the old one.
        let active = self.cfg.get(&CFG_ACTIVE_POLICY, 0).unwrap_or(0);
        let next = 1 - active.min(1);
        {
            let (target, name) = if next == 0 {
                (&mut self.policy_a, MAP_POLICY_A)
            } else {
                (&mut self.policy_b, MAP_POLICY_B)
            };
            clear_policy(target, name)?;
            for (key, value) in &plan.policy {
                target
                    .insert(key, value, 0)
                    .map_err(map_err(name, "inserting an entry"))?;
            }
        }
        self.cfg
            .set(CFG_ACTIVE_POLICY, next, 0)
            .map_err(map_err(MAP_CONFIG, "switching the active policy"))?;
        let (old, name) = if next == 0 {
            (&mut self.policy_b, MAP_POLICY_B)
        } else {
            (&mut self.policy_a, MAP_POLICY_A)
        };
        clear_policy(old, name)?;
        stats.policy_entries = plan.policy.len();

        // Scopes last: every attached scope already has its identity.
        let scopes: BTreeMap<u64, &Path> = targets
            .iter()
            .filter(|t| wanted.contains_key(&t.id))
            .filter_map(|t| t.attach.as_deref().map(|dir| (t.id, dir)))
            .collect();
        self.sync_scopes(&scopes, &mut stats)?;
        Ok(stats)
    }

    fn sync_scopes(
        &mut self,
        scopes: &BTreeMap<u64, &Path>,
        stats: &mut ApplyStats,
    ) -> Result<(), DatapathError> {
        let pinned = pinned_scopes(&self.config.pin_dir);
        for (id, pins) in &pinned {
            if scopes.contains_key(id) {
                continue;
            }
            for pin in pins {
                // Unlinking the pin drops the last reference; the cgroup is
                // usually gone already, which detached the program.
                if let Err(e) = std::fs::remove_file(pin) {
                    warn!(pin = %pin.display(), error = %e, "could not release a network policy scope link");
                }
            }
            stats.scopes_released += 1;
        }
        for (id, dir) in scopes {
            let complete = pinned
                .get(id)
                .is_some_and(|pins| pins.len() == PROGRAMS.len());
            if complete && !self.replace_scopes {
                continue;
            }
            // A cgroup id is its inode: a directory that is gone, or now
            // has another inode, is a container that stopped meanwhile.
            match cgroup_id(dir) {
                Ok(current) if current == *id => {}
                _ => {
                    debug!(cgroup = %dir.display(), id, "container cgroup changed before attaching; next pass");
                    continue;
                }
            }
            let prefix = format!("{SCOPE_PIN_PREFIX}{id}_");
            attach_pinned(&mut self.ebpf, dir, &self.config.pin_dir, &prefix)?;
            stats.scopes_attached += 1;
        }
        self.replace_scopes = false;
        Ok(())
    }

    pub fn counters(&self) -> Result<Counters, DatapathError> {
        let sum = |index: u32| -> Result<u64, DatapathError> {
            let values = self
                .counters
                .get(&index, 0)
                .map_err(map_err(MAP_COUNTERS, format!("reading slot {index}")))?;
            Ok(values.iter().sum())
        };
        Ok(Counters {
            egress_allow: sum(C_EGRESS_ALLOW)?,
            egress_deny: sum(C_EGRESS_DENY)?,
            ingress_allow: sum(C_INGRESS_ALLOW)?,
            ingress_deny: sum(C_INGRESS_DENY)?,
            would_block: sum(C_WOULD_BLOCK)?,
            unknown_cgroup: sum(C_UNKNOWN_CGROUP)?,
            samples_dropped: sum(C_SAMPLES_DROPPED)?,
            samples_rate_limited: sum(C_SAMPLES_RATE_LIMITED)?,
        })
    }

    /// Up to `max` blocked-flow samples, oldest first.
    pub fn drain_samples(&mut self, max: usize) -> Vec<Sample> {
        let mut out = Vec::new();
        while out.len() < max {
            let Some(item) = self.samples.next() else {
                break;
            };
            if item.len() >= std::mem::size_of::<Sample>() {
                // SAFETY: the program writes whole `Sample` values; the
                // length was checked and the read tolerates any alignment.
                let sample = unsafe { std::ptr::read_unaligned(item.as_ptr() as *const Sample) };
                out.push(sample);
            }
        }
        out
    }

    /// Detaches every program and removes every pin under `pin_dir`: the
    /// one-step rollback to `legacy` (§6, item 9).
    pub fn remove(pin_dir: &Path) -> Result<(), DatapathError> {
        for program in PROGRAMS {
            let path = pin_dir.join(format!("link_{program}"));
            if path.exists() {
                match PinnedLink::from_pin(&path) {
                    Ok(link) => {
                        if let Err(e) = link.unpin() {
                            return Err(DatapathError::Program {
                                program,
                                operation: "unpinning its link".into(),
                                reason: e.to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "could not open a pinned network policy link; removing the pin")
                    }
                }
                let _ = std::fs::remove_file(&path);
            }
        }
        // Scope links and maps go with their pins.
        if pin_dir.exists() {
            std::fs::remove_dir_all(pin_dir).map_err(|e| DatapathError::Io {
                operation: "removing",
                path: pin_dir.display().to_string(),
                reason: e.to_string(),
            })?;
        }
        info!(pin_dir = %pin_dir.display(), "detached the network policy programs and removed their pins");
        Ok(())
    }
}

impl CgroupDatapath for EbpfDatapath {
    fn set_mode(&mut self, mode: NodeMode) -> Result<(), DatapathError> {
        EbpfDatapath::set_mode(self, mode)
    }

    fn apply(
        &mut self,
        plan: &DatapathPlan,
        targets: &[CgroupTarget],
    ) -> Result<ApplyStats, DatapathError> {
        EbpfDatapath::apply(self, plan, targets)
    }

    fn counters(&self) -> Result<Counters, DatapathError> {
        EbpfDatapath::counters(self)
    }

    fn drain_samples(&mut self, max: usize) -> Vec<Sample> {
        EbpfDatapath::drain_samples(self, max)
    }
}

/// Scope link pins by cgroup id.
fn pinned_scopes(pin_dir: &Path) -> BTreeMap<u64, Vec<PathBuf>> {
    let mut out: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(pin_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(SCOPE_PIN_PREFIX)) else {
            continue;
        };
        let Some(id) = rest.split('_').next().and_then(|id| id.parse::<u64>().ok()) else {
            continue;
        };
        out.entry(id).or_default().push(entry.path());
    }
    out
}

fn sync_lpm<K: aya::Pod + Ord>(
    map: &mut LpmTrie<MapData, K, u32>,
    name: &'static str,
    wanted: &BTreeMap<(u32, K), u32>,
    stats: &mut ApplyStats,
) -> Result<(), DatapathError> {
    let current: Vec<Key<K>> = map.keys().filter_map(Result::ok).collect();
    for key in current {
        let id = (key.prefix_len(), key.data());
        if !wanted.contains_key(&id) {
            map.remove(&key)
                .map_err(map_err(name, "removing an address"))?;
            stats.addresses_removed += 1;
        }
    }
    for ((len, data), identity) in wanted {
        let key = Key::new(*len, *data);
        if map.get(&key, 0).ok() != Some(*identity) {
            map.insert(&key, identity, 0)
                .map_err(map_err(name, "setting an address"))?;
            stats.addresses_set += 1;
        }
    }
    Ok(())
}

fn clear_policy(
    map: &mut HashMap<MapData, PolicyKey, PolicyValue>,
    name: &'static str,
) -> Result<(), DatapathError> {
    let keys: Vec<PolicyKey> = map.keys().filter_map(Result::ok).collect();
    for key in keys {
        map.remove(&key)
            .map_err(map_err(name, "clearing an entry"))?;
    }
    Ok(())
}

fn load_program(ebpf: &mut Ebpf, program: &'static str) -> Result<(), DatapathError> {
    let prog = ebpf.program_mut(program).ok_or_else(|| missing(program))?;
    if program == PROG_INGRESS {
        let prog: &mut CgroupSkb = prog
            .try_into()
            .map_err(|e: aya::programs::ProgramError| prog_err(program, "reading its type", e))?;
        prog.load().map_err(|e| prog_err(program, "verifying", e))
    } else {
        let prog: &mut CgroupSockAddr = prog
            .try_into()
            .map_err(|e: aya::programs::ProgramError| prog_err(program, "reading its type", e))?;
        prog.load().map_err(|e| prog_err(program, "verifying", e))
    }
}

fn link_err(program: &'static str) -> impl Fn(aya::programs::links::LinkError) -> DatapathError {
    move |e| DatapathError::Program {
        program,
        operation: "converting its link".into(),
        reason: e.to_string(),
    }
}

/// Attaches every (loaded) program to `cgroup` and pins the links as
/// `<prefix><program>` in `pin_dir`. A link already pinned there, from an
/// earlier object, is released only after the new one is in.
fn attach_pinned(
    ebpf: &mut Ebpf,
    cgroup: &Path,
    pin_dir: &Path,
    prefix: &str,
) -> Result<(), DatapathError> {
    let dir = std::fs::File::open(cgroup).map_err(|e| DatapathError::Io {
        operation: "opening the cgroup",
        path: cgroup.display().to_string(),
        reason: e.to_string(),
    })?;
    for program in PROGRAMS {
        let pin = pin_dir.join(format!("{prefix}{program}"));
        let previous = if pin.exists() {
            PinnedLink::from_pin(&pin).ok()
        } else {
            None
        };
        let link: FdLink = if program == PROG_INGRESS {
            let prog: &mut CgroupSkb = ebpf
                .program_mut(program)
                .ok_or_else(|| missing(program))?
                .try_into()
                .map_err(|e: aya::programs::ProgramError| {
                    prog_err(program, "reading its type", e)
                })?;
            let id = prog
                .attach(&dir, CgroupSkbAttachType::Ingress, CgroupAttachMode::Single)
                .map_err(|e| attach_err(program, cgroup, e))?;
            prog.take_link(id)
                .map_err(|e| prog_err(program, "taking its link", e))?
                .try_into()
                .map_err(link_err(program))?
        } else {
            let prog: &mut CgroupSockAddr = ebpf
                .program_mut(program)
                .ok_or_else(|| missing(program))?
                .try_into()
                .map_err(|e: aya::programs::ProgramError| {
                    prog_err(program, "reading its type", e)
                })?;
            let id = prog
                .attach(&dir, CgroupAttachMode::Single)
                .map_err(|e| attach_err(program, cgroup, e))?;
            prog.take_link(id)
                .map_err(|e| prog_err(program, "taking its link", e))?
                .try_into()
                .map_err(link_err(program))?
        };
        if let Some(old) = previous {
            // The new link is live; drop the old pin. Its program detaches
            // when the last descriptor closes, at the end of this scope.
            if let Err(e) = old.unpin() {
                warn!(program, error = %e, "could not unpin the previous network policy link");
            }
        }
        link.pin(&pin).map_err(|e| DatapathError::Program {
            program,
            operation: format!("pinning its link at {}", pin.display()),
            reason: e.to_string(),
        })?;
    }
    Ok(())
}

fn missing(program: &'static str) -> DatapathError {
    DatapathError::Program {
        program,
        operation: "finding it in the object".into(),
        reason: "missing".into(),
    }
}

fn attach_err(
    program: &'static str,
    cgroup: &Path,
    e: aya::programs::ProgramError,
) -> DatapathError {
    DatapathError::Program {
        program,
        operation: format!("attaching to {}", cgroup.display()),
        reason: e.to_string(),
    }
}

fn prog_err(
    program: &'static str,
    operation: &'static str,
    e: aya::programs::ProgramError,
) -> DatapathError {
    DatapathError::Program {
        program,
        operation: operation.into(),
        reason: e.to_string(),
    }
}

/// These load the real programs and drive them with real sockets, so they
/// need Linux, root, BTF, `python3`, `ip` and a build with the `ebpf`
/// feature. Anywhere else they print why and return.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use crate::ebpf::compile::{compile_datapath, identity_of, DatapathInput};
    use crate::ebpf::PROGRAMS;
    use crate::enforcer::SystemPeer;
    use crate::identity::IdentityMap;
    use crate::rule::{
        Action, Direction, L4Protocol, Origin, Peer, PortRange, Profile, Rule, WorkloadPolicy,
    };
    use temps_workload::WorkloadKind;
    use temps_workload::WorkloadRef;

    const CONNECT: &str = r#"
import socket, sys
kind = socket.SOCK_DGRAM if sys.argv[5] == "udp" else socket.SOCK_STREAM
s = socket.socket(socket.AF_INET, kind)
s.settimeout(float(sys.argv[4]))
try:
    s.bind((sys.argv[1], 0))
    if kind == socket.SOCK_DGRAM:
        s.sendto(b"x", (sys.argv[2], int(sys.argv[3])))
    else:
        s.connect((sys.argv[2], int(sys.argv[3])))
    print("ok")
except PermissionError:
    print("eperm")
except (TimeoutError, socket.timeout):
    print("timeout")
except OSError as e:
    print("err", e.errno)
"#;

    const LISTEN: &str = r#"
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((sys.argv[1], int(sys.argv[2])))
s.listen(64)
print("ready", flush=True)
while True:
    c, _ = s.accept()
    c.close()
"#;

    /// A throwaway cgroup tree, test addresses on `lo`, its own pin
    /// directory, and the helper processes; all removed on drop.
    struct Lab {
        root: PathBuf,
        pin: PathBuf,
        net: u8,
        children: Vec<Child>,
    }

    fn skip(reason: &str) -> Option<Lab> {
        println!("skipping the eBPF datapath test: {reason}");
        None
    }

    impl Lab {
        fn new(net: u8) -> Option<Self> {
            if PROGRAMS.is_none() {
                return skip("built without the `ebpf` feature");
            }
            // SAFETY: plain syscall without arguments.
            if unsafe { libc::geteuid() } != 0 {
                return skip("not running as root");
            }
            if Command::new("python3").arg("--version").output().is_err() {
                return skip("python3 is not installed");
            }
            let root = PathBuf::from(format!(
                "/sys/fs/cgroup/temps-netpol-test-{}-{net}",
                std::process::id()
            ));
            let pin = PathBuf::from(format!(
                "/sys/fs/bpf/temps-netpol-test-{}-{net}",
                std::process::id()
            ));
            if std::fs::create_dir_all(&root).is_err() {
                return skip("cannot create a cgroup under /sys/fs/cgroup");
            }
            let lab = Lab {
                root,
                pin,
                net,
                children: Vec::new(),
            };
            for name in ["a", "b", "c", "d"] {
                std::fs::create_dir_all(lab.root.join(name)).unwrap();
            }
            for host in 1..=4 {
                let status = Command::new("ip")
                    .args([
                        "addr",
                        "add",
                        &format!("{}/32", lab.addr(host)),
                        "dev",
                        "lo",
                    ])
                    .status();
                if !matches!(status, Ok(s) if s.success()) {
                    return skip("cannot add a test address to lo with `ip`");
                }
            }
            Some(lab)
        }

        fn addr(&self, host: u8) -> String {
            format!("10.250.{}.{host}", self.net)
        }

        fn config(&self) -> DatapathConfig {
            DatapathConfig {
                pin_dir: self.pin.clone(),
                attach_cgroup: self.root.clone(),
                max_level: 8,
                ipcache_max: 1024,
                policy_max: 4096,
                sample_interval_us: 1,
            }
        }

        fn cgroups(&self) -> Vec<CgroupTarget> {
            [(sandbox(), "a"), (service(), "b"), (deployment(), "c")]
                .into_iter()
                .map(|(workload, dir)| CgroupTarget {
                    id: cgroup_id(&self.root.join(dir)).unwrap(),
                    workload,
                    attach: None,
                })
                .collect()
        }

        /// A cgroup outside the attach point, like a container's Docker
        /// scope, created on first use.
        fn scope(&self) -> PathBuf {
            let dir = PathBuf::from(format!("{}-scope", self.root.display()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn command_in(&self, cgroup: &str, script: &str, args: &[&str]) -> Command {
            let dir = if cgroup == "scope" {
                self.scope()
            } else {
                self.root.join(cgroup)
            };
            let procs = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.join("cgroup.procs"))
                .unwrap();
            let fd = procs.as_raw_fd();
            let mut command = Command::new("python3");
            command.arg("-c").arg(script).args(args);
            // SAFETY: only an async-signal-safe write to an fd opened before
            // the fork, moving the child into the cgroup before it runs
            // anything.
            unsafe {
                command.pre_exec(move || {
                    let _keep = &procs;
                    if libc::write(fd, b"0".as_ptr().cast(), 1) == 1 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            command
        }

        /// `ok`, `eperm`, `timeout` or `err <errno>`.
        fn connect(&self, from: &str, src: u8, dst: u8, port: u16, proto: &str) -> String {
            let out = self
                .command_in(
                    from,
                    CONNECT,
                    &[
                        &self.addr(src),
                        &self.addr(dst),
                        &port.to_string(),
                        "1.5",
                        proto,
                    ],
                )
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn listen(&mut self, cgroup: &str, host: u8, port: u16) {
            let mut child = self
                .command_in(cgroup, LISTEN, &[&self.addr(host), &port.to_string()])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(
                line.trim(),
                "ready",
                "listener on {}:{port} did not start",
                self.addr(host)
            );
            self.children.push(child);
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            for child in &mut self.children {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = EbpfDatapath::remove(&self.pin);
            for host in 1..=4 {
                let _ = Command::new("ip")
                    .args([
                        "addr",
                        "del",
                        &format!("{}/32", self.addr(host)),
                        "dev",
                        "lo",
                    ])
                    .status();
            }
            for name in ["a", "b", "c", "d"] {
                let _ = std::fs::remove_dir(self.root.join(name));
            }
            let _ = std::fs::remove_dir(&self.root);
            let _ = std::fs::remove_dir(format!("{}-scope", self.root.display()));
        }
    }

    fn sandbox() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, 1)
    }
    fn service() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, 2)
    }
    fn deployment() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Deployment, 3)
    }

    fn rule(id: i64, direction: Direction, peer: WorkloadRef, port: u16) -> Rule {
        Rule {
            id,
            direction,
            peer: Peer::Workload(peer),
            ports: Some(PortRange::single(port)),
            protocol: L4Protocol::Tcp,
            action: Action::Allow,
            origin: Origin::Link,
        }
    }

    /// The sandbox may reach the service on 28080 and nothing else; the
    /// service only accepts the sandbox on 28080; the deployment is open.
    fn plan(lab: &Lab, with_service_address: bool) -> DatapathPlan {
        let policies = vec![
            WorkloadPolicy {
                subject: sandbox(),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![rule(1, Direction::Egress, service(), 28080)],
            },
            WorkloadPolicy {
                subject: service(),
                egress: Profile::Open,
                ingress: Profile::Restricted,
                rules: vec![rule(2, Direction::Ingress, sandbox(), 28080)],
            },
        ];
        let mut identities = IdentityMap::new();
        identities.insert(sandbox(), lab.addr(1).parse().unwrap());
        if with_service_address {
            identities.insert(service(), lab.addr(2).parse().unwrap());
        }
        identities.insert(deployment(), lab.addr(3).parse().unwrap());
        let local: BTreeSet<WorkloadRef> = [sandbox(), service(), deployment()].into();
        let none = BTreeSet::new();
        let system: Vec<SystemPeer> = Vec::new();
        compile_datapath(DatapathInput {
            policies: &policies,
            identities: &identities,
            local: &local,
            internet: &none,
            trial: &none,
            system_egress: &system,
            system_ingress: &[],
        })
        .unwrap()
    }

    #[test]
    fn enforces_links_in_both_directions_and_fails_closed() {
        let Some(mut lab) = Lab::new(11) else { return };
        let object = PROGRAMS.unwrap();
        let mut dp = EbpfDatapath::open(lab.config(), object).unwrap();
        let stats = dp.apply(&plan(&lab, true), &lab.cgroups()).unwrap();
        assert!(stats.without_cgroup.is_empty());
        dp.set_mode(NodeMode::Enforce).unwrap();
        lab.listen("b", 2, 28080);
        lab.listen("c", 3, 28080);
        lab.listen("a", 1, 28081);

        assert_eq!(lab.connect("a", 1, 2, 28080, "tcp"), "ok", "linked peer");
        assert_eq!(
            lab.connect("a", 1, 3, 28080, "tcp"),
            "eperm",
            "unlinked peer"
        );
        assert_eq!(
            lab.connect("a", 1, 2, 29090, "tcp"),
            "eperm",
            "linked peer, other port"
        );
        assert_eq!(
            lab.connect("c", 3, 2, 28080, "tcp"),
            "timeout",
            "restricted ingress drops an unlinked source"
        );
        assert_eq!(lab.connect("c", 3, 1, 28081, "tcp"), "ok", "open ingress");
        assert_eq!(
            lab.connect("d", 4, 2, 28080, "tcp"),
            "eperm",
            "a slice without an identity"
        );
        assert_eq!(
            lab.connect("a", 1, 3, 53, "udp"),
            "eperm",
            "unconnected UDP is judged too"
        );
        assert_eq!(
            lab.connect("c", 3, 1, 53, "udp"),
            "ok",
            "open egress over UDP"
        );

        let counters = dp.counters().unwrap();
        assert!(counters.egress_deny >= 3, "{counters:?}");
        assert!(counters.ingress_deny >= 1, "{counters:?}");
        assert!(counters.unknown_cgroup >= 1, "{counters:?}");
        let samples = dp.drain_samples(64);
        let a = identity_of(sandbox()).unwrap();
        assert!(
            samples
                .iter()
                .any(|s| s.subject == a && s.dir == DIR_EGRESS && s.would_block == 0),
            "{samples:?}"
        );
    }

    #[test]
    fn shadow_and_legacy_never_block() {
        let Some(mut lab) = Lab::new(12) else { return };
        let mut dp = EbpfDatapath::open(lab.config(), PROGRAMS.unwrap()).unwrap();
        dp.apply(&plan(&lab, true), &lab.cgroups()).unwrap();
        lab.listen("c", 3, 28080);
        lab.listen("b", 2, 28080);

        dp.set_mode(NodeMode::Shadow).unwrap();
        assert_eq!(lab.connect("a", 1, 3, 28080, "tcp"), "ok");
        assert_eq!(lab.connect("d", 4, 2, 28080, "tcp"), "ok");
        let counters = dp.counters().unwrap();
        assert!(counters.would_block >= 1, "{counters:?}");
        assert_eq!(counters.egress_deny, 0, "{counters:?}");
        assert!(dp.drain_samples(64).iter().any(|s| s.would_block == 1));

        dp.set_mode(NodeMode::Legacy).unwrap();
        assert_eq!(lab.connect("a", 1, 3, 28080, "tcp"), "ok");
        assert_eq!(lab.connect("c", 3, 2, 28080, "tcp"), "ok");
    }

    #[test]
    fn enforcement_outlives_the_loader_and_reopening_does_not_attach_twice() {
        let Some(mut lab) = Lab::new(13) else { return };
        {
            let mut dp = EbpfDatapath::open(lab.config(), PROGRAMS.unwrap()).unwrap();
            dp.apply(&plan(&lab, true), &lab.cgroups()).unwrap();
            dp.set_mode(NodeMode::Enforce).unwrap();
        }
        lab.listen("b", 2, 28080);
        lab.listen("c", 3, 28080);
        // No datapath object exists now: only the pins enforce.
        assert_eq!(lab.connect("a", 1, 3, 28080, "tcp"), "eperm");
        assert_eq!(lab.connect("a", 1, 2, 28080, "tcp"), "ok");

        let attached_before = attached_links(&lab.root);
        let mut dp = EbpfDatapath::open(lab.config(), PROGRAMS.unwrap()).unwrap();
        assert_eq!(
            attached_links(&lab.root),
            attached_before,
            "reopening attached the programs again"
        );
        assert_eq!(dp.mode().unwrap(), NodeMode::Enforce);
        dp.apply(&plan(&lab, true), &lab.cgroups()).unwrap();
        assert_eq!(lab.connect("a", 1, 3, 28080, "tcp"), "eperm");
        assert_eq!(lab.connect("a", 1, 2, 28080, "tcp"), "ok");
    }

    #[test]
    fn a_removed_address_stops_carrying_its_identity() {
        let Some(mut lab) = Lab::new(14) else { return };
        let mut dp = EbpfDatapath::open(lab.config(), PROGRAMS.unwrap()).unwrap();
        dp.apply(&plan(&lab, true), &lab.cgroups()).unwrap();
        dp.set_mode(NodeMode::Enforce).unwrap();
        lab.listen("b", 2, 28080);
        assert_eq!(lab.connect("a", 1, 2, 28080, "tcp"), "ok");
        // The service's address is released (its container is gone). The
        // address now falls in 10.0.0.0/8, which no rule allows.
        let stats = dp.apply(&plan(&lab, false), &lab.cgroups()).unwrap();
        assert!(stats.addresses_removed >= 1, "{stats:?}");
        assert_eq!(lab.connect("a", 1, 2, 28080, "tcp"), "eperm");
    }

    #[test]
    fn loading_needs_sys_admin_or_bpf_with_net_admin() {
        assert!(may_load_programs(1 << CAP_SYS_ADMIN));
        assert!(may_load_programs((1 << CAP_BPF) | (1 << CAP_NET_ADMIN)));
        assert!(!may_load_programs(1 << CAP_BPF));
        assert!(!may_load_programs(1 << 10));
        assert!(!may_load_programs(0));
    }

    #[test]
    fn a_container_outside_the_slice_is_enforced_through_its_scope() {
        let Some(mut lab) = Lab::new(15) else { return };
        let mut dp = EbpfDatapath::open(lab.config(), PROGRAMS.unwrap()).unwrap();
        dp.set_mode(NodeMode::Enforce).unwrap();
        lab.listen("c", 3, 28080);
        lab.listen("b", 2, 28080);
        let mut targets: Vec<CgroupTarget> = lab
            .cgroups()
            .into_iter()
            .filter(|t| t.workload != sandbox())
            .collect();
        dp.apply(&plan(&lab, true), &targets).unwrap();
        // Before it is a target, nothing is attached to the scope.
        assert_eq!(lab.connect("scope", 1, 3, 28080, "tcp"), "ok");

        let scope = lab.scope();
        targets.push(CgroupTarget {
            id: cgroup_id(&scope).unwrap(),
            workload: sandbox(),
            attach: Some(scope.clone()),
        });
        let stats = dp.apply(&plan(&lab, true), &targets).unwrap();
        assert_eq!(stats.scopes_attached, 1, "{stats:?}");
        assert_eq!(lab.connect("scope", 1, 3, 28080, "tcp"), "eperm");
        assert_eq!(lab.connect("scope", 1, 2, 28080, "tcp"), "ok");

        // Applying again attaches nothing more.
        let again = dp.apply(&plan(&lab, true), &targets).unwrap();
        assert_eq!(again.scopes_attached, 0, "{again:?}");
        assert_eq!(attached_links(&scope), 5);

        // The container is gone: its scope links are released.
        targets.pop();
        let stats = dp.apply(&plan(&lab, true), &targets).unwrap();
        assert_eq!(stats.scopes_released, 1, "{stats:?}");
        assert_eq!(attached_links(&scope), 0);
        assert_eq!(lab.connect("scope", 1, 3, 28080, "tcp"), "ok");
    }

    /// Links attached to `cgroup`, from `bpftool cgroup show`.
    fn attached_links(cgroup: &Path) -> usize {
        Command::new("bpftool")
            .args(["cgroup", "show"])
            .arg(cgroup)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| {
                        l.contains("connect") || l.contains("sendmsg") || l.contains("ingress")
                    })
                    .count()
            })
            .unwrap_or(0)
    }
}
