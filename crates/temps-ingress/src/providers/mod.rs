// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ingress providers that ship with Temps (ADR-116 D1): the one place
//! outside a provider's own module that names it. Everything else reaches a
//! provider through [`crate::registry::ProviderRegistry`] and the provider
//! traits.

// Private to `providers`, so the compiler keeps the engine from naming a
// provider (ADR-116 D1). Tests reach in for their fixtures.
#[cfg(not(test))]
mod cloudflare;
#[cfg(test)]
pub(crate) mod cloudflare;
#[cfg(not(test))]
mod tailscale;
#[cfg(test)]
pub(crate) mod tailscale;

/// What planning and applying ask of the tailnet: Tailscale's own state,
/// which the engine holds and calls by method.
pub(crate) use tailscale::service::Tailnet;

use std::sync::Arc;

use crate::registry::ProviderFactory;

/// Every built-in provider, in the order the console lists them.
pub fn builtin() -> Vec<Arc<dyn ProviderFactory>> {
    vec![
        Arc::new(crate::providers::cloudflare::CloudflareTunnelFactory),
        Arc::new(crate::providers::tailscale::TailscaleFactory),
    ]
}
