// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The purchase-triggered entry point onto the bulk activation engine
//! (ADR-042 §1a, P3).
//!
//! One implementation of [`CloudTelemetryActivationTrigger`], called by
//! `POST /cloud/enroll` and by nothing else. It does what the operator path's
//! `POST /bulk-jobs/estimate` + `POST /bulk-jobs` pair does, minus the
//! confirmation step:
//!
//! | | operator path | this |
//! |---|---|---|
//! | Scope | chosen in the request body | every `local`-mode project, always |
//! | Window | chosen, defaulting to retention | retention, never chosen |
//! | Estimate computed | yes | **yes** |
//! | Estimate gates the start | **yes**, via `plan_token` | no |
//!
//! # Why the estimate is still computed with nothing gating on it
//!
//! ADR-042 §6 gives it three jobs that have nothing to do with authorization:
//! the ETA the customer watches, the audit record they would need to dispute an
//! invoice, and the per-project byte budget the worker's anomaly guard compares
//! against. Skipping it here would leave the purchase path — the one that spends
//! money with no human in the loop — as the *only* path with no budget behind
//! it. So the estimate runs; it simply does not block.
//!
//! # Why there is no scope parameter
//!
//! [`CloudTelemetryActivationTrigger::start_purchase_activation`] takes nothing.
//! A project list or a window here would be a second way to spend a customer's
//! money on an arbitrary set of projects, reachable from a crate that has no
//! `plan_token` verification in it. The scope is a fixed property of the trigger
//! — "everything this instance still stores locally, over everything local
//! storage holds" — which is exactly what the enrollment authorized and nothing
//! more.
//!
//! # Ineligible projects are enqueued, not filtered out
//!
//! A project whose `cloud_telemetry_fidelity` is not `queryable` still gets a
//! row, and the worker records it as `skipped` with its reason and the setting
//! that unblocks it (ADR-042 §4). Filtering it out here would be the operator
//! path's behaviour, and the operator path can do that because it hands the
//! operator a table of skips *before* they confirm. The purchase path has no
//! such moment: the job's own status card is the only surface, so a project that
//! silently never appears on it is a project whose history quietly does not
//! exist on Cloud, discovered months later.

use std::sync::Arc;

use temps_cloud_client::CloudLink;
use temps_core::{
    CloudTelemetryActivationTrigger, DBDateTime, StartedTelemetryActivation,
    TelemetryActivationOutcome, TelemetryActivationSkipped,
};
use temps_entities::cloud_telemetry_bulk_jobs::BulkJobTrigger;

use crate::services::cloud_backfill::{estimate_backfill, CloudBackfillSource};
use crate::services::cloud_bulk_activation::{
    BulkJobProjectPlan, CloudBulkActivationError, CloudBulkActivationService, EnqueueBulkJobRequest,
};
use crate::services::cloud_fidelity::{CloudPolicyCache, CloudPolicyError, CloudTelemetryPolicy};
use crate::services::telemetry_write_mode::{TelemetryWriteModeService, CLOUD_SETUP_PATH};

/// Ceiling on how many projects one purchase-triggered job may cover.
///
/// The same bound the operator path applies, for a different reason: there the
/// limit comes from what a `plan_token` can carry, here from what one job can
/// sensibly *be*. An instance past this size gets no automatic job at all and a
/// log line saying so — a truncated activation that silently left half the
/// instance behind would be worse than none, because nothing on screen would
/// say which half.
pub const MAX_PURCHASE_ACTIVATION_PROJECTS: usize =
    crate::services::cloud_bulk_activation_plan::MAX_PLAN_PROJECTS;

/// Starts the activation a Temps Cloud enrollment just paid for.
pub struct PurchaseActivationTrigger {
    jobs: Arc<CloudBulkActivationService>,
    write_modes: Arc<TelemetryWriteModeService>,
    policies: Arc<CloudPolicyCache>,
    link: Arc<CloudLink>,
    source: Arc<CloudBackfillSource>,
    /// How far back "everything local storage holds" reaches, in days. Injected
    /// rather than read here so the enqueued window and the operator path's
    /// default window come from one place.
    retention_days: u32,
}

impl PurchaseActivationTrigger {
    pub fn new(
        jobs: Arc<CloudBulkActivationService>,
        write_modes: Arc<TelemetryWriteModeService>,
        policies: Arc<CloudPolicyCache>,
        link: Arc<CloudLink>,
        source: Arc<CloudBackfillSource>,
        retention_days: u32,
    ) -> Self {
        Self {
            jobs,
            write_modes,
            policies,
            link,
            source,
            retention_days,
        }
    }

