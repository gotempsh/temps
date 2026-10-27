// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network policy programs (network access plan, §4.15).
//!
//! They are attached to `temps.slice`, where the workload driver places
//! containers, and to the Docker scope of each workload container created
//! outside it. Either way they only ever see processes of Temps workloads;
//! sshd, the agent and WireGuard are never below an attach point and can
//! never be filtered (§4.11).
//!
//! - **Identity comes from the cgroup.** The programs walk the caller's
//!   ancestors from the root down and take the first one registered in
//!   `CGROUP_IDENTITY`: a workload's slice, or a container's own scope. A
//!   process below an attach point with no registered ancestor is unknown,
//!   which `enforce` refuses.
//! - **Egress is decided once per connection**, at `connect` (TCP and
//!   connected UDP) or `sendmsg` (unconnected UDP). A refusal returns
//!   `EPERM`, so the application fails at once instead of timing out.
//! - **Ingress** (`cgroup_skb/ingress`) only judges packets that open
//!   something: a TCP SYN without ACK, or a UDP datagram that is not for a
//!   connected socket. Everything else returns before any map lookup.
//! - **Lookup order** (§4.9): entries for every subject (`SUBJECT_ANY`:
//!   system allows, the metadata block), then the subject's own entries
//!   (exact port, then any port; denies already folded in by the compiler),
//!   then the profile's default.
//! - **Shadow and trial** evaluate as usual, count "would block", sample,
//!   and let the flow through.
#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::BPF_F_NO_PREALLOC,
    helpers::{
        bpf_get_current_ancestor_cgroup_id, bpf_ktime_get_ns, bpf_sk_fullsock,
        bpf_skb_ancestor_cgroup_id,
    },
    macros::{cgroup_skb, cgroup_sock_addr, map},
    maps::{lpm_trie::Key, Array, HashMap, LpmTrie, PerCpuArray, RingBuf},
    programs::{SkBuffContext, SockAddrContext},
};
use temps_netpol_ebpf_common::*;

#[map]
static CGROUP_IDENTITY: HashMap<u64, Endpoint> = HashMap::pinned(16384, BPF_F_NO_PREALLOC);
#[map]
static IPCACHE_V4: LpmTrie<Ipv4Key, u32> = LpmTrie::pinned(65536, BPF_F_NO_PREALLOC);
#[map]
static IPCACHE_V6: LpmTrie<Ipv6Key, u32> = LpmTrie::pinned(65536, BPF_F_NO_PREALLOC);
#[map]
static POLICY_A: HashMap<PolicyKey, PolicyValue> = HashMap::pinned(65536, BPF_F_NO_PREALLOC);
#[map]
static POLICY_B: HashMap<PolicyKey, PolicyValue> = HashMap::pinned(65536, BPF_F_NO_PREALLOC);
#[map]
static CONFIG: Array<u32> = Array::pinned(CONFIG_SLOTS, 0);
#[map]
static COUNTERS: PerCpuArray<u64> = PerCpuArray::pinned(COUNTER_SLOTS, 0);
#[map]
static SAMPLES: RingBuf = RingBuf::pinned(256 * 1024, 0);
#[map]
static SAMPLE_GATE: Array<u64> = Array::pinned(1, 0);

const DEFAULT_SAMPLE_INTERVAL_NS: u64 = 200_000_000;
const TCP_SYN: u8 = 0x02;
const TCP_ACK: u8 = 0x10;
const TCP_ESTABLISHED: u32 = 1;

#[inline(always)]
fn cfg(index: u32) -> u32 {
    CONFIG.get(index).copied().unwrap_or(0)
}

#[inline(always)]
fn count(index: u32) {
    if let Some(slot) = COUNTERS.get_ptr_mut(index) {
        // SAFETY: per-CPU slot, written only by programs on this CPU.
        unsafe { *slot += 1 };
    }
}

/// One connection or datagram being judged.
struct Flow {
    dir: u8,
    proto: u8,
    port: u16,
    peer: u32,
    family: u8,
    addr: [u8; 16],
}

#[inline(always)]
fn peer_v4(addr: [u8; 4]) -> u32 {
    match IPCACHE_V4.get(&Key::new(32, addr)) {
        Some(id) => *id,
        None => ID_WORLD,
    }
}

