// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TLS configuration helpers shared across server-side HTTP clients.
//!
//! Temps defaults to strict TLS verification on every outbound HTTP client.
//! Operators running self-signed certs on a fully trusted internal network
//! can opt in to skipping verification by toggling `insecure_tls` in the
//! application settings (stored in the database).
//!
//! Settings are loaded at server startup and cached in a process-wide
//! `AtomicBool` so that the sync `make_client()` callsites scattered
//! across the codebase don't have to await a DB lookup. The settings UI
//! re-publishes the value via `set_insecure_tls()` whenever it changes.
//!
//! Worker→control-plane traffic that traverses the public internet
//! (e.g. relay-mode `temps join`) MUST run with strict verification —
//! a MitM there would steal the join token and let an attacker register
//! a malicious worker. The opt-in flag is intended only for lab/internal
//! deployments and only applies to *server-side* clients that read this
//! cache. CLI binaries do not consult it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static INSECURE_TLS: AtomicBool = AtomicBool::new(false);
static WARN_ONCE: Once = Once::new();

/// Returns `true` when the server has been configured to skip TLS
/// certificate verification on outbound HTTP clients.
///
/// Logs a single warning the first time this returns `true` per process
/// so the operator is reminded their traffic is unauthenticated.
pub fn insecure_tls_enabled() -> bool {
    let enabled = INSECURE_TLS.load(Ordering::Relaxed);
    if enabled {
        WARN_ONCE.call_once(|| {
            tracing::warn!(
                "AppSettings.insecure_tls = true — TLS certificate verification is DISABLED \
                 for server-side HTTP clients. This is unsafe on untrusted networks. \
                 Disable it from the settings UI to restore strict verification."
            );
        });
    }
    enabled
}

/// Update the cached opt-in flag from the loaded `AppSettings`.
///
/// Called once at server startup with the persisted value, and again from
/// the settings update handler whenever the operator toggles it.
pub fn set_insecure_tls(enabled: bool) {
    INSECURE_TLS.store(enabled, Ordering::Relaxed);
}
