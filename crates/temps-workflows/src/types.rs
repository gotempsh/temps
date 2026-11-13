// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Run and step states, the effective per-environment settings, and the
//! validation of names and keys (plaintext metadata, ADR-103 §5).

use serde::{Deserialize, Serialize};
use temps_entities::workflow_environment_settings;
use utoipa::ToSchema;

use crate::error::{WorkflowError, WorkflowResult};

/// Where a run is in its lifecycle (ADR-103 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WorkflowRunStatus)]
pub enum RunStatus {
    /// Due now, waiting for a free invoker or under a concurrency limit.
    Queued,
    /// Leased to an invocation.
    Running,
    /// Suspended in `sleep` until `due_at`.
    Sleeping,
    /// Suspended in `waitForEvent`; `due_at` is the timeout, if any.
    Waiting,
    /// A step failed and is retried at `due_at`.
    Retrying,
    Completed,
    Failed,
    Cancelled,
    /// Repeated invocation failures or lease expiries; resumed by an operator.
    Stuck,
}

impl RunStatus {
    pub const ALL: [RunStatus; 9] = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::Sleeping,
        RunStatus::Waiting,
        RunStatus::Retrying,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::Stuck,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Sleeping => "sleeping",
            RunStatus::Waiting => "waiting",
            RunStatus::Retrying => "retrying",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Stuck => "stuck",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }

    /// No further invocation happens without an operator action.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    }

    /// Counted against `max_active_runs`.
    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }
}

/// What a journal entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WorkflowStepKind)]
pub enum StepKind {
    Step,
    Sleep,
    Wait,
    Now,
    Random,
    SendEvent,
    /// A child run started by the workflow, completed by Temps with the
    /// child's status and output when it finishes.
    Child,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Step => "step",
            StepKind::Sleep => "sleep",
            StepKind::Wait => "wait",
            StepKind::Now => "now",
            StepKind::Random => "random",
            StepKind::SendEvent => "send_event",
            StepKind::Child => "child",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            StepKind::Step,
            StepKind::Sleep,
            StepKind::Wait,
            StepKind::Now,
            StepKind::Random,
            StepKind::SendEvent,
            StepKind::Child,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
    }
}

/// A journal entry's state. `pending` is a sleep or wait not yet over, or a
/// step whose failed attempt is waiting for its retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WorkflowStepStatus)]
pub enum StepStatus {
    Pending,
    Completed,
    Failed,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Completed => "completed",
            StepStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            StepStatus::Pending,
            StepStatus::Completed,
            StepStatus::Failed,
        ]
        .into_iter()
        .find(|status| status.as_str() == value)
    }

    pub fn is_final(self) -> bool {
        !matches!(self, StepStatus::Pending)
    }
}

/// The settings in effect for an environment: its row, or the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkflowSettings {
    /// Compose service hosting the workflow handler; `None`: the main service.
    pub host_service: Option<String>,
    pub invocation_deadline_secs: i32,
    pub lease_secs: i32,
    pub max_concurrent_runs: i32,
    pub max_active_runs: i32,
    pub max_steps_per_run: i32,
    pub max_journal_bytes_per_run: i64,
    pub inline_payload_limit_bytes: i32,
    pub max_payload_bytes: i32,
    pub retention_days: i32,
    pub stuck_after_failures: i32,
}

impl Default for WorkflowSettings {
    /// Mirrors the column defaults of `workflow_environment_settings`.
    fn default() -> Self {
        Self {
            host_service: None,
            invocation_deadline_secs: 900,
            lease_secs: 120,
            max_concurrent_runs: 50,
            max_active_runs: 10_000,
            max_steps_per_run: 1_000,
            max_journal_bytes_per_run: 64 * 1024 * 1024,
            inline_payload_limit_bytes: 256 * 1024,
            max_payload_bytes: 4 * 1024 * 1024,
            retention_days: 30,
            stuck_after_failures: 8,
        }
    }
}

impl From<&workflow_environment_settings::Model> for WorkflowSettings {
    fn from(row: &workflow_environment_settings::Model) -> Self {
        Self {
            host_service: row.host_service.clone(),
            invocation_deadline_secs: row.invocation_deadline_secs,
            lease_secs: row.lease_secs,
            max_concurrent_runs: row.max_concurrent_runs,
            max_active_runs: row.max_active_runs,
            max_steps_per_run: row.max_steps_per_run,
            max_journal_bytes_per_run: row.max_journal_bytes_per_run,
            inline_payload_limit_bytes: row.inline_payload_limit_bytes,
            max_payload_bytes: row.max_payload_bytes,
            retention_days: row.retention_days,
            stuck_after_failures: row.stuck_after_failures,
        }
    }
}