#[inline(always)]
fn peer_v6(addr: [u8; 16]) -> u32 {
    match IPCACHE_V6.get(&Key::new(128, addr)) {
        Some(id) => *id,
        None => ID_WORLD,
    }
}

#[inline(always)]
fn policy_get(active: u32, key: &PolicyKey) -> Option<PolicyValue> {
    // SAFETY: map values are plain data; the pointer is only read here.
    let value = if active == 0 {
        unsafe { POLICY_A.get(key) }
    } else {
        unsafe { POLICY_B.get(key) }
    };
    value.copied()
}

#[inline(always)]
fn lookup(active: u32, flow: &Flow, subject: u32) -> Option<PolicyValue> {
    let mut key = PolicyKey {
        dir: flow.dir,
        proto: flow.proto,
        port: flow.port,
        subject,
        peer: flow.peer,
    };
    if let Some(value) = policy_get(active, &key) {
        return Some(value);
    }
    key.port = 0;
    policy_get(active, &key)
}

/// (allowed, deciding rule).
#[inline(always)]
fn decide(ep: &Endpoint, flow: &Flow) -> (bool, u32) {
    let active = cfg(CFG_ACTIVE_POLICY);
    if let Some(value) = lookup(active, flow, SUBJECT_ANY) {
        return (value.verdict == VERDICT_ALLOW, value.rule);
    }
    let (profile, entries) = if flow.dir == DIR_EGRESS {
        (ep.egress, ep.egress_entries)
    } else {
        (ep.ingress, ep.ingress_entries)
    };
    if profile == PROFILE_NONE {
        return (false, 0);
    }
    if entries != 0 {
        if let Some(value) = lookup(active, flow, ep.identity) {
            return (value.verdict == VERDICT_ALLOW, value.rule);
        }
    }
    if profile == PROFILE_OPEN {
        return (true, 0);
    }
    (
        flow.dir == DIR_EGRESS && flow.peer == ID_WORLD && ep.internet != 0,
        0,
    )
}

#[inline(always)]
fn sample(ep: &Endpoint, flow: &Flow, rule: u32, would_block: bool) {
    // SAFETY: helper with no arguments.
    let now = unsafe { bpf_ktime_get_ns() };
    if let Some(gate) = SAMPLE_GATE.get_ptr_mut(0) {
        let interval = match cfg(CFG_SAMPLE_INTERVAL_US) {
            0 => DEFAULT_SAMPLE_INTERVAL_NS,
            us => us as u64 * 1_000,
        };
        // SAFETY: a single u64 slot. Racing CPUs can each take a sample in
        // the same interval; the bound stays a small multiple of the rate.
        unsafe {
            if now < *gate {
                count(C_SAMPLES_RATE_LIMITED);
                return;
            }
            *gate = now + interval;
        }
    }
    match SAMPLES.reserve::<Sample>(0) {
        Some(mut entry) => {
            entry.write(Sample {
                ts_ns: now,
                subject: ep.identity,
                peer: flow.peer,
                addr: flow.addr,
                port: flow.port,
                proto: flow.proto,
                dir: flow.dir,
                family: flow.family,
                would_block: would_block as u8,
                _pad: [0; 2],
                rule,
                _pad2: 0,
            });
            entry.submit(0);
        }
        None => count(C_SAMPLES_DROPPED),
    }
}

/// 1 lets the operation through, 0 refuses it.
#[inline(always)]
fn judge(mode: u32, ep: &Endpoint, flow: &Flow) -> i32 {
    let (allowed, rule) = decide(ep, flow);
    let (allow_counter, deny_counter) = if flow.dir == DIR_EGRESS {
        (C_EGRESS_ALLOW, C_EGRESS_DENY)
    } else {
        (C_INGRESS_ALLOW, C_INGRESS_DENY)
    };
    if allowed {
        count(allow_counter);
        return 1;
    }
    if mode == MODE_SHADOW || ep.trial != 0 {
        count(C_WOULD_BLOCK);
        sample(ep, flow, rule, true);
        return 1;
    }
    count(deny_counter);
    sample(ep, flow, rule, false);
    0
}

