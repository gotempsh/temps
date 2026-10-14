// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub address: String,
    pub console_address: String,
    /// Listener that serves only the console's public routes (ADR-051): health
    /// probes and the unauthenticated-by-session receivers under `/api`
    /// (tracking, provider webhooks, ingest). Requests for the public ingress
    /// hostname, requests the admin gate denies, and public ingest paths on
    /// any host are sent here instead of to `console_address`, so they cannot
    /// reach an admin handler or the dashboard whatever their path.
    ///
    /// `None` when no such listener exists (a split `temps proxy` started
    /// without `--console-ingress-address`): the proxy then keeps the earlier
    /// behaviour, a 404 for gate-denied requests and `console_address` for
    /// public ingest paths.
    pub console_ingress_address: Option<String>,
    /// Proxy-owned loopback service for authenticated worker DNS sync.
    /// `None` keeps the routes on the console, used when cluster DNS is off.
    pub internal_dns_sync_address: Option<String>,
    pub tls_address: Option<String>,
    pub preview_domain: Option<String>, // e.g., "preview.example.com"
    /// When true, HTTP requests are served directly without redirecting to HTTPS.
    /// Useful for local development without TLS certificates.
    pub disable_https_redirect: bool,
    /// On-demand HTTP-01 TLS certificate manager (ADR-018). `None` (default)
    /// disables on-demand issuance entirely — the proxy's TLS callback behaves
    /// exactly as before, returning `Ok(None)` with no side effect when no cert
    /// is found. When wired (only when `on_demand_tls_enabled` is set in
    /// settings), the TLS callback asks this manager to provision a cert in the
    /// background for allowlisted, stable, in-zone hostnames.
    pub on_demand_cert_manager: Option<Arc<crate::on_demand_cert::OnDemandCertManager>>,
    /// Startup state of the console running in this same process
    /// (`temps serve`). When set, requests forwarded to `console_address`
    /// while the console failed to start (or is still starting) get a status
    /// page explaining why instead of a generic 503. `None` for the split
    /// `temps proxy` process, which cannot observe a separate console.
    pub console_startup: Option<Arc<temps_core::console_startup::ConsoleStartupState>>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8080".to_string(),
            console_address: "127.0.0.1:3000".to_string(),
            console_ingress_address: None,
            internal_dns_sync_address: None,
            tls_address: None,
            preview_domain: Some("localhost".to_string()), // Default for local development
            disable_https_redirect: false,
            on_demand_cert_manager: None,
            console_startup: None,
        }
    }
}
