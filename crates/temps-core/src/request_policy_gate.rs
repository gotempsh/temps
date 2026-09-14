// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Synchronous, allocation-free request policy extension point for resolved routes.
//! Implementations must evaluate an in-memory snapshot: no I/O, awaits, or locks
//! may occur on the proxy request path.

use std::{net::IpAddr, sync::Arc};

/// Reject path spellings an upstream may parse differently from the policy
/// evaluator. Both live enforcement and simulation must call this function.
pub fn ambiguous_policy_path(path: &str) -> bool {
    if path.contains('\\')
        || path.contains("//")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return true;
    }
    path.as_bytes().windows(3).any(|part| {
        part[0] == b'%'
            && matches!(
                (part[1].to_ascii_lowercase(), part[2].to_ascii_lowercase()),
                (b'2', b'f' | b'e' | b'5') | (b'5', b'c')
            )
    })
}

/// Borrowed request facts after route and trusted client IP resolution.
pub struct RequestPolicyContext<'a> {
    pub path: &'a str,
    pub method: &'a str,
    pub host: &'a str,
    pub project_id: i32,
    pub environment_id: i32,
    pub client_ip: Option<IpAddr>,
}

/// `Continue` delegates to the legacy project IP gate. `Allow` means the
/// installed policy owns project access, including migrated IP restrictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPolicyDecision {
    Continue,
    Allow {
        rule_id: Option<i32>,
        revision: Option<i64>,
    },
    Deny {
        reason: &'static str,
        rule_id: Option<i32>,
        revision: Option<i64>,
    },
    Unavailable {
        reason: &'static str,
    },
}

pub trait RequestPolicyGate: Send + Sync {
    fn evaluate(&self, context: &RequestPolicyContext<'_>) -> RequestPolicyDecision;
}

/// OSS default: no request policy is configured.
pub struct OpenRequestPolicyGate;

impl RequestPolicyGate for OpenRequestPolicyGate {
    fn evaluate(&self, _context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
        RequestPolicyDecision::Continue
    }
}

/// Write-once handoff from plugin registration to the live proxy.
pub struct RequestPolicyGateSlot {
    gate: arc_swap::ArcSwap<Arc<dyn RequestPolicyGate>>,
    claimed: std::sync::atomic::AtomicBool,
    ready: std::sync::atomic::AtomicBool,
}

impl RequestPolicyGateSlot {
    pub fn new_default() -> Self {
        Self {
            gate: arc_swap::ArcSwap::new(Arc::new(Arc::new(OpenRequestPolicyGate))),
            claimed: std::sync::atomic::AtomicBool::new(false),
            ready: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Returns false if another provider already claimed the slot.
    pub fn set(&self, gate: Arc<dyn RequestPolicyGate>) -> bool {
        if self
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            self.gate.store(Arc::new(gate));
            self.ready.store(true, std::sync::atomic::Ordering::Release);
            true
        } else {
            false
        }
    }

    /// Complete plugin discovery. If no provider claimed the slot, the open
    /// default may now safely delegate to the legacy project IP gate.
    pub fn finish_registration(&self) {
        self.ready.store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Default for RequestPolicyGateSlot {
    fn default() -> Self {
        Self::new_default()
    }
}

impl RequestPolicyGate for RequestPolicyGateSlot {
    fn evaluate(&self, context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
        if !self.ready.load(std::sync::atomic::Ordering::Acquire) {
            return RequestPolicyDecision::Unavailable {
                reason: "request policy registration pending",
            };
        }
        self.gate.load().evaluate(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_path_spellings() {
        for path in [
            "/a//b", "/a/../b", "/a/./b", "/a%2fb", "/a%2Fb", "/a%5cb", "/a%2eb", "/a%25b", "/a\\b",
        ] {
            assert!(ambiguous_policy_path(path), "{path}");
        }
        for path in ["/a/b", "/a-b", "/a%20b", "/a.b", "/a/%E2%82%AC"] {
            assert!(!ambiguous_policy_path(path), "{path}");
        }
    }

    struct DenyAll;
    impl RequestPolicyGate for DenyAll {
        fn evaluate(&self, _context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
            RequestPolicyDecision::Deny {
                reason: "test",
                rule_id: Some(1),
                revision: Some(2),
            }
        }
    }

    #[test]
    fn slot_defaults_to_continue_and_first_registration_wins() {
        let slot = RequestPolicyGateSlot::new_default();
        let context = RequestPolicyContext {
            path: "/",
            method: "GET",
            host: "example.test",
            project_id: 1,
            environment_id: 2,
            client_ip: None,
        };
        assert_eq!(
            slot.evaluate(&context),
            RequestPolicyDecision::Unavailable {
                reason: "request policy registration pending"
            }
        );
        assert!(slot.set(Arc::new(DenyAll)));
        assert!(!slot.set(Arc::new(OpenRequestPolicyGate)));
        assert_eq!(
            slot.evaluate(&context),
            RequestPolicyDecision::Deny {
                reason: "test",
                rule_id: Some(1),
                revision: Some(2)
            }
        );
    }

    #[test]
    fn completed_registration_without_provider_delegates_to_legacy_gate() {
        let slot = RequestPolicyGateSlot::new_default();
        slot.finish_registration();
        let context = RequestPolicyContext {
            path: "/",
            method: "GET",
            host: "example.test",
            project_id: 1,
            environment_id: 2,
            client_ip: None,
        };
        assert_eq!(slot.evaluate(&context), RequestPolicyDecision::Continue);
    }
}