/// Deepest ancestor level looked at when `CONFIG[CFG_LEVEL]` is unset.
/// `temps.slice/temps-<kind>.slice/temps-<kind>-<id>.slice` is level 3 and
/// `system.slice/docker-<id>.scope` level 2.
const DEFAULT_MAX_LEVEL: u32 = 8;
/// Hard bound on the walk, so the verifier sees a bounded loop.
const MAX_LEVEL: u32 = 16;

#[inline(always)]
fn max_level() -> u32 {
    match cfg(CFG_LEVEL) {
        0 => DEFAULT_MAX_LEVEL,
        level if level > MAX_LEVEL => MAX_LEVEL,
        level => level,
    }
}

/// The endpoint of the shallowest registered ancestor. `ancestor(level)`
/// returns 0 past the cgroup's depth, which ends the walk.
#[inline(always)]
fn walk(mode: u32, ancestor: impl Fn(i32) -> u64) -> Result<Endpoint, i32> {
    let last = max_level();
    let mut level = 1;
    while level <= MAX_LEVEL {
        if level > last {
            break;
        }
        let cgroup = ancestor(level as i32);
        if cgroup == 0 {
            break;
        }
        // SAFETY: map value is plain data, copied out.
        if let Some(ep) = unsafe { CGROUP_IDENTITY.get(&cgroup) } {
            return Ok(*ep);
        }
        level += 1;
    }
    count(C_UNKNOWN_CGROUP);
    Err(if mode == MODE_ENFORCE { 0 } else { 1 })
}

#[inline(always)]
fn current_endpoint(mode: u32) -> Result<Endpoint, i32> {
    // SAFETY: helper reading the current task's cgroup.
    walk(mode, |level| unsafe {
        bpf_get_current_ancestor_cgroup_id(level)
    })
}

#[inline(always)]
fn v4_bytes(addr: u32) -> [u8; 4] {
    // The u32 holds the address in network order; its memory bytes are the
    // address bytes.
    addr.to_ne_bytes()
}

#[inline(always)]
fn widen_v4(addr: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0] = addr[0];
    out[1] = addr[1];
    out[2] = addr[2];
    out[3] = addr[3];
    out
}

#[inline(always)]
fn egress_v4(ctx: &SockAddrContext) -> i32 {
    let mode = cfg(CFG_MODE);
    if mode == MODE_LEGACY {
        return 1;
    }
    let ep = match current_endpoint(mode) {
        Ok(ep) => ep,
        Err(verdict) => return verdict,
    };
    // SAFETY: the context pointer is valid for the program's lifetime.
    let sa = unsafe { &*ctx.sock_addr };
    let addr = v4_bytes(sa.user_ip4);
    if addr[0] == 127 {
        return 1;
    }
    let flow = Flow {
        dir: DIR_EGRESS,
        proto: sa.protocol as u8,
        port: u16::from_be(sa.user_port as u16),
        peer: peer_v4(addr),
        family: AF_INET,
        addr: widen_v4(addr),
    };
    judge(mode, &ep, &flow)
}

#[inline(always)]
fn egress_v6(ctx: &SockAddrContext) -> i32 {
    let mode = cfg(CFG_MODE);
    if mode == MODE_LEGACY {
        return 1;
    }
    let ep = match current_endpoint(mode) {
        Ok(ep) => ep,
        Err(verdict) => return verdict,
    };
    // SAFETY: the context pointer is valid for the program's lifetime.
    let sa = unsafe { &*ctx.sock_addr };
    let words = [
        sa.user_ip6[0],
        sa.user_ip6[1],
        sa.user_ip6[2],
        sa.user_ip6[3],
    ];
    let proto = sa.protocol as u8;
    let port = u16::from_be(sa.user_port as u16);
    // A dual-stack socket reaches IPv4 peers through `::ffff:a.b.c.d`; judge
    // those exactly like a `connect4`.
    if words[0] == 0 && words[1] == 0 && words[2] == u32::to_be(0xffff) {
        let addr = v4_bytes(words[3]);
        if addr[0] == 127 {
            return 1;
        }
        let flow = Flow {
            dir: DIR_EGRESS,
            proto,
            port,
            peer: peer_v4(addr),
            family: AF_INET,
            addr: widen_v4(addr),
        };
        return judge(mode, &ep, &flow);
    }
    if words[0] == 0 && words[1] == 0 && words[2] == 0 && words[3] == u32::to_be(1) {
        return 1;
    }
    let mut addr = [0u8; 16];
    let mut i = 0;
    while i < 4 {
        let b = words[i].to_ne_bytes();
        addr[i * 4] = b[0];
        addr[i * 4 + 1] = b[1];
        addr[i * 4 + 2] = b[2];
        addr[i * 4 + 3] = b[3];
        i += 1;
    }
    let flow = Flow {
        dir: DIR_EGRESS,
        proto,
        port,
        peer: peer_v6(addr),
        family: AF_INET6,
        addr,
    };
    judge(mode, &ep, &flow)
}

