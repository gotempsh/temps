//! Builds and sends a deploy-failure report: a redacted, user-editable copy
//! of a failed deployment's build trace, either forwarded to the Temps team
//! or handed to the user to paste into a GitHub issue.
//!
//! Deliberately NOT built on `temps_core::telemetry` — that abstraction's
//! module docs explicitly forbid free-form user text in its payloads (see
//! `crates/temps-core/src/telemetry.rs`). This is a separate, synchronous,
//! always-user-initiated path: the caller gets a real success/failure
//! result (unlike telemetry's fire-and-forget model), and nothing is ever
//! sent without the user reviewing — and being free to edit — the exact
//! text first.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use temps_core::EncryptionService;
use temps_entities::deployment_jobs;
use thiserror::Error;

use super::log_redaction::{redact_common_secret_patterns, redact_known_secrets};
use super::sensitive_envelope;
use super::services::{DeploymentError, DeploymentService};

/// Central endpoint deploy-failure reports are POSTed to. Deliberately a
/// different path from `temps-telemetry`'s `DEFAULT_TELEMETRY_ENDPOINT` --
/// that pipeline never carries free text; this one only ever carries a
/// report the user reviewed and explicitly chose to send. Overridable via
/// `TEMPS_FAILURE_REPORT_ENDPOINT`, mirroring how `TEMPS_TELEMETRY_ENDPOINT`
/// already lets self-hosters point telemetry at their own ingest server.
pub const DEFAULT_FAILURE_REPORT_ENDPOINT: &str =
    "https://telemetry.temps.sh/v1/deploy-failure-reports";

/// How long the outbound report POST is allowed to take. This is a
/// synchronous, user-initiated action (the user is waiting on a "Send"
/// button), so the timeout is longer than telemetry's fire-and-forget one
/// but still bounded.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Fields of `deployment_jobs.job_config` that may hold sensitive maps
/// sealed via `sensitive_envelope::write_sealed`. Not every job type writes
/// every field — `read_sealed_optional` treats a missing field as "none".
const SENSITIVE_JOB_CONFIG_FIELDS: &[&str] = &[
    "environment_variables",
    "remote_environment_variables",
    "secrets",
    "build_args",
    "environment_vars",
];

/// Deployment job errors are often nested chains ("Job execution failed:
/// Failed to build image: Build failed: Build failed: Docker stream error:
/// ..."), which makes an unbounded GitHub issue title unreadable in a PR
/// list or notification. Cap it, breaking at a char boundary since these
/// messages can contain multi-byte output from arbitrary build tools.
const MAX_TITLE_ERROR_CHARS: usize = 80;

fn truncate_for_title(message: &str) -> String {
    let trimmed = message.trim();
    match trimmed.char_indices().nth(MAX_TITLE_ERROR_CHARS) {
        Some((byte_idx, _)) => format!("{}…", &trimmed[..byte_idx]),
        None => trimmed.to_string(),
    }
}

#[derive(Debug, Error)]
pub enum FailureReportError {
    #[error("Job '{job_id}' not found in deployment {deployment_id}")]
    JobNotFound { deployment_id: i32, job_id: String },

    #[error("Failed to read log for job '{job_id}': {reason}")]
    LogRead { job_id: String, reason: String },

    #[error("Failed to build failure-report HTTP client: {reason}")]
    HttpClient { reason: String },

    #[error("Failed to send failure report for deployment {deployment_id}: {reason}")]
    SendFailed { deployment_id: i32, reason: String },

    #[error("Deployment lookup failed: {0}")]
    Deployment(#[from] DeploymentError),

    #[error(
        "Outbound failure reporting is disabled on this instance (TEMPS_TELEMETRY opt-out); \
         no report was sent"
    )]
    ReportingDisabled,
}

/// A redacted, user-editable preview of a failed deployment's trace.
pub struct FailureReportPreview {
    pub redacted_log: String,
    pub error_message: Option<String>,
    /// Whether the "send to Temps" action should be offered at all. Mirrors
    /// the existing `TEMPS_TELEMETRY` opt-out — an operator who disabled all
    /// outbound reporting to Temps shouldn't have this action silently
    /// re-enable it. The GitHub-issue path is unaffected: nothing in that
    /// flow talks to a Temps server, only the user's own browser to GitHub.
    pub reporting_enabled: bool,
    pub failed_job_type: String,
    pub github_issue_title: String,
    pub github_issue_body: String,
}

#[derive(Serialize)]
struct FailureReportPayload<'a> {
    report_text: &'a str,
    temps_version: &'a str,
    project_id: i32,
    deployment_id: i32,
    failed_job_id: &'a str,
    failed_job_type: &'a str,
}

#[derive(Clone)]
pub struct FailureReportService {
    deployment_service: Arc<DeploymentService>,
    log_service: Arc<temps_logs::LogService>,
    encryption_service: Arc<EncryptionService>,
    client: reqwest::Client,
    endpoint: String,
    /// Whether this instance may send reports outbound at all. Read once at
    /// construction from the same `TEMPS_TELEMETRY` opt-out the preview
    /// reports, and enforced in `send_report` so the opt-out holds for direct
    /// API callers, not only for clients that respect the preview's flag.
    reporting_enabled: bool,
}

