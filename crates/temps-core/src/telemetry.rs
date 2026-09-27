// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// `temps serve` started a different Temps version than the previous
    /// successful start, or applied migrations to an existing database.
    /// Carries `from_version` (`unknown` when not recorded), `to_version` and
    /// `migrations_applied`.
    UpgradeCompleted,
    /// Startup after an upgrade failed while applying database migrations.
    /// Carries the versions, `stage`, `pending_migrations` and a failure code.
    /// Sent synchronously before the process exits.
    UpgradeFailed,
    WorkerNodeJoined,
    /// A worker node's registration was rejected or failed.
    WorkerNodeJoinFailed,

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
    GitProviderConnectFailed,

    // ---- Domains & networking ----
    CustomDomainAdded,
    SslCertificateIssued,
    /// Certificate issuance or renewal failed. Carries `stage`,
    /// `verification_method` and a failure code; the automatic renewal
    /// scheduler also sets `renewal` and `automatic`.
    SslCertificateFailed,

    // ---- Managed services ----
    ServiceCreated,
    ServiceClusterCreated,
    /// Creating a managed service failed. Carries `engine`.
    ServiceCreateFailed,
    PgMajorUpgradeCompleted,
    PgMajorUpgradeFailed,
    PitrRestoreTriggered,
    BackupConfigured,
    /// One backup run finished successfully. Carries the `engine` key and
    /// coarse `duration_bucket` / `size_bucket` bands.
    BackupSucceeded,
    BackupFailed,
    /// One restore finished successfully. Carries `mode` and a coarse
    /// `duration_bucket`.
    RestoreSucceeded,
    RestoreFailed,

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
            Self::UpgradeFailed => "upgrade_failed",
            Self::WorkerNodeJoined => "worker_node_joined",
            Self::WorkerNodeJoinFailed => "worker_node_join_failed",

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
            Self::GitProviderConnectFailed => "git_provider_connect_failed",

            Self::CustomDomainAdded => "custom_domain_added",
            Self::SslCertificateIssued => "ssl_certificate_issued",
            Self::SslCertificateFailed => "ssl_certificate_failed",

            Self::ServiceCreated => "service_created",
            Self::ServiceClusterCreated => "service_cluster_created",
            Self::ServiceCreateFailed => "service_create_failed",
            Self::PgMajorUpgradeCompleted => "pg_major_upgrade_completed",
            Self::PgMajorUpgradeFailed => "pg_major_upgrade_failed",
            Self::PitrRestoreTriggered => "pitr_restore_triggered",
            Self::BackupConfigured => "backup_configured",
            Self::BackupSucceeded => "backup_succeeded",
            Self::BackupFailed => "backup_failed",
            Self::RestoreSucceeded => "restore_succeeded",
            Self::RestoreFailed => "restore_failed",

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
            Self::UpgradeFailed,
            Self::WorkerNodeJoined,
            Self::WorkerNodeJoinFailed,
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
            Self::GitProviderConnectFailed,
            Self::CustomDomainAdded,
            Self::SslCertificateIssued,
            Self::SslCertificateFailed,
            Self::ServiceCreated,
            Self::ServiceClusterCreated,
            Self::ServiceCreateFailed,
            Self::PgMajorUpgradeCompleted,
            Self::PgMajorUpgradeFailed,
            Self::PitrRestoreTriggered,
            Self::BackupConfigured,
            Self::BackupSucceeded,
            Self::BackupFailed,
            Self::RestoreSucceeded,
            Self::RestoreFailed,
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

/// Version of the [`OperationFailureCode`] taxonomy. Sent with every failure
/// event as `classifier_version`; increment it when matching semantics or wire
/// labels change so the dashboard can tell old and new classifications apart.
pub const OPERATION_FAILURE_CLASSIFIER_VERSION: u8 = 1;

/// Fixed, non-identifying reason an operation (backup, restore, service
/// creation, certificate issuance, ...) failed.
///
/// Failure events carry one of these labels instead of the error message: the
/// message can contain hostnames, bucket names, credentials or user input, so
/// it never leaves the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationFailureCode {
    Timeout,
    DnsResolution,
    NetworkConnection,
    Tls,
    Authentication,
    PermissionDenied,
    NotFound,
    Conflict,
    RateLimited,
    InvalidConfiguration,
    UnsupportedVersion,
    DiskExhausted,
    OutOfMemory,
    ImagePull,
    ContainerStart,
    /// No node (local daemon or worker) is allowed or able to run the workload.
    NoEligibleNode,
    Storage,
    Database,
    Cancelled,
    /// The ACME server could not validate control of the domain (DNS not
    /// pointing at this server, challenge not reachable, CAA forbids issuance).
    ChallengeValidation,
    Unknown,
}

