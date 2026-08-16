//! Anonymous product telemetry abstraction.
//!
//! Temps optionally reports **anonymous** product-usage events to a central
//! endpoint so the maintainers can understand whether the product is actually
//! working for self-hosters (e.g. "instances that tried to deploy" vs
//! "instances that deployed successfully"). No PII, repo names, domains, or
//! secrets are ever sent — only a stable random `anonymous_id` generated on the
//! instance, the event name, and a small bag of non-identifying properties.
//!
//! This module defines the *abstraction* only. The concrete reporter (HTTP
//! client, anonymous-id persistence, opt-out handling) lives in the
//! `temps-telemetry` crate. Feature crates depend on the [`TelemetryReporter`]
//! trait via `Arc<dyn TelemetryReporter>` so they never need a direct
//! dependency on the telemetry crate, mirroring the [`crate::AuditLogger`]
//! pattern.
//!
//! Reporting is **fire-and-forget**: a call to [`TelemetryReporter::report`]
//! must never block the caller or fail the surrounding operation. A dead or
//! slow telemetry endpoint has zero effect on user-facing behaviour.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Canonical set of product-telemetry event names.
///
/// Kept as an enum (rather than free strings) so callsites can't typo an event
/// name and the central ingest API's accepted list stays in lockstep with what
/// the binary can actually emit. The wire representation is the snake_case
/// string returned by [`TelemetryEventKind::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryEventKind {
    // ---- Instance lifecycle ----
    InstanceStarted,
    InstanceHeartbeat,
    InstanceSetupCompleted,
    UpgradeCompleted,
    WorkerNodeJoined,

    // ---- Deployment funnel ----
    DeployAttempted,
    DeploySucceeded,
    DeployFailed,
    DeployCancelled,
    RollbackTriggered,
    FirstDeploySucceeded,

    // ---- Project & environment ----
    ProjectCreated,
    /// A project was created from a curated template. Carries the (public,
    /// non-identifying) `template_slug` so we can measure which templates drive
    /// activation. Emitted in addition to `ProjectCreated`.
    ProjectCreatedFromTemplate,
    EnvironmentCreated,
    ScaleToZeroConfigured,
    AutoDeployEnabled,
    AttackModeEnabled,

    // ---- Git & source ----
    GitProviderConnected,

    // ---- Domains & networking ----
    CustomDomainAdded,
    SslCertificateIssued,

    // ---- Managed services ----
    ServiceCreated,
    ServiceClusterCreated,
    PgMajorUpgradeCompleted,
    PitrRestoreTriggered,
    BackupConfigured,

    // ---- Observability suite activation ----
    AnalyticsFirstEventReceived,
    SessionReplayFirstSession,
    ErrorTrackingFirstError,
    AiGatewayFirstRequest,

    // ---- AI features ----
    AiSreConversationStarted,
    AutofixerFixAccepted,
    AutofixerFixRejected,

    // ---- Auth & security ----
    OidcProviderConfigured,
    ApiKeyCreated,
    VulnerabilityScanTriggered,

    // ---- Email ----
    EmailProviderConfigured,

    // ---- Status page ----
    StatusPagePublished,

    // ---- Instance health ----
    /// Periodic aggregated summary of internal errors on the instance (ERROR
    /// logs by target, console-API 5xx by route template, panics by source
    /// location). Carries only counts keyed by compile-time identifiers of our
    /// own code — never error messages. See [`crate::error_metrics`].
    ErrorSummary,
}

impl TelemetryEventKind {
    /// The stable snake_case wire name for this event.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InstanceStarted => "instance_started",
            Self::InstanceHeartbeat => "instance_heartbeat",
            Self::InstanceSetupCompleted => "instance_setup_completed",
            Self::UpgradeCompleted => "upgrade_completed",
            Self::WorkerNodeJoined => "worker_node_joined",

            Self::DeployAttempted => "deploy_attempted",
            Self::DeploySucceeded => "deploy_succeeded",
            Self::DeployFailed => "deploy_failed",
            Self::DeployCancelled => "deploy_cancelled",
            Self::RollbackTriggered => "rollback_triggered",
            Self::FirstDeploySucceeded => "first_deploy_succeeded",

