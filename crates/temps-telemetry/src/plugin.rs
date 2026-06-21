//! Plugin that registers the anonymous telemetry reporter.
//!
//! Registers an `Arc<dyn TelemetryReporter>` in the service registry so any
//! feature crate can `require_service::<dyn TelemetryReporter>()` (or accept it
//! via constructor) without depending on this crate directly — mirroring the
//! `AuditLogger` wiring.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_config::ServerConfig;
use temps_core::plugin::{PluginError, ServiceRegistrationContext, TempsPlugin};
use temps_core::telemetry::{NoopTelemetryReporter, TelemetryReporter};

use crate::TelemetryService;

/// Plugin for anonymous product telemetry.
pub struct TelemetryPlugin {
    server_config: Arc<ServerConfig>,
    /// Version string stamped onto every event (typically the server's
    /// `CARGO_PKG_VERSION`).
    temps_version: String,
}

impl TelemetryPlugin {
    pub fn new(server_config: Arc<ServerConfig>, temps_version: impl Into<String>) -> Self {
        Self {
            server_config,
            temps_version: temps_version.into(),
        }
    }
}

impl TempsPlugin for TelemetryPlugin {
    fn name(&self) -> &'static str {
        "telemetry"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Telemetry must never block startup. If the reporter can't be
            // built (e.g. the anonymous-id file can't be written), fall back to
            // a no-op reporter and log, rather than failing the server.
            let reporter: Arc<dyn TelemetryReporter> = match TelemetryService::new(
                &self.server_config.data_dir,
                self.temps_version.clone(),
            ) {
                Ok(svc) => {
                    // Wire the DB so once-per-instance milestones
                    // (report_once) are durable across restarts and across the
                    // split proxy/console processes. The DB service is registered
                    // before this plugin; if it's somehow absent, report_once
                    // degrades to a per-process in-memory guard.
                    if let Some(db) = context.get_service::<sea_orm::DatabaseConnection>() {
                        svc.set_db(db);
                    } else {
                        tracing::warn!(
                            "Telemetry: database service not available; once-per-instance \
                             milestones will be guarded per-process only (not durable)"
                        );
                    }
                    Arc::new(svc)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to initialize telemetry reporter; telemetry disabled for this run"
                    );
                    Arc::new(NoopTelemetryReporter)
                }
            };

            context.register_service(reporter);
            tracing::debug!("Telemetry plugin services registered successfully");
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_telemetry() {
        // Construct with a throwaway config; we only assert the name.
        // ServerConfig::new touches the data dir, so use a temp dir.
        let dir = std::env::temp_dir().join(format!(
            "temps-telemetry-plugin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::env::set_var("TEMPS_DATA_DIR", &dir);
        let cfg = ServerConfig::new(
            "127.0.0.1:0".to_string(),
            "postgres://localhost/none".to_string(),
            None,
            None,
        )
        .unwrap();
        std::env::remove_var("TEMPS_DATA_DIR");

        let plugin = TelemetryPlugin::new(Arc::new(cfg), "0.0.0-test");
        assert_eq!(plugin.name(), "telemetry");
        std::fs::remove_dir_all(&dir).ok();
    }
}
