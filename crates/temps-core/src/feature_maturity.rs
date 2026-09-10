// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build-time registry for the compatibility promise of user-facing features.
//!
//! Stable features intentionally render without a badge. Beta and
//! Experimental entries are exposed to the console, generated OpenAPI, and
//! CLI so every surface communicates the same promise.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const FEATURE_MATURITY_DOCS_PATH: &str = "https://temps.sh/docs/feature-maturity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Maturity {
    Stable,
    Beta,
    Experimental,
}

impl Maturity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Experimental => "experimental",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FeatureMaturity {
    pub key: &'static str,
    pub maturity: Maturity,
    pub reason: &'static str,
    pub docs_path: &'static str,
}

const STABLE_REASON: &str =
    "Verified end to end. Its public surface will remain compatible within 0.1.x.";

const fn feature(key: &'static str, maturity: Maturity, reason: &'static str) -> FeatureMaturity {
    FeatureMaturity {
        key,
        maturity,
        reason,
        docs_path: FEATURE_MATURITY_DOCS_PATH,
    }
}

pub const FEATURE_MATURITY: &[FeatureMaturity] = &[
    feature("deployments", Maturity::Stable, STABLE_REASON),
    feature("projects-environments", Maturity::Stable, STABLE_REASON),
    feature("environment-variables", Maturity::Stable, STABLE_REASON),
    feature("managed-services", Maturity::Stable, STABLE_REASON),
    feature("backups", Maturity::Stable, STABLE_REASON),
    feature("proxy", Maturity::Stable, STABLE_REASON),
    feature("domains", Maturity::Stable, STABLE_REASON),
    feature("authentication", Maturity::Stable, STABLE_REASON),
    feature("api-keys-rbac", Maturity::Stable, STABLE_REASON),
    feature("logs", Maturity::Stable, STABLE_REASON),
    feature("transactional-email", Maturity::Stable, STABLE_REASON),
    feature("uptime-monitoring", Maturity::Stable, STABLE_REASON),
    feature("blob-storage", Maturity::Stable, STABLE_REASON),
    feature("kv-store", Maturity::Stable, STABLE_REASON),
    feature("feature-flags", Maturity::Stable, STABLE_REASON),
    feature("audit-logs", Maturity::Stable, STABLE_REASON),
    feature("cli", Maturity::Stable, STABLE_REASON),
    feature("console-core", Maturity::Stable, STABLE_REASON),
    feature(
        "service-template-catalog",
        Maturity::Beta,
        "Native curated service templates currently support one application container with Temps-managed service bindings; multi-container template workloads are not available yet.",
    ),
    feature(
        "web-analytics",
        Maturity::Beta,
        "The analytics data model and query implementation are still being hardened.",
    ),
    feature(
        "session-replay",
        Maturity::Beta,
        "Session replay is functional but its unit coverage is still growing.",
    ),
    feature(
        "error-tracking",
        Maturity::Beta,
        "Event fidelity and ingest quota behavior are still being hardened.",
    ),
    feature(
        "otel-traces-metrics",
        Maturity::Beta,
        "OpenTelemetry volume controls and storage behavior may still change.",
    ),
    feature(
        "funnels",
        Maturity::Beta,
        "Funnels do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "performance-monitoring",
        Maturity::Beta,
        "Performance monitoring has limited coverage and no dedicated end-to-end scenario.",
    ),
    feature(
        "dashboards-metrics-explorer",
        Maturity::Beta,
        "Dashboards and the metrics explorer do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "alerts-metric-alerts",
        Maturity::Beta,
        "Alert rules and metric alerts do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "status-pages",
        Maturity::Beta,
        "Status pages do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "screenshots",
        Maturity::Beta,
        "Screenshots do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "notifications-webhooks",
        Maturity::Beta,
        "Notifications and webhooks do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "email-tracking",
        Maturity::Beta,
        "Email tracking does not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "teams",
        Maturity::Beta,
        "Teams have partial RBAC coverage but no dedicated end-to-end scenario.",
    ),
    feature(
        "database-browser",
        Maturity::Beta,
        "The database browser reads live service data and does not yet have end-to-end coverage.",
    ),
    feature(
        "competitor-importers",
        Maturity::Beta,
        "Importer compatibility may change and is not yet covered end to end.",
    ),
    feature(
        "multi-node-worker-join",
        Maturity::Beta,
        "Worker join is tested end to end, but multi-node hardening remains in progress.",
    ),
    feature(
        "ip-access-geo",
        Maturity::Beta,
        "IP access and geo controls do not yet have dedicated end-to-end coverage.",
    ),
    feature(
        "ai-chat",
        Maturity::Experimental,
        "The AI chat surface and interaction model are still being designed.",
    ),
    feature(
        "ai-agents-workflows",
        Maturity::Experimental,
        "The agent execution model is in active development and has no end-to-end coverage.",
    ),
    feature(
        "ai-gateway",
        Maturity::Experimental,
        "Rate limiting and cost attribution are not yet complete.",
    ),
    feature(
        "ai-foundation-api-tools",
        Maturity::Experimental,
        "The AI foundation and API tool contracts are not yet frozen.",
    ),
    feature(
        "embeddings-memory",
        Maturity::Experimental,
        "Embeddings and memory have minimal coverage and an evolving contract.",
    ),
    feature(
        "agent-execution-sandbox",
        Maturity::Experimental,
        "The agent execution sandbox has minimal coverage and may change substantially.",
    ),
    feature(
        "sandboxes-preview-environments",
        Maturity::Experimental,
        "Persistent preview workspace semantics are not yet complete.",
    ),
    feature(
        "database-ha-failover",
        Maturity::Experimental,
        "Database failover semantics are not yet hardened.",
    ),
    feature(
        "revenue-tracking",
        Maturity::Experimental,
        "Revenue tracking has no dedicated end-to-end coverage.",
    ),
    feature(
        "plugin-system",
        Maturity::Experimental,
        "The plugin SDK boundary is still moving.",
    ),
    feature(
        "vulnerability-scanning",
        Maturity::Experimental,
        "Vulnerability scanning has minimal coverage and may change substantially.",
    ),
    feature(
        "attack-mode-captcha",
        Maturity::Experimental,
        "Attack-mode CAPTCHA has minimal coverage.",
    ),
    feature(
        "static-file-hosting",
        Maturity::Experimental,
        "Static file hosting does not yet have automated test coverage.",
    ),
];

