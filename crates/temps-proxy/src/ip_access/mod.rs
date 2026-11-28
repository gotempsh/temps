// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project / per-environment client-IP allow/deny lists.
//!
//! * [`cidr_set`] parses, validates and matches CIDRs. Matching is a binary
//!   search over pre-merged integer ranges: allocation-free, no locks.
//! * [`gate`] is the [`temps_core::ProjectIpGate`] implementation the proxy
//!   consults per request. It matches against an `ArcSwap` snapshot and never
//!   touches the database on the request path.
//! * [`DenyLogLimiter`] bounds how often a denial is logged under a flood.
//!
//! The client IP these rules see is the one the proxy resolves in
//! `resolve_session_client_ip` (see `client_ip.rs`): the TCP peer, unless the
//! peer is an opted-in loopback proxy or a known CDN edge, in which case the
//! forwarded value it set. A client can never choose its own address by
//! sending `X-Forwarded-For` / `X-Real-IP` / `CF-Connecting-IP`.

pub mod cidr_set;
pub mod gate;

use std::sync::atomic::{AtomicU64, Ordering};

pub use cidr_set::{parse_cidr, CidrError, CidrSet, MAX_CIDRS_PER_POLICY, MAX_DESCRIPTION_LEN};
pub use gate::{IpAccessGateError, ProjectIpAccessGate};

/// How a scope's CIDR list is applied. Defined in `temps-core` so the
/// uptime monitor can name it without depending on the proxy.
pub use temps_core::uptime_monitor_ip_access::IpAccessMode;

/// Lock-free "log at most `max_per_window` per `window_secs`" limiter for the
/// denial log line, so a flood from a blocked network cannot turn the log into
/// the bottleneck. The count of suppressed events is reported by the next
/// line that is let through.
///
/// State is two relaxed atomics; races can let a few extra lines through at a
/// window boundary, which is fine for a log limiter.
pub struct DenyLogLimiter {
    window_secs: u64,
    max_per_window: u64,
    /// Start of the current window, unix seconds.
    window_start: AtomicU64,
    /// Lines emitted in the current window.
    emitted: AtomicU64,
    /// Events dropped since the last emitted line.
    suppressed: AtomicU64,
}

impl DenyLogLimiter {
    pub const fn new(window_secs: u64, max_per_window: u64) -> Self {
        Self {
            window_secs,
            max_per_window,
            window_start: AtomicU64::new(0),
            emitted: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// `Some(suppressed_since_last_line)` when this event should be logged,
    /// `None` when it should be dropped.
    pub fn admit(&self, now_secs: u64) -> Option<u64> {
        let start = self.window_start.load(Ordering::Relaxed);
        if now_secs.saturating_sub(start) >= self.window_secs
            && self
                .window_start
                .compare_exchange(start, now_secs, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.emitted.store(0, Ordering::Relaxed);
        }
        if self.emitted.fetch_add(1, Ordering::Relaxed) < self.max_per_window {
            Some(self.suppressed.swap(0, Ordering::Relaxed))
        } else {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_caps_a_window_and_reports_suppressed_count() {
        let limiter = DenyLogLimiter::new(10, 2);
        assert_eq!(limiter.admit(100), Some(0));
        assert_eq!(limiter.admit(100), Some(0));
        assert_eq!(limiter.admit(101), None);
        assert_eq!(limiter.admit(102), None);
        // New window: the next line reports what was dropped.
        assert_eq!(limiter.admit(111), Some(2));
        assert_eq!(limiter.admit(111), Some(0));
        assert_eq!(limiter.admit(112), None);
    }
}