    /// Whether this instance could ship anything at all right now.
    ///
    /// Checked before any project is looked at, because every one of these is a
    /// property of the link: creating a job that the worker would immediately
    /// abort puts a red card in front of an operator whose instance is working
    /// exactly as they configured it. Telemetry export in particular is explicit
    /// consent (ADR-040) — enrolling without it is a deliberate choice, not a
    /// misconfiguration to correct on the operator's behalf.
    fn link_readiness(&self) -> Option<TelemetryActivationSkipped> {
        if !self.link.is_linked() {
            return Some(TelemetryActivationSkipped::NotConfigured {
                reason: "This instance is not linked to Temps Cloud, so there is nowhere to \
                         activate projects to."
                    .to_string(),
                setup_path: Some(CLOUD_SETUP_PATH.to_string()),
            });
        }
        if matches!(
            self.link.status(),
            temps_cloud_client::LinkStatus::CredentialRejected { .. }
        ) {
            return Some(TelemetryActivationSkipped::NotConfigured {
                reason: "Temps Cloud rejected this instance's credential, so no telemetry could \
                         be shipped. Re-enroll the instance, then start the activation from the \
                         Cloud telemetry status card."
                    .to_string(),
                setup_path: Some(CLOUD_SETUP_PATH.to_string()),
            });
        }
        if !self.link.telemetry_enabled() {
            return Some(TelemetryActivationSkipped::NotConfigured {
                reason: "Temps Cloud telemetry export is switched off for this instance, so no \
                         project's spans may leave it. Nothing was activated. Turn telemetry \
                         export on and start the activation from the Cloud telemetry status \
                         card."
                    .to_string(),
                setup_path: Some(CLOUD_SETUP_PATH.to_string()),
            });
        }
        None
    }

    /// Estimate one project, or fall back to an unestimated plan entry.
    ///
    /// An estimate is bookkeeping about a transfer, never part of one, and on
    /// this path nothing gates on it. So a project whose count query fails still
    /// gets queued with a zero estimate: it will still be switched, its history
    /// will still ship, and its actuals will still be recorded.
    ///
    /// The cost of a zero estimate is a missing contribution to the ETA and a
    /// *coarser* byte budget — never an absent one.
    /// [`anomaly_byte_budget`](crate::services::anomaly_byte_budget) measures an
    /// unestimated project against
    /// [`UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES`](crate::services::UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES)
    /// instead, so a failed count cannot hand this path — the one that spends
    /// without a human confirm — an unbounded allowance on exactly the projects
    /// nothing is known about.
    async fn plan_for(
        &self,
        project_id: i32,
        policy: &CloudTelemetryPolicy,
        window_from: DBDateTime,
        window_to: DBDateTime,
    ) -> BulkJobProjectPlan {
        let mut plan = BulkJobProjectPlan {
            project_id,
            window_from,
            window_to,
            estimated_spans: 0,
            estimated_bytes: 0,
        };

        // An ineligible project is enqueued so the worker can record the skip
        // and its fix, but estimating it would only produce the same refusal in
        // a place that cannot show it.
        if !policy.fidelity.is_queryable() {
            return plan;
        }

        match estimate_backfill(
            self.source.as_ref(),
            self.link.as_ref(),
            policy,
            project_id,
            window_from,
            window_to,
        )
        .await
        {
            Ok(estimate) => {
                plan.estimated_spans = estimate.spans;
                plan.estimated_bytes = estimate.estimated_metered_bytes;
            }
            Err(error) => {
                tracing::warn!(
                    project_id,
                    %error,
                    "Could not estimate project {project_id}'s Temps Cloud activation; it is \
                     still queued and will still ship, but without an estimate it contributes \
                     nothing to the ETA and has no byte budget of its own",
                );
            }
        }

        plan
    }
}

#[async_trait::async_trait]
impl CloudTelemetryActivationTrigger for PurchaseActivationTrigger {
    async fn start_purchase_activation(
        &self,
    ) -> Result<TelemetryActivationOutcome, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(skipped) = self.link_readiness() {
            return Ok(TelemetryActivationOutcome::Skipped(skipped));
        }

