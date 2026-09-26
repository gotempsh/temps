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

    /// Attest that this provider requires no additional request-policy
    /// enforcement on worker public ingress.
    ///
    /// Providers run in-process and are trusted, but must opt in explicitly:
    /// worker snapshots do not carry provider-specific policy. The fail-closed
    /// default keeps arbitrary providers on control-plane ingress until the
    /// snapshot protocol can carry and enforce their policy. The slot caches
    /// this value at registration, so the attestation must remain valid for
    /// the provider's entire lifetime.
    fn supports_worker_ingress(&self) -> bool {
        false
    }
}

/// OSS default: no request policy is configured.
pub struct OpenRequestPolicyGate;

impl RequestPolicyGate for OpenRequestPolicyGate {
    fn evaluate(&self, _context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
        RequestPolicyDecision::Continue
    }

    fn supports_worker_ingress(&self) -> bool {
        true
    }
}

const REGISTRATION_PENDING: u8 = 0;
const REGISTRATION_PUBLISHING: u8 = 1;
const REGISTRATION_READY_OPEN: u8 = 2;
const REGISTRATION_READY_WORKER: u8 = 3;
const REGISTRATION_READY_CONTROL_PLANE: u8 = 4;

/// Write-once handoff from plugin registration to the live proxy.
pub struct RequestPolicyGateSlot {
    gate: arc_swap::ArcSwap<Arc<dyn RequestPolicyGate>>,
    registration: std::sync::atomic::AtomicU8,
}

impl RequestPolicyGateSlot {
    pub fn new_default() -> Self {
        Self {
            gate: arc_swap::ArcSwap::new(Arc::new(Arc::new(OpenRequestPolicyGate))),
            registration: std::sync::atomic::AtomicU8::new(REGISTRATION_PENDING),
        }
    }

    /// Returns false if registration already completed or another provider
    /// claimed the slot. The gate and its worker capability are published
    /// together before readers can observe a ready state.
    pub fn set(&self, gate: Arc<dyn RequestPolicyGate>) -> bool {
        if self
            .registration
            .compare_exchange(
                REGISTRATION_PENDING,
                REGISTRATION_PUBLISHING,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            let ready_state = if gate.supports_worker_ingress() {
                REGISTRATION_READY_WORKER
            } else {
                REGISTRATION_READY_CONTROL_PLANE
            };
            self.gate.store(Arc::new(gate));
            self.registration
                .store(ready_state, std::sync::atomic::Ordering::Release);
            true
        } else {
            false
        }
    }

    /// Complete plugin discovery. If no provider claimed the slot, the open
    /// default may now safely delegate to the legacy project IP gate. A claim
    /// already being published remains unavailable until publication finishes.
    pub fn finish_registration(&self) {
        let _ = self.registration.compare_exchange(
            REGISTRATION_PENDING,
            REGISTRATION_READY_OPEN,
            std::sync::atomic::Ordering::Release,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    /// Whether worker ingress can preserve the configured request policy.
    ///
    /// The built-in open policy supports worker snapshots. Other providers
    /// fail closed unless they explicitly attest that they require no
    /// additional worker-side enforcement. Discovery and provider publication
    /// also fail closed.
    pub fn supports_worker_ingress(&self) -> bool {
        matches!(
            self.registration.load(std::sync::atomic::Ordering::Acquire),
            REGISTRATION_READY_OPEN | REGISTRATION_READY_WORKER
        )
    }
}

impl Default for RequestPolicyGateSlot {
    fn default() -> Self {
        Self::new_default()
    }
}

impl RequestPolicyGate for RequestPolicyGateSlot {
    fn evaluate(&self, context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
        if !matches!(
            self.registration.load(std::sync::atomic::Ordering::Acquire),
            REGISTRATION_READY_OPEN | REGISTRATION_READY_WORKER | REGISTRATION_READY_CONTROL_PLANE
        ) {
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
        assert!(!slot.supports_worker_ingress());
        slot.finish_registration();
        assert!(slot.supports_worker_ingress());
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

    #[test]
    fn claimed_provider_disables_worker_ingress_export() {
        let slot = RequestPolicyGateSlot::new_default();
        assert!(slot.set(Arc::new(DenyAll)));
        assert!(!slot.supports_worker_ingress());
    }

    #[test]
    fn explicitly_registered_open_provider_supports_worker_ingress_export() {
        let slot = RequestPolicyGateSlot::new_default();
        assert!(slot.set(Arc::new(OpenRequestPolicyGate)));
        assert!(slot.supports_worker_ingress());
    }

    #[test]
    fn registration_race_never_exposes_default_gate_after_custom_provider_claims_slot() {
        struct BlockingCapability {
            publication_barrier: Arc<std::sync::Barrier>,
        }

        impl RequestPolicyGate for BlockingCapability {
            fn evaluate(&self, _context: &RequestPolicyContext<'_>) -> RequestPolicyDecision {
                RequestPolicyDecision::Deny {
                    reason: "test",
                    rule_id: None,
                    revision: None,
                }
            }

            fn supports_worker_ingress(&self) -> bool {
                self.publication_barrier.wait();
                self.publication_barrier.wait();
                false
            }
        }

        let slot = Arc::new(RequestPolicyGateSlot::new_default());
        let publication_barrier = Arc::new(std::sync::Barrier::new(2));
        let setter_slot = Arc::clone(&slot);
        let setter_barrier = Arc::clone(&publication_barrier);
        let setter = std::thread::spawn(move || {
            setter_slot.set(Arc::new(BlockingCapability {
                publication_barrier: setter_barrier,
            }))
        });

        publication_barrier.wait();
        slot.finish_registration();
        let context = RequestPolicyContext {
            path: "/private",
            method: "GET",
            host: "app.example.test",
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
        assert!(!slot.supports_worker_ingress());

        publication_barrier.wait();
        assert!(setter.join().expect("setter thread should complete"));
        assert!(!slot.supports_worker_ingress());
        assert!(matches!(
            slot.evaluate(&context),
            RequestPolicyDecision::Deny { .. }
        ));
    }
}