            Self::ProjectCreated => "project_created",
            Self::ProjectCreatedFromTemplate => "project_created_from_template",
            Self::EnvironmentCreated => "environment_created",
            Self::ScaleToZeroConfigured => "scale_to_zero_configured",
            Self::AutoDeployEnabled => "auto_deploy_enabled",
            Self::AttackModeEnabled => "attack_mode_enabled",

            Self::GitProviderConnected => "git_provider_connected",

            Self::CustomDomainAdded => "custom_domain_added",
            Self::SslCertificateIssued => "ssl_certificate_issued",

            Self::ServiceCreated => "service_created",
            Self::ServiceClusterCreated => "service_cluster_created",
            Self::PgMajorUpgradeCompleted => "pg_major_upgrade_completed",
            Self::PitrRestoreTriggered => "pitr_restore_triggered",
            Self::BackupConfigured => "backup_configured",

            Self::AnalyticsFirstEventReceived => "analytics_first_event_received",
            Self::SessionReplayFirstSession => "session_replay_first_session",
            Self::ErrorTrackingFirstError => "error_tracking_first_error",
            Self::AiGatewayFirstRequest => "ai_gateway_first_request",

            Self::AiSreConversationStarted => "ai_sre_conversation_started",
            Self::AutofixerFixAccepted => "autofixer_fix_accepted",
            Self::AutofixerFixRejected => "autofixer_fix_rejected",

            Self::OidcProviderConfigured => "oidc_provider_configured",
            Self::ApiKeyCreated => "api_key_created",
            Self::VulnerabilityScanTriggered => "vulnerability_scan_triggered",

            Self::EmailProviderConfigured => "email_provider_configured",

            Self::StatusPagePublished => "status_page_published",

            Self::ErrorSummary => "error_summary",
        }
    }

    /// Every known event name, used by tooling and tests to keep the central
    /// ingest API's accepted list in sync with the binary.
    pub fn all() -> &'static [TelemetryEventKind] {
        &[
            Self::InstanceStarted,
            Self::InstanceHeartbeat,
            Self::InstanceSetupCompleted,
            Self::UpgradeCompleted,
            Self::WorkerNodeJoined,
            Self::DeployAttempted,
            Self::DeploySucceeded,
            Self::DeployFailed,
            Self::DeployCancelled,
            Self::RollbackTriggered,
            Self::FirstDeploySucceeded,
            Self::ProjectCreated,
            Self::ProjectCreatedFromTemplate,
            Self::EnvironmentCreated,
            Self::ScaleToZeroConfigured,
            Self::AutoDeployEnabled,
            Self::AttackModeEnabled,
            Self::GitProviderConnected,
            Self::CustomDomainAdded,
            Self::SslCertificateIssued,
            Self::ServiceCreated,
            Self::ServiceClusterCreated,
            Self::PgMajorUpgradeCompleted,
            Self::PitrRestoreTriggered,
            Self::BackupConfigured,
            Self::AnalyticsFirstEventReceived,
            Self::SessionReplayFirstSession,
            Self::ErrorTrackingFirstError,
            Self::AiGatewayFirstRequest,
            Self::AiSreConversationStarted,
            Self::AutofixerFixAccepted,
            Self::AutofixerFixRejected,
            Self::OidcProviderConfigured,
            Self::ApiKeyCreated,
            Self::VulnerabilityScanTriggered,
            Self::EmailProviderConfigured,
            Self::StatusPagePublished,
            Self::ErrorSummary,
        ]
    }
}

/// A single anonymous telemetry event.
///
/// `properties` must contain only non-identifying values (counts, enum labels,
/// durations). It must never contain emails, IPs, repo names, domains, env-var
/// names/values, or any free-form user text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// The event name (snake_case wire form).
    pub event_type: String,
    /// Non-identifying properties. Ordered for stable serialization in tests.
    pub properties: BTreeMap<String, serde_json::Value>,
}

impl TelemetryEvent {
    /// Start building an event from a known kind.
    pub fn new(kind: TelemetryEventKind) -> Self {
        Self {
            event_type: kind.as_str().to_string(),
            properties: BTreeMap::new(),
        }
    }

