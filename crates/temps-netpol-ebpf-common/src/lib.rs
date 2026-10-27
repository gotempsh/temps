// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Map key and value layouts shared by the network policy eBPF programs and
//! the loader (network access plan, §4.15).
//!
//! Everything here is `#[repr(C)]` with explicit padding, so the programs and
//! user space agree on every byte. Addresses are kept in network byte order,
//! ports in host byte order.
#![no_std]

/// Directory under the BPF filesystem where maps and links are pinned.
pub const PIN_DIR: &str = "/sys/fs/bpf/temps/netpol";

// ---------------------------------------------------------------------------
// Map names. A BPF object name holds at most 15 characters.

pub const MAP_CGROUP_IDENTITY: &str = "CGROUP_IDENTITY";
pub const MAP_IPCACHE_V4: &str = "IPCACHE_V4";
pub const MAP_IPCACHE_V6: &str = "IPCACHE_V6";
pub const MAP_POLICY_A: &str = "POLICY_A";
pub const MAP_POLICY_B: &str = "POLICY_B";
pub const MAP_CONFIG: &str = "CONFIG";
pub const MAP_COUNTERS: &str = "COUNTERS";
pub const MAP_SAMPLES: &str = "SAMPLES";
pub const MAP_SAMPLE_GATE: &str = "SAMPLE_GATE";

// ---------------------------------------------------------------------------
// Program names, as they appear in the object.

pub const PROG_CONNECT4: &str = "connect4";
pub const PROG_CONNECT6: &str = "connect6";
pub const PROG_SENDMSG4: &str = "sendmsg4";
pub const PROG_SENDMSG6: &str = "sendmsg6";
pub const PROG_INGRESS: &str = "ingress";

// ---------------------------------------------------------------------------
// `CONFIG` slots (an array of u32).

/// [`MODE_LEGACY`], [`MODE_SHADOW`] or [`MODE_ENFORCE`].
pub const CFG_MODE: u32 = 0;
/// Deepest ancestor level the programs look at for a registered cgroup
/// (0: the programs' default of 8). A workload slice such as
/// `/sys/fs/cgroup/temps.slice/temps-sandbox.slice/temps-sandbox-7.slice` is
/// level 3, a Docker scope such as `system.slice/docker-<id>.scope` level 2.
pub const CFG_LEVEL: u32 = 1;
/// Which policy map is live: 0 for `POLICY_A`, 1 for `POLICY_B`.
pub const CFG_ACTIVE_POLICY: u32 = 2;
/// Low 32 bits of the loaded object's digest, so a restarted agent can tell
/// whether the attached programs are the ones it embeds.
pub const CFG_PROGRAM_DIGEST: u32 = 3;
/// Minimum nanoseconds between two blocked-flow samples, divided by 1,000.
pub const CFG_SAMPLE_INTERVAL_US: u32 = 4;
pub const CONFIG_SLOTS: u32 = 8;

pub const MODE_LEGACY: u32 = 0;
pub const MODE_SHADOW: u32 = 1;
pub const MODE_ENFORCE: u32 = 2;

// ---------------------------------------------------------------------------
// Profiles, directions, verdicts.

pub const PROFILE_OPEN: u8 = 0;
pub const PROFILE_RESTRICTED: u8 = 1;
pub const PROFILE_NONE: u8 = 2;

pub const DIR_EGRESS: u8 = 0;
pub const DIR_INGRESS: u8 = 1;

pub const VERDICT_ALLOW: u8 = 1;
pub const VERDICT_DENY: u8 = 2;

pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

pub const AF_INET: u8 = 2;
pub const AF_INET6: u8 = 10;

// ---------------------------------------------------------------------------
// Identities.
//
// An identity is a u32. The top four bits say what it names:
//
// | top bits | names                                   |
// |----------|-----------------------------------------|
// | 0        | reserved: 0 means "every subject" in a policy key, 1 is the world |
// | 1–5      | a workload: kind code, then its id in the low 28 bits |
// | 0xF      | an address prefix: named by a rule, a system peer, or a built-in range |

/// Policy-key subject for entries that apply to every subject: system allows
/// and the metadata block, checked before the subject's own entries.
pub const SUBJECT_ANY: u32 = 0;
/// An address in no ipcache prefix: the internet.
pub const ID_WORLD: u32 = 1;