impl FailureReportService {
    pub fn new(
        deployment_service: Arc<DeploymentService>,
        log_service: Arc<temps_logs::LogService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Result<Self, FailureReportError> {
        let endpoint = std::env::var("TEMPS_FAILURE_REPORT_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_FAILURE_REPORT_ENDPOINT.to_string());

        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .user_agent(format!(
                "temps-failure-report/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|e| FailureReportError::HttpClient {
                reason: e.to_string(),
            })?;

        Ok(Self {
            deployment_service,
            log_service,
            encryption_service,
            client,
            endpoint,
            reporting_enabled: Self::reporting_enabled_from_env(),
        })
    }

    /// Whether the operator has opted out of all outbound reporting to
    /// Temps. Mirrors `temps_telemetry::TelemetryService::enabled_from_env`
    /// exactly (same env var, same accepted opt-out values) rather than
    /// depending on that crate just for this one check.
    fn reporting_enabled_from_env() -> bool {
        match std::env::var("TEMPS_TELEMETRY") {
            Ok(v) => !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "off" | "no" | "disabled"
            ),
            Err(_) => true,
        }
    }

    /// Jobs for `deployment_id`, in execution order, up to and including
    /// `job_id`. Positional truncation (rather than comparing
    /// `execution_order` values, which can be `None`) since
    /// `get_deployment_jobs` already returns jobs ordered by
    /// `execution_order` ascending.
    async fn jobs_up_to(
        &self,
        project_id: i32,
        deployment_id: i32,
        job_id: &str,
    ) -> Result<Vec<deployment_jobs::Model>, FailureReportError> {
        let jobs = self
            .deployment_service
            .get_deployment_jobs(project_id, deployment_id)
            .await?;

        let idx = jobs
            .iter()
            .position(|j| j.job_id == job_id)
            .ok_or_else(|| FailureReportError::JobNotFound {
                deployment_id,
                job_id: job_id.to_string(),
            })?;

        Ok(jobs[..=idx].to_vec())
    }

    /// Every known secret/env value configured for the jobs in `jobs` --
    /// the redaction set for [`log_redaction::redact_known_secrets`].
    fn known_secret_values(&self, jobs: &[deployment_jobs::Model]) -> Vec<String> {
        let mut values = Vec::new();
        for job in jobs {
            let Some(job_config) = job.job_config.as_ref() else {
                continue;
            };
            for field in SENSITIVE_JOB_CONFIG_FIELDS {
                let map = sensitive_envelope::read_sealed_optional(
                    job_config,
                    Some(&self.encryption_service),
                    field,
                )
                .unwrap_or(None)
                .unwrap_or_default();
                values.extend(map.into_values());
            }
        }
        values
    }

    /// Concatenated, redacted plain-text trace for the given failed job:
    /// every job's log up to and including it, each pass through both
    /// redaction layers.
    async fn build_redacted_log(
        &self,
        jobs: &[deployment_jobs::Model],
    ) -> Result<String, FailureReportError> {
        let mut combined = String::new();
        for job in jobs {
            // Not every job type writes a log file -- a job that finishes in
            // a handful of milliseconds without calling `log_service.log_*`
            // (e.g. PrepareSourceBundleJob) never creates one. That's not a
            // failure of this report: treat a missing log file as "no output
            // for this stage" and keep going, rather than aborting the whole
            // preview over one job's silence.
            let content = match self.log_service.get_log_content(&job.log_id).await {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    "(no log output for this stage)".to_string()
                }
                Err(e) => {
                    return Err(FailureReportError::LogRead {
                        job_id: job.job_id.clone(),
                        reason: e.to_string(),
                    })
                }
            };
            combined.push_str(&format!(
                "=== {} ({}) — {:?} ===\n{}\n\n",
                job.name, job.job_id, job.status, content
            ));
        }

        let secrets = self.known_secret_values(jobs);
        let redacted = redact_known_secrets(&combined, &secrets);
        Ok(redact_common_secret_patterns(&redacted))
    }