        // Refused up front with the in-flight job named, rather than after
        // estimating every project — the estimate is cheap relative to a send
        // but it is not free, and the answer cannot change.
        if let Some(active) = self.jobs.active_job().await? {
            return Ok(TelemetryActivationOutcome::Skipped(
                TelemetryActivationSkipped::AlreadyActive {
                    batch_id: active.job.id.to_string(),
                },
            ));
        }

        let project_ids = self.write_modes.local_mode_project_ids().await?;
        if project_ids.is_empty() {
            return Ok(TelemetryActivationOutcome::Skipped(
                TelemetryActivationSkipped::NoLocalProjects,
            ));
        }
        if project_ids.len() > MAX_PURCHASE_ACTIVATION_PROJECTS {
            return Ok(TelemetryActivationOutcome::Skipped(
                TelemetryActivationSkipped::NotConfigured {
                    reason: format!(
                        "This instance has {} projects still storing spans locally, more than \
                         one activation job may carry ({MAX_PURCHASE_ACTIVATION_PROJECTS}). \
                         Nothing was activated automatically — activating half of them and \
                         saying nothing about the rest would be worse. Activate them in batches \
                         from the Cloud telemetry status card.",
                        project_ids.len()
                    ),
                    setup_path: Some(CLOUD_SETUP_PATH.to_string()),
                },
            ));
        }

        // ADR-042 §1: the window is "everything local storage holds". Anything
        // older has already been dropped by retention, so quoting or queueing it
        // would promise history that no longer exists. Fixed here, once, and
        // never recomputed — see `BulkJobProjectPlan`.
        let window_to = chrono::Utc::now();
        let window_from = window_to - chrono::Duration::days(self.retention_days as i64);

        let mut projects = Vec::with_capacity(project_ids.len());
        for project_id in &project_ids {
            let policy = match self.policies.resolve_project(*project_id).await {
                Ok(policy) => policy,
                // Deleted between the id list and its turn. Leave it out
                // entirely: there is no project to switch and no history to
                // ship, and a row that only ever renders as `project_not_found`
                // is noise on the card.
                Err(CloudPolicyError::ProjectNotFound { .. }) => continue,
                // A lookup failure is not this project's verdict. Queue it
                // unestimated and let the worker resolve the policy again when
                // its turn arrives, where a skip is recordable and visible.
                Err(error) => {
                    tracing::warn!(
                        project_id = *project_id,
                        %error,
                        "Could not read a project's Cloud telemetry policy while queueing the \
                         purchase-triggered activation; queueing it unestimated"
                    );
                    CloudTelemetryPolicy::metered()
                }
            };
            projects.push(
                self.plan_for(*project_id, &policy, window_from, window_to)
                    .await,
            );
        }

        if projects.is_empty() {
            return Ok(TelemetryActivationOutcome::Skipped(
                TelemetryActivationSkipped::NoLocalProjects,
            ));
        }

        let detail = match self
            .jobs
            .enqueue_job(EnqueueBulkJobRequest {
                trigger: BulkJobTrigger::Purchase,
                // ADR-042 §8: the payment is the authorization and there is no
                // operator to attribute the spend to. The column is nullable
                // precisely so this does not have to name somebody who did not
                // choose it.
                requested_by_user_id: None,
                // ADR-042 §9: `plan_hash` is set only on the operator path. It
                // is the identity of a *confirmed* estimate, and there was no
                // confirmation here. Writing one would claim a two-phase
                // confirm happened.
                plan_hash: None,
                projects: projects.clone(),
            })
            .await
        {
            Ok(detail) => detail,
            // Lost a race with a concurrent enqueue between the check above and
            // here. Not an error to report: point at the winner.
            Err(CloudBulkActivationError::JobAlreadyActive { job_id, .. }) => {
                return Ok(TelemetryActivationOutcome::Skipped(
                    TelemetryActivationSkipped::AlreadyActive {
                        batch_id: job_id.to_string(),
                    },
                ))
            }
            Err(error) => return Err(Box::new(error)),
        };

