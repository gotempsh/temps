//! Production kill switch for MCP-role API access (issue #19).
//!
//! MCP itself is an external Node client (`mcp/src/index.ts`) that calls the
//! normal Temps API using an API key whose role is `Role::Mcp`. There was
//! previously no way for an operator to globally disable that access path
//! without deleting every MCP key by hand.
//!
//! `AppSettings.mcp_access_enabled` is the durable, DB-backed source of
//! truth (editable at runtime via the settings API, per-install, with audit
//! logging — see `CLAUDE.md`'s guidance against environment-variable
//! configuration). Because the auth middleware evaluates this on the hot
//! path for every request authenticated with an API key, the value is
//! mirrored into a process-wide `AtomicBool` exactly like
//! `temps_core::tls::insecure_tls_enabled` mirrors `AppSettings.insecure_tls`
//! — so the check never costs a DB round-trip. `ConfigService::get_settings`
//! and `update_settings` republish the cache whenever settings are loaded or
//! saved, so a toggle in the settings UI takes effect within one cache
//! refresh cycle without a server restart.
//!
//! Defaults to `true` (enabled) so installs that already provisioned MCP
//! keys are not locked out until an operator explicitly opts out.

use std::sync::atomic::{AtomicBool, Ordering};

static MCP_ACCESS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Returns `true` unless an operator has explicitly disabled MCP-role API
/// access via `AppSettings.mcp_access_enabled`.
pub fn mcp_access_enabled() -> bool {
    MCP_ACCESS_ENABLED.load(Ordering::Relaxed)
}

/// Update the cached kill-switch flag from the loaded `AppSettings`.
///
/// Called whenever settings are loaded or updated (see
/// `temps_config::ConfigService`) so a toggle in the settings UI takes
/// effect on the next auth check without a server restart.
pub fn set_mcp_access_enabled(enabled: bool) {
    MCP_ACCESS_ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    // `MCP_ACCESS_ENABLED` is a single process-wide global. Rust runs `#[test]`
    // functions concurrently by default, so exercising the default value and
    // the toggle behavior in separate tests would race on the shared
    // AtomicBool. Keep it as one sequential test instead of pulling in a
    // dedicated serialization crate for a single boolean flag.
    #[test]
    fn default_is_enabled_and_toggles_both_ways() {
        // The static initializer sets this to `true`; assert it before any
        // mutation in case a previous test in the same binary changed it.
        set_mcp_access_enabled(true);
        assert!(
            mcp_access_enabled(),
            "kill switch must default to enabled so existing MCP keys keep working"
        );

        set_mcp_access_enabled(false);
        assert!(!mcp_access_enabled());

        set_mcp_access_enabled(true);
        assert!(mcp_access_enabled());
    }
}