pub fn feature_maturity(key: &str) -> Option<&'static FeatureMaturity> {
    FEATURE_MATURITY.iter().find(|feature| feature.key == key)
}

/// Resolve labeled API paths to the same feature keys used by the console.
/// Stable operations return `None` because `x-maturity` is intentionally only
/// emitted where callers need a compatibility warning.
pub fn feature_key_for_api_path(path: &str) -> Option<&'static str> {
    // Resolve specific products before broad families such as `/metrics`.
    // Several product APIs expose metric-shaped resources without adopting
    // the generic telemetry feature's maturity promise.
    if path.starts_with("/revenue") || path.contains("/revenue/") {
        return Some("revenue-tracking");
    }
    if path.contains("/service-template") || path.contains("/service-runtime") {
        return Some("service-template-catalog");
    }
    if path.starts_with("/otel/genai") {
        return Some("ai-agents-workflows");
    }
    if path.contains("session-replay") || path.starts_with("/session-replays") {
        return Some("session-replay");
    }
    if path.contains("/funnels") {
        return Some("funnels");
    }
    if path.starts_with("/analytics")
        || path.contains("/api-analytics")
        || path.contains("projects-analytics")
        || path.contains("active-visitors")
        || path.contains("ai-agent-pages")
        || path.contains("ai-pages")
    {
        return Some("web-analytics");
    }
    if path.contains("alert-rules") || path.starts_with("/alerts") {
        return Some("alerts-metric-alerts");
    }
    if path.contains("/error-") || path.contains("error-groups") {
        return Some("error-tracking");
    }
    if path.starts_with("/otel/") {
        return Some("otel-traces-metrics");
    }
    if path.starts_with("/performance") {
        return Some("performance-monitoring");
    }
    if path.starts_with("/dashboards") {
        return Some("dashboards-metrics-explorer");
    }
    if path.starts_with("/status-pages") || path.starts_with("/incidents") {
        return Some("status-pages");
    }
    if path.starts_with("/screenshots") {
        return Some("screenshots");
    }
    if path.starts_with("/notification-") || path.contains("/webhooks") {
        return Some("notifications-webhooks");
    }
    if path == "/emails/events"
        || path == "/emails/events/stats"
        || path.starts_with("/emails/{email_id}/events")
        || path.starts_with("/emails/{email_id}/track/")
        || path.starts_with("/emails/{id}/tracking")
        || path.starts_with("/email-providers/{id}/tracking/")
        || path.starts_with("/t/pixel/")
        || path.starts_with("/t/click/")
    {
        return Some("email-tracking");
    }
    if path.starts_with("/teams") || path.contains("/access/{team_id}") {
        return Some("teams");
    }
    if path.contains("/query/") {
        return Some("database-browser");
    }
    if path.starts_with("/imports") || path.starts_with("/external-services/import") {
        return Some("competitor-importers");
    }
    // Node runtime metrics are part of stable monitoring; only node-control
    // and worker-join surfaces inherit the multi-node maturity warning.
    if path == "/nodes/{id}/metrics" {
        return None;
    }
    if path.starts_with("/nodes") {
        return Some("multi-node-worker-join");
    }
    if path.contains("ip-access") || path.contains("geolocation") {
        return Some("ip-access-geo");
    }
    if path.starts_with("/chat") {
        return Some("ai-chat");
    }
    if path.contains("/agents") {
        return Some("ai-agents-workflows");
    }
    if path.starts_with("/ai/") || path.starts_with("/ai-gateway") {
        return Some("ai-gateway");
    }
    if path.starts_with("/embeddings") || path.starts_with("/memory") {
        return Some("embeddings-memory");
    }
    if path.starts_with("/v1/sandboxes") || path.starts_with("/v1/sandbox-snapshots") {
        return Some("sandboxes-preview-environments");
    }
    if path.starts_with("/plugins") || path.starts_with("/external-plugins") {
        return Some("plugin-system");
    }
    if path.contains("vulnerability-scan") {
        return Some("vulnerability-scanning");
    }
    if path.contains("captcha") || path.contains("challenge-token") {
        return Some("attack-mode-captcha");
    }
    if path.contains("/static-bundles") || path.ends_with("/deploy/static") {
        return Some("static-file-hosting");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_keys_are_unique_and_documented() {
        let mut keys = HashSet::new();
        for entry in FEATURE_MATURITY {
            assert!(
                keys.insert(entry.key),
                "duplicate feature key: {}",
                entry.key
            );
            assert!(
                !entry.reason.trim().is_empty(),
                "missing reason: {}",
                entry.key
            );
            assert_eq!(entry.docs_path, FEATURE_MATURITY_DOCS_PATH);
        }
    }

    #[test]
    fn registry_contains_each_maturity_tier() {
        assert!(FEATURE_MATURITY
            .iter()
            .any(|entry| entry.maturity == Maturity::Stable));
        assert!(FEATURE_MATURITY
            .iter()
            .any(|entry| entry.maturity == Maturity::Beta));
        assert!(FEATURE_MATURITY
            .iter()
            .any(|entry| entry.maturity == Maturity::Experimental));
    }

    #[test]
    fn lookup_returns_the_canonical_entry() {
        let analytics = feature_maturity("web-analytics").expect("web analytics is registered");
        assert_eq!(analytics.maturity, Maturity::Beta);
        assert!(feature_maturity("mcp-servers").is_none());
    }

    #[test]
    fn api_path_mapping_prefers_specific_features() {
        assert_eq!(
            feature_key_for_api_path("/projects/{project_id}/funnels/{funnel_id}"),
            Some("funnels")
        );
        assert_eq!(
            feature_key_for_api_path("/_temps/session-replay/events"),
            Some("session-replay")
        );
        assert_eq!(
            feature_key_for_api_path("/projects/{project_id}/revenue/metrics/mrr"),
            Some("revenue-tracking")
        );
        assert_eq!(
            feature_key_for_api_path("/projects/{project_id}/has-error-groups"),
            Some("error-tracking")
        );
        assert_eq!(
            feature_key_for_api_path("/projects/{project_id}/service-template/upgrade"),
            Some("service-template-catalog")
        );
        assert_eq!(
            feature_key_for_api_path("/projects/{project_id}/service-runtime"),
            Some("service-template-catalog")
        );
        assert_eq!(feature_key_for_api_path("/projects"), None);
    }

    #[test]
    fn test_feature_key_for_api_path_stable_runtime_metrics_remain_stable() {
        for path in [
            "/deployments/{id}/metrics",
            "/deployments/{id}/metrics/latest",
            "/nodes/{id}/metrics",
            "/external-services/{id}/metrics",
            "/external-services/{id}/metrics/latest",
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics",
        ] {
            assert_eq!(
                feature_key_for_api_path(path),
                None,
                "stable runtime metrics path must not receive an x-maturity warning: {path}"
            );
        }
    }

    #[test]
    fn test_feature_key_for_api_path_email_tracking_receives_beta_maturity() {
        for path in [
            "/emails/events",
            "/emails/events/stats",
            "/emails/{email_id}/events",
            "/emails/{email_id}/track/open",
            "/emails/{id}/tracking",
            "/email-providers/{id}/tracking/status",
            "/t/pixel/{token}",
            "/t/click/{token}",
        ] {
            let feature_key = feature_key_for_api_path(path);
            assert_eq!(feature_key, Some("email-tracking"), "path: {path}");
            assert_eq!(
                feature_key
                    .and_then(feature_maturity)
                    .map(|entry| entry.maturity),
                Some(Maturity::Beta),
                "email tracking path must carry the beta compatibility warning: {path}"
            );
        }
    }
}
