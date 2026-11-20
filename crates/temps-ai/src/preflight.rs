// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessCheckStatus {
    Passed,
    Warning,
    Failed,
    NotTested,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessCheckMode {
    Preflight,
    Smoke,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessCheckOverall {
    Passed,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct HarnessCheck {
    pub id: String,
    pub label: String,
    pub status: HarnessCheckStatus,
    pub detail: String,
    pub action: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct HarnessCheckReport {
    pub provider_id: String,
    pub mode: HarnessCheckMode,
    pub overall: HarnessCheckOverall,
    pub checked_at: String,
    pub diagnostic_id: String,
    pub checks: Vec<HarnessCheck>,
}

impl HarnessCheckReport {
    pub fn calculate_overall(checks: &[HarnessCheck]) -> HarnessCheckOverall {
        if checks
            .iter()
            .any(|check| check.status == HarnessCheckStatus::Failed)
        {
            HarnessCheckOverall::Failed
        } else if checks.iter().any(|check| {
            matches!(
                check.status,
                HarnessCheckStatus::Warning | HarnessCheckStatus::NotTested
            )
        }) {
            HarnessCheckOverall::Warning
        } else {
            HarnessCheckOverall::Passed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_preserves_failed_and_unchecked_states() {
        let check = |status| HarnessCheck {
            id: "runtime".into(),
            label: "Runtime".into(),
            status,
            detail: "safe".into(),
            action: None,
            duration_ms: 1,
        };
        assert_eq!(
            HarnessCheckReport::calculate_overall(&[check(HarnessCheckStatus::Passed)]),
            HarnessCheckOverall::Passed
        );
        assert_eq!(
            HarnessCheckReport::calculate_overall(&[check(HarnessCheckStatus::NotTested)]),
            HarnessCheckOverall::Warning
        );
        assert_eq!(
            HarnessCheckReport::calculate_overall(&[check(HarnessCheckStatus::Failed)]),
            HarnessCheckOverall::Failed
        );
    }

    #[test]
    fn report_serializes_stable_api_values_without_diagnostic_material() {
        let report = HarnessCheckReport {
            provider_id: "codex_cli".into(),
            mode: HarnessCheckMode::Preflight,
            overall: HarnessCheckOverall::Warning,
            checked_at: "2026-01-01T00:00:00Z".into(),
            diagnostic_id: "diagnostic-1".into(),
            checks: vec![HarnessCheck {
                id: "relay".into(),
                label: "Model relay".into(),
                status: HarnessCheckStatus::NotTested,
                detail: "No provider call was made.".into(),
                action: None,
                duration_ms: 0,
            }],
        };
        let value = serde_json::to_value(report).expect("serialize report");
        assert_eq!(value["mode"], "preflight");
        assert_eq!(value["overall"], "warning");
        assert_eq!(value["checks"][0]["status"], "not_tested");
    }
}

/// Installed and latest version of a provider's harness CLI inside the
/// caller's persistent workspace.
///
/// A harness that is behind the latest release cannot offer models released
/// after it, so the console shows this and lets the operator update it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct HarnessUpdateStatus {
    pub provider_id: String,
    /// Version reported by the CLI in the workspace, when it could be read.
    pub installed_version: Option<String>,
    /// Latest version published for the CLI, when it could be looked up.
    pub latest_version: Option<String>,
    /// True only when both versions are known and `latest_version` is newer.
    pub update_available: bool,
    /// An update is running right now.
    pub updating: bool,
    /// When the versions were last read from the workspace (RFC 3339).
    pub checked_at: Option<String>,
    /// Why the versions could not be determined, when they could not.
    pub detail: Option<String>,
}