    /// Attach a non-identifying property. Chainable.
    ///
    /// Values are converted via `serde_json::Value::from`, so strings, numbers,
    /// and bools all work. Prefer enum labels and counts — never raw user input.
    pub fn with<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<serde_json::Value>,
    {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Attach an optional property only when present. Chainable.
    pub fn with_opt<K, V>(self, key: K, value: Option<V>) -> Self
    where
        K: Into<String>,
        V: Into<serde_json::Value>,
    {
        match value {
            Some(v) => self.with(key, v),
            None => self,
        }
    }
}

/// Trait for services that can report anonymous product telemetry.
///
/// Implementations MUST be fire-and-forget: [`Self::report`] returns
/// immediately (spawning any network work in the background) and never returns
/// an error to the caller. A disabled reporter (operator opted out) is a no-op.
#[async_trait::async_trait]
pub trait TelemetryReporter: Send + Sync {
    /// Report an event. Never blocks on the network and never fails the caller.
    fn report(&self, event: TelemetryEvent);

    /// Report a "first-touch" milestone event **at most once per instance, ever**.
    ///
    /// Use this for events whose name promises a single lifetime occurrence
    /// (e.g. `analytics_first_event_received`, `ai_gateway_first_request`,
    /// `first_deploy_succeeded`) but whose callsite fires per-action (every
    /// pageview / request / deploy). Without this guard those events become a
    /// firehose: telemetry volume scales with the self-hoster's production
    /// traffic, which both skews the metric and leaks a coarse activity signal.
    ///
    /// `milestone` is a stable key (use the event's wire name) used to dedupe.
    /// Implementations MUST be fire-and-forget like [`Self::report`] and MUST
    /// guarantee the hot path stays cheap (an in-process check before any
    /// durable lookup), so a busy instance pays no per-event cost after the
    /// first emit.
    ///
    /// The default implementation simply forwards to [`Self::report`] every time
    /// (no dedupe) — concrete reporters override it with the real once-guard.
    fn report_once(&self, _milestone: &'static str, event: TelemetryEvent) {
        self.report(event);
    }

    /// Whether telemetry is currently enabled. Callsites can use this to skip
    /// building expensive property bags when reporting is off.
    fn is_enabled(&self) -> bool;
}

/// A no-op reporter used when telemetry is disabled or unavailable, so callers
/// can always hold an `Arc<dyn TelemetryReporter>` without `Option`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTelemetryReporter;

#[async_trait::async_trait]
impl TelemetryReporter for NoopTelemetryReporter {
    fn report(&self, _event: TelemetryEvent) {}
    fn is_enabled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_wire_names_are_snake_case_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for kind in TelemetryEventKind::all() {
            let name = kind.as_str();
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "event name '{name}' must be snake_case"
            );
            assert!(seen.insert(name), "duplicate event name '{name}'");
        }
    }

    #[test]
    fn all_covers_every_variant() {
        // If a variant is added but not added to all(), as_str() on it will be
        // missing from the list and this length check is a cheap tripwire.
        // 38 events (34 initial + instance_heartbeat + project_created_from_template
        // + error_summary + deploy_cancelled).
        assert_eq!(TelemetryEventKind::all().len(), 38);
    }

    #[test]
    fn builder_attaches_properties() {
        let event = TelemetryEvent::new(TelemetryEventKind::DeploySucceeded)
            .with("runtime", "nixpacks")
            .with("duration_ms", 8700)
            .with_opt("region", Some("fsn1"))
            .with_opt::<_, String>("absent", None);

        assert_eq!(event.event_type, "deploy_succeeded");
        assert_eq!(event.properties.get("runtime").unwrap(), "nixpacks");
        assert_eq!(event.properties.get("duration_ms").unwrap(), 8700);
        assert_eq!(event.properties.get("region").unwrap(), "fsn1");
        assert!(!event.properties.contains_key("absent"));
    }

    #[test]
    fn noop_reporter_is_disabled_and_silent() {
        let reporter = NoopTelemetryReporter;
        assert!(!reporter.is_enabled());
        // Must not panic.
        reporter.report(TelemetryEvent::new(TelemetryEventKind::ProjectCreated));
    }
}