pub const ID_CLASS_SHIFT: u32 = 28;
pub const ID_LOCAL_MASK: u32 = (1 << ID_CLASS_SHIFT) - 1;
pub const ID_CLASS_PREFIX: u32 = 0xF;

// ---------------------------------------------------------------------------
// Per-CPU counters (`COUNTERS`, an array of u64).

pub const C_EGRESS_ALLOW: u32 = 0;
pub const C_EGRESS_DENY: u32 = 1;
pub const C_INGRESS_ALLOW: u32 = 2;
pub const C_INGRESS_DENY: u32 = 3;
pub const C_WOULD_BLOCK: u32 = 4;
pub const C_UNKNOWN_CGROUP: u32 = 5;
pub const C_SAMPLES_DROPPED: u32 = 6;
pub const C_SAMPLES_RATE_LIMITED: u32 = 7;
pub const COUNTER_SLOTS: u32 = 8;

// ---------------------------------------------------------------------------
// Values.

/// `CGROUP_IDENTITY`: workload slice cgroup id → this.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub identity: u32,
    pub egress: u8,
    pub ingress: u8,
    /// Restricted egress may reach [`ID_WORLD`].
    pub internet: u8,
    /// Evaluate and count, never block.
    pub trial: u8,
    /// The subject has entries of its own in this direction. An open
    /// direction without any skips every policy lookup.
    pub egress_entries: u8,
    pub ingress_entries: u8,
    pub _pad: [u8; 2],
}

/// `POLICY_A` / `POLICY_B` key.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PolicyKey {
    pub dir: u8,
    pub proto: u8,
    /// Host byte order; 0 matches any port.
    pub port: u16,
    /// A workload identity, or [`SUBJECT_ANY`].
    pub subject: u32,
    pub peer: u32,
}

/// `POLICY_A` / `POLICY_B` value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PolicyValue {
    pub verdict: u8,
    pub _pad: [u8; 3],
    /// The rule that decided, truncated to 32 bits; 0 for built-in entries.
    pub rule: u32,
}

/// `IPCACHE_V4` key data, network byte order.
pub type Ipv4Key = [u8; 4];
/// `IPCACHE_V6` key data, network byte order.
pub type Ipv6Key = [u8; 16];

/// One blocked (or would-be-blocked) flow, written to `SAMPLES`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub ts_ns: u64,
    pub subject: u32,
    pub peer: u32,
    /// The peer's address, network byte order; IPv4 uses the first 4 bytes.
    pub addr: [u8; 16],
    pub port: u16,
    pub proto: u8,
    pub dir: u8,
    /// [`AF_INET`] or [`AF_INET6`].
    pub family: u8,
    /// 1 when the flow was let through by shadow or trial mode.
    pub would_block: u8,
    pub _pad: [u8; 2],
    pub rule: u32,
    pub _pad2: u32,
}

#[cfg(all(feature = "user", target_os = "linux"))]
mod pod {
    // SAFETY: plain `#[repr(C)]` data with explicit padding and no pointers;
    // every bit pattern is a valid value.
    unsafe impl aya::Pod for super::Endpoint {}
    unsafe impl aya::Pod for super::PolicyKey {}
    unsafe impl aya::Pod for super::PolicyValue {}
    unsafe impl aya::Pod for super::Sample {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn layouts_have_no_implicit_padding() {
        assert_eq!(size_of::<Endpoint>(), 12);
        assert_eq!(size_of::<PolicyKey>(), 12);
        assert_eq!(size_of::<PolicyValue>(), 8);
        assert_eq!(size_of::<Sample>(), 48);
    }

    #[test]
    fn map_names_fit_the_kernel_limit() {
        for name in [
            MAP_CGROUP_IDENTITY,
            MAP_IPCACHE_V4,
            MAP_IPCACHE_V6,
            MAP_POLICY_A,
            MAP_POLICY_B,
            MAP_CONFIG,
            MAP_COUNTERS,
            MAP_SAMPLES,
            MAP_SAMPLE_GATE,
        ] {
            assert!(name.len() <= 15, "{name} is longer than 15 characters");
        }
    }
}