    /// Build the redacted, editable preview shown to the user before they
    /// choose to send anything.
    pub async fn build_preview(
        &self,
        project_id: i32,
        deployment_id: i32,
        job_id: &str,
    ) -> Result<FailureReportPreview, FailureReportError> {
        let jobs = self.jobs_up_to(project_id, deployment_id, job_id).await?;
        let failed_job = jobs.last().expect("jobs_up_to always returns >= 1 job");
        let failed_job_type = failed_job.job_type.clone();
        let error_message = failed_job.error_message.clone();

        let redacted_log = self.build_redacted_log(&jobs).await?;

        // Only ever included when the project's repo is public -- a private
        // repo's URL/branch must never be pasted into an issue on the
        // public gotempsh/temps tracker.
        let repo_reference = self
            .deployment_service
            .get_public_repo_reference(project_id, deployment_id)
            .await?;

        let github_issue_title = format!(
            "Deploy failure: {}{}",
            failed_job.name,
            error_message
                .as_deref()
                .map(|m| format!(" — {}", truncate_for_title(m)))
                .unwrap_or_default()
        );
        let repo_line = repo_reference
            .map(|r| {
                format!(
                    "**Repository:** https://github.com/{}/{} @ {}\n",
                    r.owner, r.repo, r.branch
                )
            })
            .unwrap_or_default();
        let github_issue_body = format!(
            "**Temps version:** {}\n**Failed stage:** {} ({})\n{}\n\
             Paste your (redacted) deployment log below:\n\n```\n[paste here]\n```\n",
            env!("CARGO_PKG_VERSION"),
            failed_job.name,
            failed_job_type,
            repo_line,
        );

        Ok(FailureReportPreview {
            redacted_log,
            error_message,
            reporting_enabled: Self::reporting_enabled_from_env(),
            failed_job_type,
            github_issue_title,
            github_issue_body,
        })
    }

    /// Send a user-reviewed (and possibly user-edited) report. Re-applies
    /// only the known-secret exact-match pass to whatever the client
    /// submits — defense in depth against an accidentally-reintroduced
    /// literal secret value — but NOT the heuristic pattern pass, which
    /// could otherwise mangle text the user added themselves.
    pub async fn send_report(
        &self,
        project_id: i32,
        deployment_id: i32,
        job_id: &str,
        report_text: &str,
    ) -> Result<(), FailureReportError> {
        // Enforce the opt-out here, not just in the preview. `reporting_enabled`
        // was only used to decide whether the UI offered the action, so an
        // operator who set TEMPS_TELEMETRY=0 to stop all outbound reporting
        // could still have deployment logs POSTed to the central endpoint by
        // anyone with DeploymentsRead calling this route directly. The opt-out
        // has to hold at the point the request would actually leave the box.
        if !self.reporting_enabled {
            return Err(FailureReportError::ReportingDisabled);
        }

        let jobs = self.jobs_up_to(project_id, deployment_id, job_id).await?;
        let failed_job = jobs.last().expect("jobs_up_to always returns >= 1 job");
        let secrets = self.known_secret_values(&jobs);
        let safe_text = redact_known_secrets(report_text, &secrets);

        let payload = FailureReportPayload {
            report_text: &safe_text,
            temps_version: env!("CARGO_PKG_VERSION"),
            project_id,
            deployment_id,
            failed_job_id: &failed_job.job_id,
            failed_job_type: &failed_job.job_type,
        };

        let response = self
            .client
            .post(&self.endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|e| FailureReportError::SendFailed {
                deployment_id,
                reason: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(FailureReportError::SendFailed {
                deployment_id,
                reason: format!("central endpoint returned {}", response.status()),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporting_enabled_defaults_on_and_honors_opt_out() {
        std::env::remove_var("TEMPS_TELEMETRY");
        assert!(FailureReportService::reporting_enabled_from_env());

        std::env::set_var("TEMPS_TELEMETRY", "0");
        assert!(!FailureReportService::reporting_enabled_from_env());
        std::env::remove_var("TEMPS_TELEMETRY");
    }

    /// The opt-out has to be enforced where the request would leave the box,
    /// not only where the UI decides whether to offer the button. Before this,
    /// TEMPS_TELEMETRY=0 only cleared a flag in the preview response, so any
    /// caller with DeploymentsRead could still POST deployment logs to the
    /// central endpoint by calling the send route directly.
    #[test]
    fn send_report_is_refused_when_reporting_is_disabled() {
        // The construction path reads the same env var the preview reports, so
        // an instance built under the opt-out carries reporting_enabled = false.
        std::env::set_var("TEMPS_TELEMETRY", "0");
        let disabled = FailureReportService::reporting_enabled_from_env();
        std::env::remove_var("TEMPS_TELEMETRY");
        assert!(!disabled);

        // And that state maps to a refusal the client can render, not a 500.
        let problem: temps_core::problemdetails::Problem =
            FailureReportError::ReportingDisabled.into();
        use axum::response::IntoResponse;
        assert_eq!(
            problem.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn truncate_for_title_passes_short_messages_through() {
        assert_eq!(
            truncate_for_title("build failed: exit code 1"),
            "build failed: exit code 1"
        );
    }

    #[test]
    fn truncate_for_title_caps_long_nested_error_chains() {
        let message = "Job execution failed: Failed to build image: Build failed: Build failed: \
             Docker stream error: process \"/bin/sh -c pnpm install --frozen-lockfile\" did not \
             complete successfully: exit code: 127";
        let truncated = truncate_for_title(message);
        assert!(truncated.chars().count() <= MAX_TITLE_ERROR_CHARS + 1);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_for_title_trims_whitespace() {
        assert_eq!(truncate_for_title("  padded  "), "padded");
    }
}