        Ok(TelemetryActivationOutcome::Started(
            StartedTelemetryActivation {
                batch_id: detail.job.id.to_string(),
                project_ids: projects.iter().map(|plan| plan.project_id).collect(),
                estimated_spans: detail.job.estimated_spans,
                estimated_bytes: detail.job.estimated_bytes,
                window_from: window_from.to_rfc3339(),
                window_to: window_to.to_rfc3339(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;

    fn at(rfc3339: &str) -> DBDateTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc)
    }

    /// A trigger with a link that is not enrolled — enough to exercise the
    /// readiness gate, which is the part that must answer before anything is
    /// read out of the database.
    fn unlinked_trigger() -> (tempfile::TempDir, PurchaseActivationTrigger) {
        let directory = tempfile::tempdir().expect("temp dir");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let trigger = PurchaseActivationTrigger::new(
            Arc::new(CloudBulkActivationService::new(db.clone())),
            Arc::new(TelemetryWriteModeService::new(db.clone())),
            Arc::new(CloudPolicyCache::new(db.clone())),
            link,
            Arc::new(CloudBackfillSource::Timescale(db)),
            30,
        );
        (directory, trigger)
    }

    #[tokio::test]
    async fn an_unlinked_instance_queues_nothing_and_says_where_to_link_it() {
        // The readiness gate must answer before any query runs — the database in
        // this trigger is `Disconnected`, so reaching one would fail the test
        // rather than pass it quietly.
        let (_directory, trigger) = unlinked_trigger();

        let outcome = trigger
            .start_purchase_activation()
            .await
            .expect("an unlinked instance is not an error");

        match outcome {
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NotConfigured {
                reason,
                setup_path,
            }) => {
                assert!(reason.contains("not linked"), "{reason}");
                assert_eq!(setup_path.as_deref(), Some(CLOUD_SETUP_PATH));
            }
            other => panic!("expected a not-configured skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn telemetry_export_switched_off_activates_nothing_rather_than_overriding_consent() {
        // Enrolling without consenting to telemetry export is a deliberate
        // choice. Queueing a job that ships spans anyway would override it;
        // queueing one that immediately aborts would put a red card in front of
        // an operator whose instance is working exactly as configured.
        let directory = tempfile::tempdir().expect("temp dir");
        let state_path = directory.path().join("cloud-link/state.json");
        let mut state = temps_cloud_client::EnrollmentState::new("https://cloud.test/");
        state.token = Some("instance-token".to_string());
        state.tenant_id = Some(uuid::Uuid::new_v4());
        state.save(&state_path).expect("persist enrollment state");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("telemetry export stays off");

        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let trigger = PurchaseActivationTrigger::new(
            Arc::new(CloudBulkActivationService::new(db.clone())),
            Arc::new(TelemetryWriteModeService::new(db.clone())),
            Arc::new(CloudPolicyCache::new(db.clone())),
            link,
            Arc::new(CloudBackfillSource::Timescale(db)),
            30,
        );

        let outcome = trigger
            .start_purchase_activation()
            .await
            .expect("withheld consent is not an error");

        match outcome {
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NotConfigured {
                reason,
                ..
            }) => assert!(reason.contains("switched off"), "{reason}"),
            other => panic!("expected a not-configured skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_ineligible_project_is_queued_unestimated_rather_than_dropped() {
        // The purchase path has no pre-confirm table of skips, so a project the
        // worker will skip must still get a row — otherwise it never appears on
        // the only surface that could tell the operator its history is missing.
        let (_directory, trigger) = unlinked_trigger();

        let plan = trigger
            .plan_for(
                7,
                &CloudTelemetryPolicy::metered(),
                at("2026-08-01T00:00:00Z"),
                at("2026-09-01T00:00:00Z"),
            )
            .await;

        assert_eq!(plan.project_id, 7);
        assert_eq!(
            (plan.estimated_spans, plan.estimated_bytes),
            (0, 0),
            "an ineligible project is not estimated: nothing would be sent for it"
        );
        assert!(!CloudTelemetryFidelity::Metered.is_queryable());
    }

    #[tokio::test]
    async fn an_estimate_failure_queues_the_project_anyway_instead_of_failing_the_activation() {
        // The estimate is bookkeeping about a transfer, never part of one, and
        // nothing on this path gates on it. A count query that fails must cost
        // the ETA, not the customer's activation.
        let (_directory, trigger) = unlinked_trigger();

        // `Disconnected` guarantees the estimate errors; the plan must survive.
        let plan = trigger
            .plan_for(
                11,
                &CloudTelemetryPolicy::queryable(["http.route".to_string()]),
                at("2026-08-01T00:00:00Z"),
                at("2026-09-01T00:00:00Z"),
            )
            .await;

        assert_eq!(plan.project_id, 11);
        assert_eq!(plan.estimated_spans, 0);
        assert_eq!(plan.window_to, at("2026-09-01T00:00:00Z"));
    }
}
