//! Concrete anonymous-telemetry reporter.
//!
//! See [`crate`] and [`temps_core::telemetry`] for the abstraction and privacy
//! contract. This service:
//! - persists a stable random `anonymous_id` in the data directory,
//! - honours the `TEMPS_TELEMETRY` opt-out env var,
//! - sends each event as a fire-and-forget timed HTTP POST so a dead endpoint
//!   never affects the running server.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use temps_core::telemetry::{TelemetryEvent, TelemetryReporter};
use thiserror::Error;

/// File (relative to the data dir) holding the stable anonymous instance id.
pub const ANONYMOUS_ID_FILE: &str = "anonymous_id";

/// Default central ingest endpoint. Overridable with `TEMPS_TELEMETRY_ENDPOINT`
/// (e.g. when self-hosting your own ingest, or pointing at a local dev server).
pub const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.temps.sh/v1/events";

/// How long a single telemetry POST is allowed to take before being abandoned.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Error, Debug)]
pub enum TelemetryInitError {
    #[error("Failed to read/write anonymous id file at '{path}': {reason}")]
    AnonymousIdIo { path: String, reason: String },

    #[error("Failed to build telemetry HTTP client: {reason}")]
    HttpClient { reason: String },
}

/// The wire payload sent to the ingest API. Matches the Bun `telemetry-api`
/// `POST /v1/events` body.
#[derive(Debug, Serialize)]
struct EventPayload<'a> {
    anonymous_id: &'a str,
    event_type: &'a str,
    properties: &'a std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temps_version: Option<&'a str>,
}

/// Anonymous product-telemetry reporter.
#[derive(Clone)]
pub struct TelemetryService {
    inner: Arc<Inner>,
}

struct Inner {
    enabled: bool,
    anonymous_id: String,
    temps_version: String,
    endpoint: String,
    client: reqwest::Client,
}

impl TelemetryService {
    /// Build a reporter rooted at `data_dir`.
    ///
    /// `temps_version` is stamped onto every event (pass the server's
    /// `CARGO_PKG_VERSION`). Telemetry is enabled unless the operator opted out
    /// via `TEMPS_TELEMETRY` set to `0`/`false`/`off`/`no`. The anonymous id is
    /// always loaded/generated (even when disabled) so flipping telemetry back
    /// on doesn't churn the instance identity.
    pub fn new(
        data_dir: &Path,
        temps_version: impl Into<String>,
    ) -> Result<Self, TelemetryInitError> {
        let version = temps_version.into();
        let enabled = Self::enabled_from_env();
        let endpoint = std::env::var("TEMPS_TELEMETRY_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_TELEMETRY_ENDPOINT.to_string());

        let anonymous_id = Self::load_or_create_anonymous_id(data_dir)?;

        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .user_agent(format!("temps-telemetry/{version}"))
            .build()
            .map_err(|e| TelemetryInitError::HttpClient {
                reason: e.to_string(),
            })?;

        if enabled {
            tracing::info!(
                anonymous_id = %anonymous_id,
                endpoint = %endpoint,
                "Anonymous product telemetry is ENABLED. No PII is collected. \
                 Disable with TEMPS_TELEMETRY=0."
            );
        } else {
            tracing::info!("Anonymous product telemetry is DISABLED (TEMPS_TELEMETRY opt-out).");
        }

        Ok(Self {
            inner: Arc::new(Inner {
                enabled,
                anonymous_id,
                temps_version: version,
                endpoint,
                client,
            }),
        })
    }

    /// Read the `TEMPS_TELEMETRY` opt-out flag. Enabled by default; treats
    /// `0`/`false`/`off`/`no`/`disabled` (case-insensitive) as opt-out.
    fn enabled_from_env() -> bool {
        match std::env::var("TEMPS_TELEMETRY") {
            Ok(v) => !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "off" | "no" | "disabled"
            ),
            Err(_) => true,
        }
    }

    fn anonymous_id_path(data_dir: &Path) -> PathBuf {
        data_dir.join(ANONYMOUS_ID_FILE)
    }

