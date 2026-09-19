// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-045 §4: the adapter that turns a `ConsoleOidcConfig`/`ConsoleOidcRevoke`
//! frame arriving on the console-proxy connection into a managed
//! `oidc_providers` row, via [`CloudService`]/`temps-auth`'s `OidcService`.
//!
//! The worker itself lives in `temps-cloud-client` (`ConsoleProxyWorker`)
//! and is started by [`CloudService::start_console_proxy_worker`], which
//! hands it this adapter as its `ConsoleOidcSink`.

use std::sync::Arc;

use temps_cloud_client::ConsoleOidcSink;
use temps_cloud_protocol::console_proxy::ConsoleOidcConfig;

use crate::CloudService;

/// Adapts [`CloudService`]'s managed-OIDC-provider methods to the
/// [`ConsoleOidcSink`] shape a console-proxy connection drives.
pub struct ConsoleOidcAdapter {
    service: Arc<CloudService>,
}

impl ConsoleOidcAdapter {
    pub fn new(service: Arc<CloudService>) -> Self {
        Self { service }
    }
}

#[async_trait::async_trait]
impl ConsoleOidcSink for ConsoleOidcAdapter {
    async fn on_config(&self, config: ConsoleOidcConfig) {
        // `jwks_uri` is not consumed: `OidcService` resolves keys through
        // standard discovery from `issuer`. `console_host` is pinned by the
        // worker itself for the life of the connection (ADR-045 §3).
        let config = temps_auth::oidc_service::ManagedCloudOidcConfig {
            issuer: config.issuer,
            client_id: config.client_id,
            client_secret: config.client_secret,
        };
        if let Err(error) = self.service.apply_console_oidc_config(config).await {
            tracing::error!(
                %error,
                "failed to apply the managed console-access OIDC configuration Temps Cloud sent"
            );
        }
    }

    async fn on_revoke(&self) {
        if let Err(error) = self.service.revoke_console_oidc_provider().await {
            tracing::error!(
                %error,
                "failed to revoke the managed console-access OIDC provider on Cloud's request"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A fake sink that just records whether each hook fired, for testing
    /// call sites that only depend on the [`ConsoleOidcSink`] trait rather
    /// than on a real [`CloudService`] (which needs a live database).
    #[derive(Default)]
    struct RecordingSink {
        configured: AtomicBool,
        revoked: AtomicBool,
    }

    #[async_trait::async_trait]
    impl ConsoleOidcSink for RecordingSink {
        async fn on_config(&self, _config: ConsoleOidcConfig) {
            self.configured.store(true, Ordering::SeqCst);
        }

        async fn on_revoke(&self) {
            self.revoked.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn sink_trait_is_object_safe_and_dispatches() {
        let sink: Arc<dyn ConsoleOidcSink> = Arc::new(RecordingSink::default());
        sink.on_config(ConsoleOidcConfig {
            issuer: "https://cloud.example.com".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            jwks_uri: "https://cloud.example.com/oidc/jwks".to_string(),
            console_host: "instance.console.example.com".to_string(),
        })
        .await;
        sink.on_revoke().await;
        // Reaching here at all proves `dyn ConsoleOidcSink` is object-safe --
        // the shape a `ConsoleProxyWorker::spawn(..., sink: Arc<dyn
        // ConsoleOidcSink>, ...)` parameter requires.
    }
}
