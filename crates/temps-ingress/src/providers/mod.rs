// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ingress providers that ship with Temps (ADR-116 D1): the one place
//! outside a provider's own module that names it. Everything else reaches a
//! provider through [`crate::registry::ProviderRegistry`] and the provider
//! traits.

pub(crate) mod cloudflare;
pub(crate) mod tailscale;

use std::sync::Arc;

use crate::registry::ProviderFactory;

/// Every built-in provider, in the order the console lists them.
pub fn builtin() -> Vec<Arc<dyn ProviderFactory>> {
    vec![
        Arc::new(crate::providers::cloudflare::CloudflareTunnelFactory),
        Arc::new(crate::providers::tailscale::TailscaleFactory),
    ]
}