impl WorkflowSettings {
    /// The same ranges as the table's CHECK constraints, reported per field.
    pub fn validate(&self) -> WorkflowResult<()> {
        fn range(field: &'static str, value: i64, min: i64, max: i64) -> WorkflowResult<()> {
            if value < min || value > max {
                return Err(WorkflowError::Validation {
                    field,
                    reason: format!("{value} is outside {min}..={max}"),
                });
            }
            Ok(())
        }
        if let Some(service) = &self.host_service {
            validate_name("host_service", service, 128)?;
        }
        range(
            "invocation_deadline_secs",
            self.invocation_deadline_secs.into(),
            10,
            3600,
        )?;
        range("lease_secs", self.lease_secs.into(), 15, 900)?;
        range(
            "max_concurrent_runs",
            self.max_concurrent_runs.into(),
            1,
            10_000,
        )?;
        range("max_active_runs", self.max_active_runs.into(), 1, 1_000_000)?;
        range(
            "max_steps_per_run",
            self.max_steps_per_run.into(),
            1,
            100_000,
        )?;
        range(
            "max_journal_bytes_per_run",
            self.max_journal_bytes_per_run,
            1024,
            1024 * 1024 * 1024,
        )?;
        range(
            "inline_payload_limit_bytes",
            self.inline_payload_limit_bytes.into(),
            1024,
            MAX_INLINE_LIMIT_BYTES.into(),
        )?;
        range(
            "max_payload_bytes",
            self.max_payload_bytes.into(),
            1024,
            MAX_PAYLOAD_LIMIT_BYTES.into(),
        )?;
        if self.max_payload_bytes < self.inline_payload_limit_bytes {
            return Err(WorkflowError::Validation {
                field: "max_payload_bytes",
                reason: "must be at least inline_payload_limit_bytes".into(),
            });
        }
        range("retention_days", self.retention_days.into(), 1, 3650)?;
        range(
            "stuck_after_failures",
            self.stuck_after_failures.into(),
            1,
            100,
        )?;
        Ok(())
    }
}

/// The largest `max_payload_bytes` an environment may set.
pub const MAX_PAYLOAD_LIMIT_BYTES: i32 = 64 * 1024 * 1024;
/// The largest `inline_payload_limit_bytes` an environment may set.
pub const MAX_INLINE_LIMIT_BYTES: i32 = 1024 * 1024;

/// The longest event name, in characters (the `workflow_events.name` CHECK).
pub const MAX_EVENT_NAME: usize = 200;

/// Validate a workflow, step or event name: 1..=`max` characters of
/// `[A-Za-z0-9._:/-]`. Names are plaintext metadata (labels, URLs-free
/// identifiers); the restricted alphabet keeps them out of HTML, SQL and log
/// injection paths and makes accidental personal data (spaces, `@`) visible.
pub fn validate_name(field: &'static str, value: &str, max: usize) -> WorkflowResult<()> {
    if value.is_empty() || value.chars().count() > max {
        return Err(WorkflowError::Validation {
            field,
            reason: format!("must be 1 to {max} characters"),
        });
    }
    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/')))
    {
        return Err(WorkflowError::Validation {
            field,
            reason: format!("character {bad:?} is not allowed; use letters, digits and . _ - : /"),
        });
    }
    Ok(())
}

/// Validate an idempotency or event key: 1..=256 printable characters with no
/// control characters. Keys may be opaque values chosen by the app, so the
/// alphabet is wider than names'; they must still not carry personal data.
pub fn validate_key(field: &'static str, value: &str) -> WorkflowResult<()> {
    if value.is_empty() || value.chars().count() > 256 {
        return Err(WorkflowError::Validation {
            field,
            reason: "must be 1 to 256 characters".into(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(WorkflowError::Validation {
            field,
            reason: "must not contain control characters".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_round_trip_and_classify() {
        for status in RunStatus::ALL {
            assert_eq!(RunStatus::parse(status.as_str()), Some(status));
        }
        assert!(RunStatus::Completed.is_terminal());
        assert!(!RunStatus::Stuck.is_terminal());
        assert!(RunStatus::Waiting.is_active());
        assert_eq!(RunStatus::parse("paused"), None);
        assert_eq!(StepKind::parse("send_event"), Some(StepKind::SendEvent));
        assert_eq!(StepStatus::parse("pending"), Some(StepStatus::Pending));
    }

    #[test]
    fn defaults_are_valid_and_match_the_table() {
        let defaults = WorkflowSettings::default();
        defaults.validate().unwrap();
        assert_eq!(defaults.invocation_deadline_secs, 900);
        assert_eq!(defaults.inline_payload_limit_bytes, 262_144);
    }

    #[test]
    fn settings_reject_out_of_range_values_by_field() {
        let mut settings = WorkflowSettings {
            lease_secs: 5,
            ..WorkflowSettings::default()
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("lease_secs"), "{error}");
        settings.lease_secs = 60;
        settings.max_payload_bytes = 2048;
        settings.inline_payload_limit_bytes = 4096;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn names_allow_identifiers_and_reject_the_rest() {
        validate_name("workflow", "billing/dunning.v2", 128).unwrap();
        assert!(validate_name("workflow", "", 128).is_err());
        assert!(validate_name("workflow", "user@example.com", 128).is_err());
        assert!(validate_name("workflow", "has space", 128).is_err());
        assert!(validate_name("workflow", &"a".repeat(129), 128).is_err());
        validate_key("idempotency_key", "order:123 retry").unwrap();
        assert!(validate_key("idempotency_key", "line\nbreak").is_err());
    }
}