    /// Load the persisted anonymous id, generating and persisting a new random
    /// one on first run. The id is a random UUID v4 — not derived from anything
    /// machine-identifying.
    fn load_or_create_anonymous_id(data_dir: &Path) -> Result<String, TelemetryInitError> {
        let path = Self::anonymous_id_path(data_dir);

        if path.exists() {
            let raw =
                std::fs::read_to_string(&path).map_err(|e| TelemetryInitError::AnonymousIdIo {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })?;
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
            // Empty/corrupt file: fall through and regenerate.
        }

        let id = format!("inst_{}", uuid::Uuid::new_v4().simple());

        // Best-effort create the data dir; it normally already exists.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &id).map_err(|e| TelemetryInitError::AnonymousIdIo {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        Ok(id)
    }

    /// The stable anonymous id for this instance (exposed for diagnostics).
    pub fn anonymous_id(&self) -> &str {
        &self.inner.anonymous_id
    }
}

#[async_trait::async_trait]
impl TelemetryReporter for TelemetryService {
    fn report(&self, event: TelemetryEvent) {
        if !self.inner.enabled {
            return;
        }

        // Clone the small amount of state the background task needs. The whole
        // point is to return immediately; all network work happens in a
        // detached task with its own timeout (the client is configured with
        // SEND_TIMEOUT).
        let inner = self.inner.clone();

        tokio::spawn(async move {
            let payload = EventPayload {
                anonymous_id: &inner.anonymous_id,
                event_type: &event.event_type,
                properties: &event.properties,
                temps_version: if inner.temps_version.is_empty() {
                    None
                } else {
                    Some(&inner.temps_version)
                },
            };

            match inner
                .client
                .post(&inner.endpoint)
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::trace!(
                        event = %event.event_type,
                        "telemetry event sent"
                    );
                }
                Ok(resp) => {
                    // Non-2xx is not an error worth surfacing loudly — telemetry
                    // is best-effort. Debug level keeps it out of normal logs.
                    tracing::debug!(
                        event = %event.event_type,
                        status = %resp.status(),
                        "telemetry endpoint returned non-success status"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        event = %event.event_type,
                        error = %e,
                        "telemetry send failed (ignored)"
                    );
                }
            }
        });
    }

    fn is_enabled(&self) -> bool {
        self.inner.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::telemetry::TelemetryEventKind;

    /// A temp dir helper that doesn't pull in extra deps.
    fn temp_dir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!("temps-telemetry-test-{}", uuid::Uuid::new_v4().simple());
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn anonymous_id_is_stable_across_loads() {
        let dir = temp_dir();
        let id1 = TelemetryService::load_or_create_anonymous_id(&dir).unwrap();
        let id2 = TelemetryService::load_or_create_anonymous_id(&dir).unwrap();
        assert_eq!(id1, id2, "anonymous id must be stable once generated");
        assert!(id1.starts_with("inst_"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn anonymous_id_regenerated_when_file_empty() {
        let dir = temp_dir();
        let path = TelemetryService::anonymous_id_path(&dir);
        std::fs::write(&path, "   ").unwrap();
        let id = TelemetryService::load_or_create_anonymous_id(&dir).unwrap();
        assert!(id.starts_with("inst_"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn disabled_service_is_noop_and_reports_disabled() {
        let dir = temp_dir();
        // Force opt-out for this construction.
        std::env::set_var("TEMPS_TELEMETRY", "0");
        let svc = TelemetryService::new(&dir, "0.0.0-test").unwrap();
        std::env::remove_var("TEMPS_TELEMETRY");

        assert!(!svc.is_enabled());
        // Must not panic and must not spawn a request.
        svc.report(TelemetryEvent::new(TelemetryEventKind::ProjectCreated));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enabled_from_env_defaults_on_and_honors_opt_out() {
        std::env::remove_var("TEMPS_TELEMETRY");
        assert!(TelemetryService::enabled_from_env());

        for off in ["0", "false", "OFF", "No", "disabled"] {
            std::env::set_var("TEMPS_TELEMETRY", off);
            assert!(
                !TelemetryService::enabled_from_env(),
                "{off} should disable"
            );
        }
        for on in ["1", "true", "yes", "anything-else"] {
            std::env::set_var("TEMPS_TELEMETRY", on);
            assert!(TelemetryService::enabled_from_env(), "{on} should enable");
        }
        std::env::remove_var("TEMPS_TELEMETRY");
    }
}