impl OperationFailureCode {
    /// The stable snake_case wire label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::DnsResolution => "dns_resolution",
            Self::NetworkConnection => "network_connection",
            Self::Tls => "tls",
            Self::Authentication => "authentication",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::UnsupportedVersion => "unsupported_version",
            Self::DiskExhausted => "disk_exhausted",
            Self::OutOfMemory => "out_of_memory",
            Self::ImagePull => "image_pull",
            Self::ContainerStart => "container_start",
            Self::NoEligibleNode => "no_eligible_node",
            Self::Storage => "storage",
            Self::Database => "database",
            Self::Cancelled => "cancelled",
            Self::ChallengeValidation => "challenge_validation",
            Self::Unknown => "unknown",
        }
    }

    /// Every code, for tests and tooling.
    pub fn all() -> &'static [OperationFailureCode] {
        &[
            Self::Timeout,
            Self::DnsResolution,
            Self::NetworkConnection,
            Self::Tls,
            Self::Authentication,
            Self::PermissionDenied,
            Self::NotFound,
            Self::Conflict,
            Self::RateLimited,
            Self::InvalidConfiguration,
            Self::UnsupportedVersion,
            Self::DiskExhausted,
            Self::OutOfMemory,
            Self::ImagePull,
            Self::ContainerStart,
            Self::NoEligibleNode,
            Self::Storage,
            Self::Database,
            Self::Cancelled,
            Self::ChallengeValidation,
            Self::Unknown,
        ]
    }

    /// Classify a free-form error message into a fixed code, locally.
    ///
    /// Matching is most-specific-first: resource exhaustion, ACME problem
    /// types and image pulls before the generic authentication, permission and
    /// network buckets, which their messages also mention. HTTP status codes
    /// only count when they appear as a status (`status: 404`), never as bare
    /// digits, since messages routinely contain IDs, sizes and migration names.
    pub fn classify(message: &str) -> Self {
        let m = message.to_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
        let status = |code: u16| has_http_status(&m, code);

        if has(&[
            "no space left on device",
            "disk quota exceeded",
            "enospc",
            "disk full",
        ]) {
            Self::DiskExhausted
        } else if has(&[
            "out of memory",
            "oomkilled",
            "cannot allocate memory",
            "exit code 137",
        ]) {
            Self::OutOfMemory
        } else if has(&["acme:error:ratelimited"]) {
            Self::RateLimited
        } else if has(&["acme:error:dns"]) {
            Self::DnsResolution
        } else if has(&["acme:error:connection"]) {
            Self::NetworkConnection
        } else if has(&[
            "acme:error:unauthorized",
            "acme:error:incorrectresponse",
            "acme:error:caa",
            "acme:error:rejectedidentifier",
            "acme:error:tls",
        ]) {
            Self::ChallengeValidation
        } else if has(&[
            "manifest unknown",
            "pull access denied",
            "failed to pull",
            "error pulling image",
            "image not found",
        ]) {
            Self::ImagePull
        } else if has(&["timed out", "timeout", "deadline exceeded"]) {
            Self::Timeout
        } else if has(&["cancelled", "canceled", "aborted by user"]) && !has(&["context canceled"])
        {
            Self::Cancelled
        } else if has(&[
            "certificate verify failed",
            "invalid certificate",
            "certificate has expired",
            "self-signed certificate",
            "self signed certificate",
            "tls handshake",
            "unknownissuer",
        ]) {
            Self::Tls
        } else if has(&[
            "failed to lookup address",
            "dns error",
            "name or service not known",
            "nodename nor servname",
            "no such host",
            "nxdomain",
            "temporary failure in name resolution",
        ]) {
            Self::DnsResolution
        } else if has(&["rate limit", "ratelimit", "too many requests"]) || status(429) {
            Self::RateLimited
        } else if has(&[
            "unauthorized",
            "authentication failed",
            "invalid credentials",
            "bad credentials",
            "invalidaccesskeyid",
            "signaturedoesnotmatch",
            "password authentication failed",
            "invalid token",
        ]) || status(401)
        {
            Self::Authentication
        } else if has(&[
            "permission denied",
            "access denied",
            "accessdenied",
            "forbidden",
        ]) || status(403)
        {
            Self::PermissionDenied
        } else if has(&[
            "connection refused",
            "connection reset",
            "broken pipe",
            "network is unreachable",
            "host is unreachable",
            "error trying to connect",
            "error sending request",
        ]) {
            Self::NetworkConnection
        } else if has(&[
            "unsupported version",
            "version mismatch",
            "incompatible version",
            "version is not supported",
            "version not supported",
        ]) {
            Self::UnsupportedVersion
        } else if has(&[
            "already exists",
            "conflict",
            "duplicate key",
            "already in use",
            "port is already allocated",
        ]) {
            Self::Conflict
        } else if has(&[
            "not found",
            "no such",
            "no acme order",
            "does not exist",
            "nosuchbucket",
            "nosuchkey",
        ]) || status(404)
        {
            Self::NotFound
        } else if has(&[
            "failed to start container",
            "container exited",
            "exited with code",
            "oci runtime",
        ]) {
            Self::ContainerStart
        } else if has(&["s3", "bucket", "multipart", "object store"]) {
            Self::Storage
        } else if has(&[
            "database",
            "sqlstate",
            "sea_orm",
            "dberr",
            "relation",
            "migration",
        ]) {
            Self::Database
        } else if has(&[
            "invalid",
            "validation",
            "missing required",
            "must be",
            "malformed",
        ]) {
            Self::InvalidConfiguration
        } else {
            Self::Unknown
        }
    }
}

