// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Synchronous, allocation-free request policy extension point for resolved routes.
//! Implementations must evaluate an in-memory snapshot: no I/O, awaits, or locks
//! may occur on the proxy request path.

use std::{net::IpAddr, sync::Arc};

/// Borrowed request facts after route and trusted client IP resolution.
///
/// `path` is the URI path exactly as parsed from the incoming request header:
/// the query is excluded, and percent escapes, dot segments, repeated slashes,
/// and backslashes are not decoded or normalized. The proxy does not rewrite
/// that request-header path after evaluation before forwarding it upstream.
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
/// A provider returning `Allow` for a path-based policy is responsible for
/// interpreting [`RequestPolicyContext::path`] exactly as its protected
/// upstream does, and for rejecting ambiguous spellings before allowing them.
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
    /// Evaluate the exact request facts supplied by the proxy.
    ///
    /// Implementations must apply any normalization or ambiguity rejection
    /// required by their own path policy before returning `Allow`.
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

    struct RejectRawAmbiguousPath;
    impl RequestPolicyGate for RejectRawAmbiguousPath {
        fn evaluate(&self, context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
            if matches!(context.path, "/hook%2fadmin" | "/hook/../admin") {
                RequestPolicyDecision::Deny {
                    reason: "ambiguous test path",
                    rule_id: None,
                    revision: None,
                }
            } else {
                RequestPolicyDecision::Allow {
                    rule_id: None,
                    revision: None,
                }
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
    fn slot_preserves_raw_path_for_provider_interpretation() {
        let slot = RequestPolicyGateSlot::new_default();
        assert!(slot.set(Arc::new(RejectRawAmbiguousPath)));

        for path in ["/hook%2fadmin", "/hook/../admin"] {
            let context = RequestPolicyContext {
                path,
                method: "POST",
                host: "example.test",
                project_id: 1,
                environment_id: 2,
                client_ip: None,
            };
            assert!(matches!(
                slot.evaluate(&context),
                RequestPolicyDecision::Deny { .. }
            ));
        }
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
