//! Shared types for the Lighthouse Performance Audit plugin.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ============================================================================
// Settings
// ============================================================================

/// Plugin-level configuration persisted in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PluginSettings {
    /// Whether to auto-run audits on deployment events.
    pub auto_audit_on_deploy: bool,
    /// Lighthouse categories to audit (performance, accessibility, best-practices, seo).
    pub categories: Vec<String>,
    /// Score threshold — scores below this trigger a warning.
    pub score_threshold: u32,
    /// Lighthouse CLI timeout in seconds.
    pub timeout_secs: u64,
    /// Chrome flags to pass to Lighthouse.
    pub chrome_flags: String,
    /// Device emulation: "mobile" or "desktop".
    pub device: String,
}

impl PluginSettings {
    pub const DEFAULT_AUTO_AUDIT: bool = true;
    pub const DEFAULT_CATEGORIES: &'static [&'static str] =
        &["performance", "accessibility", "best-practices", "seo"];
    pub const DEFAULT_SCORE_THRESHOLD: u32 = 80;
    pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
    pub const DEFAULT_CHROME_FLAGS: &'static str = "--headless --no-sandbox --disable-gpu";
    pub const DEFAULT_DEVICE: &'static str = "mobile";
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            auto_audit_on_deploy: Self::DEFAULT_AUTO_AUDIT,
            categories: Self::DEFAULT_CATEGORIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            score_threshold: Self::DEFAULT_SCORE_THRESHOLD,
            timeout_secs: Self::DEFAULT_TIMEOUT_SECS,
            chrome_flags: Self::DEFAULT_CHROME_FLAGS.to_string(),
            device: Self::DEFAULT_DEVICE.to_string(),
        }
    }
}

/// Partial update for plugin settings — only `Some` fields are written.
#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
pub struct UpdateSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_audit_on_deploy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chrome_flags: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

// ============================================================================
// Audits
// ============================================================================

/// Summary of an audit (returned in list view).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditSummary {
    pub id: String,
    pub url: String,
    pub performance_score: Option<u32>,
    pub accessibility_score: Option<u32>,
    pub best_practices_score: Option<u32>,
    pub seo_score: Option<u32>,
    pub status: AuditStatus,
    pub trigger: AuditTrigger,
    pub project_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub device: String,
    pub created_at: String,
    pub duration_ms: u64,
}

/// Full Lighthouse audit result with category details and diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LighthouseAudit {
    pub id: String,
    pub url: String,
    pub performance_score: Option<u32>,
    pub accessibility_score: Option<u32>,
    pub best_practices_score: Option<u32>,
    pub seo_score: Option<u32>,
    pub status: AuditStatus,
    pub trigger: AuditTrigger,
    pub project_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Core Web Vitals metrics
    pub metrics: Option<CoreWebVitals>,
    /// Key audit diagnostics and opportunities
    pub diagnostics: Vec<AuditDiagnostic>,
    /// Raw Lighthouse JSON output (stored compressed, returned on demand)
    pub raw_json_available: bool,
    pub error_message: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: u64,
    pub device: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditTrigger {
    /// Triggered automatically by a deployment event
    Deployment,
    /// Triggered manually via the API/UI
    Manual,
}

/// Core Web Vitals extracted from Lighthouse results.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoreWebVitals {
    /// Largest Contentful Paint (milliseconds)
    pub lcp_ms: Option<f64>,
    /// First Contentful Paint (milliseconds)
    pub fcp_ms: Option<f64>,
    /// Total Blocking Time (milliseconds)
    pub tbt_ms: Option<f64>,
    /// Cumulative Layout Shift (unitless)
    pub cls: Option<f64>,
    /// Speed Index (milliseconds)
    pub speed_index_ms: Option<f64>,
    /// Time to Interactive (milliseconds)
    pub tti_ms: Option<f64>,
}

/// A single diagnostic or opportunity from the Lighthouse audit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditDiagnostic {
    /// Audit ID from Lighthouse (e.g., "render-blocking-resources")
    pub id: String,
    /// Human-readable title
    pub title: String,
    /// Score for this audit (0-1, None if not applicable)
    pub score: Option<f64>,
    /// Potential savings description (e.g., "Potential savings of 1.2 s")
    pub savings: Option<String>,
    /// Severity classification
    pub severity: DiagnosticSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Score = 0 to 0.49
    Critical,
    /// Score = 0.5 to 0.89
    Warning,
    /// Score = 0.9 to 1.0 or informational
    Info,
    /// Passed audit
    Pass,
}

// ============================================================================
// Request types
// ============================================================================

/// Request body for starting a manual audit.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AuditRequest {
    pub url: String,
    /// Override device emulation for this audit.
    pub device: Option<String>,
    /// Override categories for this audit.
    pub categories: Option<Vec<String>>,
}

/// Response body returned when a new audit is started.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StartAuditResponse {
    pub id: String,
    pub status: String,
    pub message: String,
}

/// Lighthouse CLI availability status.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatusResponse {
    pub lighthouse_available: bool,
}

// ============================================================================
// Score history for charts
// ============================================================================

/// A data point for the score timeline chart.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScoreHistoryPoint {
    pub id: String,
    pub performance_score: Option<u32>,
    pub accessibility_score: Option<u32>,
    pub best_practices_score: Option<u32>,
    pub seo_score: Option<u32>,
    pub created_at: String,
    pub trigger: AuditTrigger,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = PluginSettings::default();
        assert!(settings.auto_audit_on_deploy);
        assert_eq!(settings.categories.len(), 4);
        assert_eq!(settings.score_threshold, 80);
        assert_eq!(settings.device, "mobile");
    }

    #[test]
    fn test_settings_serialization_roundtrip() {
        let settings = PluginSettings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: PluginSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.categories.len(), 4);
        assert_eq!(deserialized.score_threshold, 80);
    }

    #[test]
    fn test_audit_status_serialization() {
        let json = serde_json::to_string(&AuditStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
        let json = serde_json::to_string(&AuditStatus::Completed).unwrap();
        assert_eq!(json, "\"completed\"");
    }

    #[test]
    fn test_audit_trigger_serialization() {
        let json = serde_json::to_string(&AuditTrigger::Deployment).unwrap();
        assert_eq!(json, "\"deployment\"");
        let json = serde_json::to_string(&AuditTrigger::Manual).unwrap();
        assert_eq!(json, "\"manual\"");
    }

    #[test]
    fn test_core_web_vitals_serialization() {
        let cwv = CoreWebVitals {
            lcp_ms: Some(2500.0),
            fcp_ms: Some(1800.0),
            tbt_ms: Some(300.0),
            cls: Some(0.1),
            speed_index_ms: Some(3000.0),
            tti_ms: Some(4500.0),
        };
        let json = serde_json::to_string(&cwv).unwrap();
        let deserialized: CoreWebVitals = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.lcp_ms, Some(2500.0));
        assert_eq!(deserialized.cls, Some(0.1));
    }

    #[test]
    fn test_diagnostic_severity_serialization() {
        let json = serde_json::to_string(&DiagnosticSeverity::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
    }
}