#[cgroup_sock_addr(connect4)]
pub fn connect4(ctx: SockAddrContext) -> i32 {
    egress_v4(&ctx)
}

#[cgroup_sock_addr(sendmsg4)]
pub fn sendmsg4(ctx: SockAddrContext) -> i32 {
    egress_v4(&ctx)
}

#[cgroup_sock_addr(connect6)]
pub fn connect6(ctx: SockAddrContext) -> i32 {
    egress_v6(&ctx)
}

#[cgroup_sock_addr(sendmsg6)]
pub fn sendmsg6(ctx: SockAddrContext) -> i32 {
    egress_v6(&ctx)
}

/// Whether the packet belongs to a connected UDP socket of the workload: a
/// reply to something its own `connect` already judged.
#[inline(always)]
fn for_connected_socket(ctx: &SkBuffContext) -> bool {
    // SAFETY: reading `sk` from the skb context and asking the kernel for
    // the full socket; both are checked for null before use.
    unsafe {
        let sk = (*ctx.skb.skb).__bindgen_anon_2.sk;
        if sk.is_null() {
            return false;
        }
        let full = bpf_sk_fullsock(sk);
        !full.is_null() && (*full).state == TCP_ESTABLISHED
    }
}

#[cgroup_skb]
pub fn ingress(ctx: SkBuffContext) -> i32 {
    let mode = cfg(CFG_MODE);
    if mode == MODE_LEGACY {
        return 1;
    }
    // cgroup_skb data starts at the IP header.
    let first: u8 = match ctx.load(0) {
        Ok(v) => v,
        Err(_) => return 1,
    };
    let (proto, l4, family) = match first >> 4 {
        4 => (
            ctx.load::<u8>(9).unwrap_or(0),
            ((first & 0x0f) as usize) * 4,
            AF_INET,
        ),
        6 => (ctx.load::<u8>(6).unwrap_or(0), 40usize, AF_INET6),
        _ => return 1,
    };
    if proto == IPPROTO_TCP {
        let flags: u8 = ctx.load(l4 + 13).unwrap_or(0);
        if flags & TCP_SYN == 0 || flags & TCP_ACK != 0 {
            // Established traffic, and replies to the workload's own
            // connections.
            return 1;
        }
    } else if proto == IPPROTO_UDP {
        if for_connected_socket(&ctx) {
            return 1;
        }
    } else {
        return 1;
    }
    let skb = ctx.skb.skb;
    // SAFETY: helper reading the cgroup of the socket receiving the skb.
    let ep = match walk(mode, |level| unsafe {
        bpf_skb_ancestor_cgroup_id(skb as *mut _, level)
    }) {
        Ok(ep) => ep,
        Err(verdict) => return verdict,
    };
    let port = u16::from_be(ctx.load::<u16>(l4 + 2).unwrap_or(0));
    let (peer, addr) = if family == AF_INET {
        let src: [u8; 4] = ctx.load(12).unwrap_or([0; 4]);
        if src[0] == 127 {
            return 1;
        }
        (peer_v4(src), widen_v4(src))
    } else {
        let src: [u8; 16] = ctx.load(8).unwrap_or([0; 16]);
        (peer_v6(src), src)
    };
    let flow = Flow {
        dir: DIR_INGRESS,
        proto,
        port,
        peer,
        family,
        addr,
    };
    judge(mode, &ep, &flow)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
