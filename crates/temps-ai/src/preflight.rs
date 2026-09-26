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
