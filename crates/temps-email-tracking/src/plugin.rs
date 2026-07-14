//! Plugin registration for email tracking

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext};
use temps_core::EncryptionService;
use tracing::debug;

use crate::event_service::EmailEventService;
use crate::handlers::{self, TrackingState};
use crate::html_rewriter::HtmlTrackingRewriter;
use crate::sns::SnsVerifier;
use temps_email::SuppressionService;

/// Email tracking plugin
pub struct EmailTrackingPlugin;

impl Default for EmailTrackingPlugin {
    fn default() -> Self {
        Self
    }
}

impl EmailTrackingPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl temps_core::plugin::TempsPlugin for EmailTrackingPlugin {
    fn name(&self) -> &'static str {
        "email-tracking"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();

            // Use the platform's external URL for tracking endpoints
            let config_service = context.require_service::<temps_config::ConfigService>();
            let external_url = config_service
                .get_external_url_or_default()
                .await
                .unwrap_or_else(|_| "http://localho.st".to_string());

            // Tracking base URL = external URL + /api (all routes are under /api)
            let tracking_base_url = format!("{}/api", external_url.trim_end_matches('/'));
            debug!("Email tracking base URL: {}", tracking_base_url);

            // Derive HMAC key from the platform's encryption service
            let encryption_service = context.require_service::<EncryptionService>();
            let hmac_key = encryption_service
                .derive_subkey("temps-email-tracking")
                .to_vec();

            // Create HTML rewriter for pixel injection + link rewriting
            let rewriter = Arc::new(HtmlTrackingRewriter::new(
                tracking_base_url,
                hmac_key.clone(),
            ));
            context.register_service(rewriter);

            // Create event service
            let event_service = Arc::new(EmailEventService::new(db.clone()));
            context.register_service(event_service.clone());

            let sns_verifier = Arc::new(SnsVerifier::new().map_err(|error| {
                PluginError::PluginRegistrationFailed {
                    plugin_name: "email-tracking".to_string(),
                    error: error.to_string(),
                }
            })?);
            let provider_service = context.require_service::<temps_email::ProviderService>();

            // SuppressionService is a thin, stateless DB wrapper (see
            // temps-email), so constructing our own instance here — rather
            // than looking one up via context.get_service — is safe and
            // avoids a hard dependency on the email plugin having already
            // registered its own copy first.
            let suppression_service = Arc::new(SuppressionService::new(db));

            // Create tracking state for handlers
            let tracking_state = Arc::new(TrackingState {
                event_service,
                sns_verifier,
                hmac_key,
                suppression_service,
                provider_service,
            });
            context.register_service(tracking_state);

            Ok(())
        })
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let tracking_state = context.get_service::<TrackingState>()?;

        let routes = handlers::public_routes().with_state(tracking_state);

        Some(PluginRoutes::new(routes))
    }
}