/// Whether a lowercased message reports HTTP status `code` as a status, e.g.
/// `status: 404`, `status code 429` or `http 401`, and not merely contains the
/// digits inside an ID, size or migration name.
fn has_http_status(message: &str, code: u16) -> bool {
    const PREFIXES: [&str; 7] = [
        "status ",
        "status: ",
        "status=",
        "status code ",
        "status code: ",
        "http ",
        "http/1.1 ",
    ];
    let code = code.to_string();
    PREFIXES.iter().any(|prefix| {
        let needle = format!("{prefix}{code}");
        message.match_indices(&needle).any(|(at, _)| {
            let after = message[at + needle.len()..].chars().next();
            !after.is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// Coarse duration band for operation events. Exact durations would let a
/// long-running series be correlated across events, so only the band is sent.
pub fn duration_bucket(duration: std::time::Duration) -> &'static str {
    match duration.as_secs() {
        0..=9 => "<10s",
        10..=59 => "10s-1m",
        60..=299 => "1-5m",
        300..=1799 => "5-30m",
        1800..=7199 => "30m-2h",
        _ => ">2h",
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

    /// Attach a failure classification: `failure_code` plus
    /// `classifier_version`. Never attaches the message itself.
    pub fn with_failure(self, code: OperationFailureCode) -> Self {
        self.with("failure_code", code.as_str())
            .with("classifier_version", OPERATION_FAILURE_CLASSIFIER_VERSION)
    }

    /// Classify `message` locally and attach only the resulting code.
    pub fn with_failure_from_message(self, message: &str) -> Self {
        self.with_failure(OperationFailureCode::classify(message))
    }

    /// Attach a coarse `duration_bucket` (see [`duration_bucket`]).
    pub fn with_duration(self, duration: std::time::Duration) -> Self {
        self.with("duration_bucket", duration_bucket(duration))
    }

    /// Attach bounded template provenance without allowing operator-defined
    /// slugs to leave the instance. Only reviewed bundled slugs are emitted.
    pub fn with_template_provenance(self, provenance: Option<&str>) -> Self {
        let (source, safe_slug) = match provenance {
            None => ("none", None),
            Some(value) => {
                if let Some(slug) = crate::templates::telemetry_safe_template_slug(value) {
                    ("bundled", Some(slug))
                } else {
                    ("custom", None)
                }
            }
        };

        self.with("is_template", provenance.is_some())
            .with("template_source", source)
            .with_opt("template_slug", safe_slug.map(str::to_string))
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
        // + error_summary + deploy_cancelled), plus 10 operation outcome events.
        assert_eq!(TelemetryEventKind::all().len(), 48);
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
    fn template_provenance_exposes_only_reviewed_public_slugs() {
        let service_event = TelemetryEvent::new(TelemetryEventKind::DeployAttempted)
            .with_template_provenance(Some("keycloak"));
        assert_eq!(service_event.properties["template_source"], "bundled");
        assert_eq!(service_event.properties["template_slug"], "keycloak");

        let private = "customer-private-template";
        let private_event = TelemetryEvent::new(TelemetryEventKind::DeployAttempted)
            .with_template_provenance(Some(private));
        let serialized = serde_json::to_string(&private_event).unwrap();
        assert_eq!(private_event.properties["template_source"], "custom");
        assert!(!private_event.properties.contains_key("template_slug"));
        assert!(!serialized.contains(private));
    }

    #[test]
    fn failure_codes_are_snake_case_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for code in OperationFailureCode::all() {
            let name = code.as_str();
            assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!(seen.insert(name), "duplicate failure code '{name}'");
        }
    }

    #[test]
    fn classify_ignores_status_digits_inside_ids_and_names() {
        use OperationFailureCode as C;
        let cases = [
            (
                "Migration m20260401_000001_add_column failed to apply",
                C::Database,
            ),
            ("backup 1404 could not be read from disk", C::Unknown),
            ("upload failed, RequestId: 7A4291D4C0429E81", C::Unknown),
            ("operation not supported on this filesystem", C::Unknown),
        ];
        for (message, expected) in cases {
            assert_eq!(
                OperationFailureCode::classify(message),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn classify_maps_common_messages() {
        use OperationFailureCode as C;
        let cases = [
            (
                "write /backups/x.tar: No space left on device",
                C::DiskExhausted,
            ),
            ("container was OOMKilled", C::OutOfMemory),
            ("error sending request: tls handshake eof", C::Tls),
            (
                "failed to lookup address information: nodename nor servname provided",
                C::DnsResolution,
            ),
            (
                "urn:ietf:params:acme:error:rateLimited: too many certificates",
                C::RateLimited,
            ),
            ("operation timed out after 300s", C::Timeout),
            ("S3 error: InvalidAccessKeyId", C::Authentication),
            ("AccessDenied: bucket policy", C::PermissionDenied),
            ("manifest unknown: postgres:99", C::ImagePull),
            (
                "tcp connect error: Connection refused (os error 61)",
                C::NetworkConnection,
            ),
            ("A service named db already exists", C::Conflict),
            (
                "NoSuchBucket: the specified bucket does not exist",
                C::NotFound,
            ),
            ("S3 multipart upload aborted at part 3", C::Storage),
            (
                "Validation error: schedule must be a cron expression",
                C::InvalidConfiguration,
            ),
            ("something odd happened", C::Unknown),
            (
                "urn:ietf:params:acme:error:unauthorized: Invalid response from http://example.com/.well-known/acme-challenge/x: 404",
                C::ChallengeValidation,
            ),
            (
                "Error response from daemon: pull access denied for private/app, repository does not exist",
                C::ImagePull,
            ),
            ("request canceled (Client.Timeout exceeded)", C::Timeout),
            ("backup cancelled by user", C::Cancelled),
            ("GitHub API returned status: 404 Not Found", C::NotFound),
            ("No ACME order found for domain: www.example.com", C::NotFound),
            ("upstream responded with status code 429", C::RateLimited),
        ];
        for (message, expected) in cases {
            assert_eq!(
                OperationFailureCode::classify(message),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn failure_event_never_carries_the_message() {
        let secret = "connect to db.internal.example:5432 as admin failed: timed out";
        let event = TelemetryEvent::new(TelemetryEventKind::BackupFailed)
            .with_failure_from_message(secret)
            .with_duration(std::time::Duration::from_secs(75));
        let serialized = serde_json::to_string(&event).unwrap();
        assert_eq!(event.properties["failure_code"], "timeout");
        assert_eq!(
            event.properties["classifier_version"],
            OPERATION_FAILURE_CLASSIFIER_VERSION
        );
        assert_eq!(event.properties["duration_bucket"], "1-5m");
        assert!(!serialized.contains("db.internal"));
        assert!(!serialized.contains("admin"));
    }

    #[test]
    fn duration_buckets_cover_boundaries() {
        use std::time::Duration;
        assert_eq!(duration_bucket(Duration::from_secs(0)), "<10s");
        assert_eq!(duration_bucket(Duration::from_secs(10)), "10s-1m");
        assert_eq!(duration_bucket(Duration::from_secs(60)), "1-5m");
        assert_eq!(duration_bucket(Duration::from_secs(300)), "5-30m");
        assert_eq!(duration_bucket(Duration::from_secs(1800)), "30m-2h");
        assert_eq!(duration_bucket(Duration::from_secs(7200)), ">2h");
    }

    #[test]
    fn noop_reporter_is_disabled_and_silent() {
        let reporter = NoopTelemetryReporter;
        assert!(!reporter.is_enabled());
        // Must not panic.
        reporter.report(TelemetryEvent::new(TelemetryEventKind::ProjectCreated));
    }
}
