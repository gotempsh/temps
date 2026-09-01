// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extension point for per-project/environment IP allow/restrict rules on
//! the proxy hot path.
//!
//! OSS registers [`OpenIpGate`] at startup, which allows every IP
//! unconditionally. A plugin (e.g. one implementing project-scoped IP
//! access rules) registers an implementation via the service registry only
//! when appropriate (e.g. gated by its own licensing check). The proxy
//! receives the gate at construction time as `Arc<dyn ProjectIpGate>`, so a
//! plugin-free binary allows all traffic unconditionally — this is an
//! opt-in restriction, never a default-deny.
//!
//! `is_allowed` is called once per proxied request (after project/environment
//! context has been resolved) and must be synchronous and lock-free — no
//! I/O, no DB query, no await. Any lookup an implementation needs (e.g.
//! Postgres) must be driven by a background refresh task, with the result
//! cached so individual `is_allowed` calls only read from memory. See
//! [`RetentionResolver`](crate::RetentionResolver) for the established
//! version of this same shape.
//!
//! **Deployment note (split topology).** A standalone proxy process loads no
//! plugins, so nothing claims [`ProjectIpGateSlot`] there and the default
//! gate allows every request. That process does, however, open the same
//! database as the console, so under-enforcing is not structural — a gate
//! only needs a way in. The proxy entrypoint therefore takes an optional
//! builder (`ProxyCommand::execute_with_ip_gate`) that an embedding binary
//! can use to supply a real gate at startup; the plain `temps proxy`
//! entrypoint supplies none and gets [`OpenIpGate`].
//!
//! The consequence worth stating plainly: a standalone proxy started through
//! `execute` enforces nothing, and that is invisible from the outside — the
//! rules are configured, and traffic simply is not checked against them.
//! Anyone running a split topology needs to confirm which entrypoint their
//! proxy nodes use.

use std::net::IpAddr;

/// Extension point for deciding whether a client IP may reach a given
/// project/environment.
pub trait ProjectIpGate: Send + Sync {
    /// Return `true` if `ip` may reach `environment_id` (within `project_id`).
    ///
    /// Implementations must be synchronous and must not perform I/O — see the
    /// module-level note. A registered gate that cannot decide (e.g. it has no
    /// rules configured for this project/environment) should return `true`:
    /// this is an opt-in restriction, so "no rule configured" must mean
    /// "unrestricted", the same as no gate being registered at all.
    fn is_allowed(&self, project_id: i32, environment_id: i32, ip: IpAddr) -> bool;

    /// Return `true` if `project_id`/`environment_id` has an active IP
    /// restriction policy at all (i.e. some IP would be denied by
    /// [`Self::is_allowed`] for this project/environment).
    ///
    /// This exists for exactly one caller: the proxy's client-IP resolution
    /// can fail (e.g. a non-INET socket, an unparsable forwarded-for value),
    /// leaving no `IpAddr` to hand to `is_allowed` at all. In that situation
    /// the proxy must fail closed (deny) only when this project/environment
    /// actually has a restriction configured — denying every request with an
    /// unresolvable IP on every unrestricted project (the common case) would
    /// be a much bigger behavior change than this method exists to avoid.
    ///
    /// Must be synchronous and must not perform I/O, same contract as
    /// `is_allowed`. Default implementation returns `false` (no policy),
    /// matching [`OpenIpGate`] and preserving today's fail-open behavior for
    /// any gate that hasn't been updated to answer this precisely.
    fn has_active_policy(&self, _project_id: i32, _environment_id: i32) -> bool {
        false
    }
}

/// Default [`ProjectIpGate`] that allows every IP unconditionally.
///
/// Registered at startup when no overriding implementation is present.
pub struct OpenIpGate;

impl ProjectIpGate for OpenIpGate {
    fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
        true
    }
}

/// Deferred-registration handle for a [`ProjectIpGate`].
///
/// Constructed with [`OpenIpGate`] loaded by default and handed to the proxy
/// immediately at construction time (as `Arc<dyn ProjectIpGate>`, via unsized
/// coercion — this type itself implements the trait). Once every plugin has
/// finished `register_services`, whichever plugin owns the slot calls
/// [`Self::set`] from `initialize_plugin_services` if a plugin registered an
/// alternative gate — see the module-level note for why this indirection
/// exists (same two-phase handoff as [`RetentionResolverSlot`](crate::RetentionResolverSlot)).
/// `is_allowed` reads are lock-free (`ArcSwap::load`).
///
/// **Write-once semantics:** only the first call to [`Self::set`] takes
/// effect. A second caller cannot silently overwrite a gate that was already
/// installed — the call is a no-op and returns `false`.
pub struct ProjectIpGateSlot {
    gate: arc_swap::ArcSwap<std::sync::Arc<dyn ProjectIpGate>>,
    /// Flipped to `true` by the first successful [`Self::set`] call.
    claimed: std::sync::atomic::AtomicBool,
}

impl ProjectIpGateSlot {
    /// Start with [`OpenIpGate`] loaded.
    pub fn new_default() -> Self {
        Self {
            gate: arc_swap::ArcSwap::new(std::sync::Arc::new(
                std::sync::Arc::new(OpenIpGate) as std::sync::Arc<dyn ProjectIpGate>
            )),
            claimed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Swap in a gate provided by a plugin, but only once.
    ///
    /// Returns `true` if the swap was applied, `false` if a gate was already
    /// set and this call was a no-op.
    pub fn set(&self, gate: std::sync::Arc<dyn ProjectIpGate>) -> bool {
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
            self.gate.store(std::sync::Arc::new(gate));
            true
        } else {
            false
        }
    }
}

impl ProjectIpGate for ProjectIpGateSlot {
    fn is_allowed(&self, project_id: i32, environment_id: i32, ip: IpAddr) -> bool {
        self.gate.load().is_allowed(project_id, environment_id, ip)
    }

    fn has_active_policy(&self, project_id: i32, environment_id: i32) -> bool {
        self.gate
            .load()
            .has_active_policy(project_id, environment_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn open_gate_allows_everything() {
        let g = OpenIpGate;
        assert!(g.is_allowed(1, 1, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
        assert!(g.is_allowed(0, 0, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn slot_defaults_to_open() {
        let slot = ProjectIpGateSlot::new_default();
        assert!(slot.is_allowed(1, 1, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    struct DenyAll;
    impl ProjectIpGate for DenyAll {
        fn is_allowed(&self, _project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
            false
        }

        fn has_active_policy(&self, _project_id: i32, _environment_id: i32) -> bool {
            true
        }
    }

    #[test]
    fn slot_set_takes_effect() {
        let slot = ProjectIpGateSlot::new_default();
        let claimed = slot.set(std::sync::Arc::new(DenyAll));
        assert!(claimed);
        assert!(!slot.is_allowed(1, 1, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    #[test]
    fn open_gate_has_no_active_policy() {
        let g = OpenIpGate;
        assert!(!g.has_active_policy(1, 1));
    }

    #[test]
    fn slot_delegates_has_active_policy_to_the_installed_gate() {
        let slot = ProjectIpGateSlot::new_default();
        assert!(!slot.has_active_policy(1, 1));
        assert!(slot.set(std::sync::Arc::new(DenyAll)));
        assert!(slot.has_active_policy(1, 1));
    }

    #[test]
    fn slot_set_is_write_once() {
        let slot = ProjectIpGateSlot::new_default();
        assert!(slot.set(std::sync::Arc::new(DenyAll)));
        // Second registration is a documented no-op — first writer wins.
        assert!(!slot.set(std::sync::Arc::new(OpenIpGate)));
        // The DenyAll gate from the first call is still in effect.
        assert!(!slot.is_allowed(1, 1, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }
}
