// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The background task that runs a bulk Cloud-telemetry activation job
//! (ADR-042 §2, §3, §5, §7).
//!
//! One task, spawned by `OtelPlugin` alongside the Cloud-primary outbox worker,
//! on any instance that has a Cloud link. It does exactly one thing: take the
//! single active job and walk its projects, **one at a time, in ascending
//! project id**, switching each to Cloud-primary and then shipping its history.
//!
//! # Why sequential is not a placeholder
//!
//! ADR-041 §3b forbids concurrent in-flight submissions from one instance until
//! Cloud confirms `/v1/telemetry`'s idempotency and metering tolerate them, and
//! ADR-042 P0 turned that into a hard structural fact: the link hands out **one**
//! [`SubmissionScope`](temps_cloud_client::SubmissionScope) at a time, globally.
//! What makes forty projects feel like one action here is the queue, not
//! parallelism.
//!
//! `backfill_cloud_telemetry_window` opens and holds that scope for the
//! duration of a single call, so the worker calls it once per **chunk** rather
//! than once per project. That is not an accident of convenience — it is what
//! gives the live outbox worker a window between chunks (ADR-042 §3: "the live
//! outbox always wins"), and it is what gives cancellation and restart a
//! boundary to land on.
//!
//! # Why the job tables are the only resume state
//!
//! A scoped submission deliberately persists nothing resumable into the Cloud
//! link's `state.json` (ADR-042 P0). So after every acknowledged chunk this
//! worker writes the returned `CloudBackfillCursor` to the project's row,
//! together with the running totals. A kill at any point costs at most the
//! chunk in flight; a restart re-enters the backfill with the same cursor and
//! re-ships nothing.
//!
//! # What stops a job, and what merely stops a project
//!
//! - **Per-project** (read error, projection failure, a refused shipment):
//!   record the truncated reason on that project and move to the next one. The
//!   mode switch that already happened is **never** rolled back — reverting a
//!   project to `local` after some spans shipped Cloud-primary would split its
//!   history across both stores and write a false boundary into the interval
//!   ledger. A recorded, retryable hole is honest; a silently bisected timeline
//!   is not (ADR-042 §7).
//! - **Instance-wide** (`NotLinked`, `CredentialRejected`,
//!   `TelemetryExportDisabled`, and a submission scope held by something else):
//!   stop the whole job with one actionable reason and leave every untouched
//!   project `pending`. Continuing would fail the remaining projects
//!   identically and bury the one real cause under a pile of duplicates.
//! - **Eligibility** (`cloud_telemetry_fidelity` is not `queryable`): record
//!   `skipped: fidelity_not_queryable` and continue. The job does **not** raise
//!   fidelity on the operator's behalf — that is a separate decision with its
//!   own cost, and it must not happen as an invisible consequence of paying.

use std::sync::Arc;
use std::time::Duration;

use temps_cloud_client::CloudLink;
use temps_core::DBDateTime;
use temps_entities::cloud_telemetry_bulk_job_projects::Model as BulkJobProject;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use uuid::Uuid;

use crate::services::cloud_backfill::{
    backfill_cloud_telemetry_window, CloudBackfillError, CloudBackfillSource, DEFAULT_BATCH_SIZE,
};
use crate::services::cloud_backfill_progress::CloudBackfillProgressService;
use crate::services::cloud_bulk_activation::{
    cursor_of, BulkAbortReason, BulkSkipReason, CloudBulkActivationError,
    CloudBulkActivationService,
};
use crate::services::cloud_fidelity::{CloudPolicyCache, CloudPolicyError, CloudTelemetryPolicy};
use crate::services::telemetry_write_mode::{
    CloudLinkSnapshot, TelemetryWriteModeError, TelemetryWriteModeService,
};

/// How long the worker waits before looking for work again when it found none.
///
/// Five seconds rather than the outbox worker's one: a bulk job is created by a
/// deliberate operator action or an enrollment, not by span traffic, so this is
/// only the ceiling on how long a freshly queued job waits to be noticed.
/// Nothing on the live Cloud-primary write path depends on it.
pub const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How wide a backfill chunk is, in days.
///
/// The unit of cursor persistence, of cancellation, and of yielding to the live
/// outbox worker — so it trades resume granularity against per-chunk overhead.
/// One day matches the `temps backfill cloud-telemetry` default, so the two
/// paths advance their cursors at the same rate and an operator comparing them
/// is comparing like with like.
pub const DEFAULT_CHUNK_DAYS: u32 = 1;

/// Where the worker reads the operator's throttle from, at runtime.
///
/// ADR-042 §3 puts `rate_limit_spans_per_sec` on the singleton `settings` row
/// rather than in an environment variable, and this trait is why that is worth
/// anything: the value is re-read as each project is picked up, so an operator
/// who starts an activation and then watches it compete with their own read IO
/// can throttle it **without stopping a job they have already paid for**. A
/// constructor parameter would have made the setting a restart-only knob, which
/// is the same as not having it.
///
/// A trait rather than a direct `ConfigService` dependency so this crate's
/// worker stays testable without a settings table, matching how
/// `temps_cloud_client::OutboxCapSource` supplies the outbox byte cap.
#[async_trait::async_trait]
pub trait BulkRateLimitSource: Send + Sync {
    /// The operator's ceiling in spans per second, or `None` for unthrottled.
    ///
    /// Implementations must return `None` — never a made-up number — when they
    /// cannot read the setting, so a database blip leaves whatever throttle the
    /// job already had rather than silently changing how fast money is spent.
    async fn bulk_rate_limit_spans_per_sec(&self) -> Option<u32>;
}

/// Where the worker reads the byte-budget anomaly factor from, at runtime
/// (ADR-042 §6.3).
///
/// Sibling of [`BulkRateLimitSource`] and read at the same point — once per
/// project — for the same reason: an operator whose spans are more
/// heterogeneous than `estimate_backfill`'s 1,000-span sample can see must be
/// able to widen the budget *without stopping a job they have already paid for*,
/// and one whose bill is running away must be able to narrow it just as fast.
///
/// A separate trait rather than a second method on `BulkRateLimitSource` so an
/// instance can wire one, both or neither, and so a caller reading a worker's
/// construction can see exactly which settings it depends on.
#[async_trait::async_trait]
pub trait BulkAnomalyFactorSource: Send + Sync {
    /// The multiple of a project's estimate at which the job stops it, or
    /// `None` when the setting cannot be read.
    ///
    /// `None` must mean "I could not read it", never "there is no guard". The
    /// worker falls back to its configured factor, so a database blip cannot
    /// silently remove a money guard from a running activation.
    async fn bulk_anomaly_factor(&self) -> Option<f32>;
}

/// Floor under a project's anomaly budget, in bytes: **64 KiB**.
///
/// The factor alone is not enough at the small end. A project whose whole window
/// is a handful of spans has an estimate of a few hundred bytes, and ordinary
/// causes — a span exported late enough to land in the window after
/// `count_spans_window` ran, a single error span carrying a stack trace —
/// can push the actual past 5× that without anything being wrong. Pausing such
/// a project would spend an operator's attention to protect an amount of money
/// that rounds to nothing.
///
/// 64 KiB is far below one Cloud submission's worth of spans at any realistic
/// projection size, so it never masks a real over-run on a project big enough
/// for one to matter; above it the factor governs entirely.
pub const MIN_ANOMALY_BUDGET_BYTES: u64 = 64 * 1024;

/// The estimate a project is given when it has none: **64 MiB**.
///
/// A zero `estimated_bytes` does not mean "this project will ship nothing" — it
/// means *nobody counted*. `plan_for` records zero whenever `estimate_backfill`
/// fails, and a project queued after a failed policy lookup arrives with zero
/// too. Treating that as "no budget" was the hole this constant closes: the one
/// path that spends a customer's money with no human confirm would have handed
/// an *unbounded* allowance to exactly the projects nothing is known about.
///
/// It is a stand-in **estimate** rather than a flat budget so the factor still
/// governs, which buys two things:
///
/// - the remedy is the one already documented everywhere else — widen the Temps
///   Cloud bulk activation anomaly factor in Settings and retry the project —
///   rather than a second, undiscoverable knob that only unknown-estimate
///   projects respond to;
/// - the ceiling stays bounded in both directions. At the default 5× factor an
///   unestimated project may spend 320 MiB before a human is asked, and at
///   [`MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`](temps_core::MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR)
///   it may spend at most 3.2 GiB. Past that the answer is not a bigger blind
///   budget: it is the operator path, which estimates the project properly and
///   asks the operator to confirm the number before anything is sent.
///
/// 64 MiB is deliberately generous against the case this is expected to arise
/// from — a transient database failure during estimation, not a systematically
/// unmeasurable instance — and deliberately far below "whatever the window
/// happens to contain", because the asymmetry ADR-042 §6.3 states still holds:
/// too tight costs a retry click, too loose costs money that cannot be returned.
pub const UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES: u64 = 64 * 1024 * 1024;

/// The machine-readable prefix on an anomaly pause's `last_error`.
///
/// Same `"<code>: <sentence>"` shape the job's `abort_reason` already uses, so
/// a client, a log grep and a human all get the code to branch on *and* the
/// sentence that says what to do — without a schema change, and without an
/// anomaly pause being indistinguishable from a transport failure in the one
/// column both are recorded in.
pub const ANOMALY_PAUSE_CODE: &str = "anomaly_byte_budget";

/// The same, for a project that was stopped against a *stand-in* estimate.
///
/// A distinct code because the row on screen says `estimated_bytes: 0`, and a
/// reader who is told they exceeded a budget derived from zero has been told
/// something that reads like a bug. It is also a different remedy: the factor
/// still widens it, but the honest fix is to re-run this project from the
/// operator path, where it is estimated properly and the number is confirmed
/// before anything ships.
pub const UNESTIMATED_PAUSE_CODE: &str = "anomaly_unestimated_budget";

/// How many bytes a project may ship before the job stops it.
///
/// Total by construction: there is no input for which this yields "no budget".
/// A project with a zero estimate is measured against
/// [`UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES`], and a factor that is not a usable
/// positive number falls back to
/// [`DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR`](temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR)
/// rather than to an absent one. Both used to return `None`, and `None` meant
/// the guard never fired — a money guard that switches itself off on the inputs
/// it is least able to reason about is worse than no guard, because the code
/// reads as though one is present.
pub fn anomaly_byte_budget(estimated_bytes: u64, factor: f32) -> u64 {
    // A NaN, infinite, zero or negative factor is not a decision anybody made:
    // `effective_bulk_anomaly_factor` already resolves those, so reaching here
    // with one means a hand-written `BulkAnomalyFactorSource`. Fall back to the
    // documented default instead of removing the budget.
    let factor = if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
    };
    let basis = if estimated_bytes == 0 {
        UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES
    } else {
        estimated_bytes
    };
    let scaled = (basis as f64) * (factor as f64);
    // Saturating rather than wrapping: an absurd factor should read as "no
    // realistic transfer can exceed this", never as a tiny budget that pauses
    // everything.
    let scaled = if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled.ceil() as u64
    };
    scaled.max(MIN_ANOMALY_BUDGET_BYTES)
}

/// Whether this project has run away with the customer's money (ADR-042 §6.3).
pub fn exceeds_anomaly_budget(estimated_bytes: u64, bytes_shipped: u64, factor: f32) -> bool {
    bytes_shipped > anomaly_byte_budget(estimated_bytes, factor)
}

/// What gets written to the project's `last_error` when the budget is exceeded.
///
/// `last_error` is bounded at
/// [`MAX_LAST_ERROR_CHARS`](crate::services::MAX_LAST_ERROR_CHARS) (300) before
/// it is stored, so every character here is spent on something a reader cannot
/// get elsewhere. Three things are therefore deliberately absent:
///
/// - **The estimate.** It is already a column on the same row
///   (`estimated_bytes`), rendered next to this message.
/// - **"The switch is not rolled back, this is retryable."** The activation card
///   already prints that for *every* failed project, so repeating it here would
///   buy nothing and cost the tail.
/// - **The factor.** A hand-set factor can render as a 39-digit float, which
///   would push a message carrying two `u64`s past the bound. The budget it
///   produced is the number a reader acts on, and the factor is on the log line.
///
/// What is left is the diagnosis and the one thing nothing else on screen says:
/// this budget is a setting, and here is where to change it. An operator who
/// reads only this must be able to act on it.
pub fn anomaly_pause_reason(bytes_shipped: u64, budget: u64) -> String {
    format!(
        "{ANOMALY_PAUSE_CODE}: shipped {bytes_shipped} bytes, over this project's {budget}-byte \
         budget, so the rest of its history was not shipped. If that budget is wrong for this \
         instance, change the Temps Cloud bulk activation anomaly factor in Settings, then retry \
         this project."
    )
}

/// What gets written to `last_error` when a project with **no estimate** is
/// stopped against its stand-in budget.
///
/// Held to the same 300-character bound and the same "diagnosis, then the one
/// thing nothing else on screen says" discipline as
/// [`anomaly_pause_reason`], with one difference in content: the row shows
/// `estimated_bytes: 0`, so the first job of this sentence is to explain that
/// the zero is a *failed measurement*, not a claim that the project was empty.
/// Without that, a reader is looking at a budget derived from nothing and no
/// reason to believe the stop was deliberate.
pub fn unestimated_pause_reason(bytes_shipped: u64, budget: u64) -> String {
    format!(
        "{UNESTIMATED_PAUSE_CODE}: this project's cost could not be measured before activation, \
         so it got a default {budget}-byte budget; it shipped {bytes_shipped} bytes and the rest \
         was not sent. Raise the Temps Cloud bulk activation anomaly factor in Settings and retry \
         it."
    )
}

/// Turn an operator's spans-per-second ceiling into the pause the backfill
/// takes between batches.
///
/// One batch is `batch_size` spans, so holding an average of `per_second` spans
/// per second means spending `batch_size / per_second` seconds on each batch.
/// Identical arithmetic to `temps backfill cloud-telemetry`'s
/// `--rate-limit-spans-per-sec`, deliberately: two paths that claim the same
/// units must mean the same thing.
pub fn rate_limit_pause(batch_size: u64, spans_per_sec: Option<u32>) -> Option<Duration> {
    let per_second = spans_per_sec.filter(|value| *value > 0)?;
    let seconds = batch_size.max(1) as f64 / per_second as f64;
    Some(Duration::from_millis((seconds * 1000.0) as u64))
}

/// Tuning for one worker instance.
///
/// Deliberately constructor parameters and **not** environment variables. The
/// chunk width and batch size are structural (they set the resume granularity
/// and must match the transport's submission size); the one knob an operator
/// plausibly changes at runtime — the throttle — is read per project from
/// [`BulkRateLimitSource`] instead, so it can be changed mid-job. `rate_limit`
/// here is the fallback used when no source is wired, and its default is the
/// ADR's stated default of unthrottled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BulkActivationTuning {
    /// Spans per Cloud submission, capped by the transport's own batch size.
    pub batch_size: u64,
    /// Chunk width in days.
    pub chunk_days: u32,
    /// Optional pause between batches, so a backfill on a live instance does
    /// not monopolise local read IO. `None` is unthrottled, per ADR-042 §3.
    pub rate_limit: Option<Duration>,
    /// How long to wait between looks for work.
    pub poll_interval: Duration,
    /// ADR-042 §6.3: the multiple of a project's estimate at which the job
    /// stops that project. The fallback used when no
    /// [`BulkAnomalyFactorSource`] is wired; a plain number rather than an
    /// `Option` because "no guard at all" is not a state this worker may be in
    /// on a path that spends money without a human confirm.
    pub anomaly_factor: f32,
}

impl Default for BulkActivationTuning {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            chunk_days: DEFAULT_CHUNK_DAYS,
            rate_limit: None,
            poll_interval: IDLE_POLL_INTERVAL,
            anomaly_factor: temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
        }
    }
}

/// What one pass over the active job did.
///
/// Returned so a test — and, later, a status endpoint — can assert on the
/// outcome rather than on log output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkActivationCycle {
    /// No job is pending or running.
    Idle,
    /// Every project reached a terminal state and the job was settled.
    Finished { job_id: Uuid, projects: usize },
    /// The job stopped on an instance-wide condition; untouched projects are
    /// still `pending`.
    Aborted {
        job_id: Uuid,
        reason: BulkAbortReason,
    },
    /// An operator asked the job to stop, and it stopped at a chunk boundary.
    Cancelled { job_id: Uuid },
    /// The process is shutting down. The job stays `running` and the next start
    /// resumes it from its persisted cursor.
    Interrupted { job_id: Uuid },
    /// The job could not be advanced because its own bookkeeping failed. The
    /// job stays active and the next cycle retries: a database blip must not
    /// strand an activation the customer already paid for.
    Deferred { reason: String },
}

/// Runs one bulk activation job at a time, to completion.
pub struct CloudBulkActivationWorker {
    jobs: Arc<CloudBulkActivationService>,
    link: Arc<CloudLink>,
    write_modes: Arc<TelemetryWriteModeService>,
    policies: Arc<CloudPolicyCache>,
    progress: Arc<CloudBackfillProgressService>,
    source: Arc<CloudBackfillSource>,
    tuning: BulkActivationTuning,
    /// ADR-042 §3's operator throttle, re-read per project. `None` on an
    /// instance with no settings service, which falls back to `tuning`.
    rate_limits: Option<Arc<dyn BulkRateLimitSource>>,
    /// ADR-042 §6.3's byte-budget factor, re-read per project on the same
    /// cadence and for the same reason. `None` falls back to `tuning`, which is
    /// never "no guard".
    anomaly_factors: Option<Arc<dyn BulkAnomalyFactorSource>>,
}

/// How one project ended, from the job's point of view.
enum ProjectOutcome {
    /// Terminal for this project, whatever the outcome; carry on.
    Settled,
    /// Stop the whole job.
    Abort(BulkAbortReason),
    /// The operator asked to stop.
    Cancelled,
    /// The process is going away.
    Interrupted,
}

impl CloudBulkActivationWorker {
    pub fn new(
        jobs: Arc<CloudBulkActivationService>,
        link: Arc<CloudLink>,
        write_modes: Arc<TelemetryWriteModeService>,
        policies: Arc<CloudPolicyCache>,
        progress: Arc<CloudBackfillProgressService>,
        source: Arc<CloudBackfillSource>,
    ) -> Self {
        Self {
            jobs,
            link,
            write_modes,
            policies,
            progress,
            source,
            tuning: BulkActivationTuning::default(),
            rate_limits: None,
            anomaly_factors: None,
        }
    }

    pub fn with_tuning(mut self, tuning: BulkActivationTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// Wire the operator's runtime throttle (ADR-042 §3).
    pub fn with_rate_limits(mut self, rate_limits: Arc<dyn BulkRateLimitSource>) -> Self {
        self.rate_limits = Some(rate_limits);
        self
    }

    /// Wire the operator's runtime byte-budget factor (ADR-042 §6.3).
    pub fn with_anomaly_factors(
        mut self,
        anomaly_factors: Arc<dyn BulkAnomalyFactorSource>,
    ) -> Self {
        self.anomaly_factors = Some(anomaly_factors);
        self
    }

    pub fn tuning(&self) -> BulkActivationTuning {
        self.tuning
    }

    /// The pause between batches for the project about to be processed.
    ///
    /// Read here — once per project, not once per batch — because the setting
    /// changes on human timescales and a settings query per 500 spans would be
    /// load the activation does not need. A project boundary is late enough to
    /// honour a change made mid-job and early enough that the change is not
    /// deferred to the next restart.
    async fn current_rate_limit(&self) -> Option<Duration> {
        match self.rate_limits.as_ref() {
            Some(source) => rate_limit_pause(
                self.tuning.batch_size,
                source.bulk_rate_limit_spans_per_sec().await,
            ),
            None => self.tuning.rate_limit,
        }
    }

    /// The byte-budget factor for the project about to be processed.
    ///
    /// Read once per project, alongside the throttle. A failed settings read
    /// falls back to the configured factor rather than to "no budget": a
    /// database blip must not be able to quietly disable the one guard standing
    /// between a bad estimate and a customer's invoice.
    async fn current_anomaly_factor(&self) -> f32 {
        match self.anomaly_factors.as_ref() {
            Some(source) => source
                .bulk_anomaly_factor()
                .await
                .filter(|factor| factor.is_finite() && *factor > 0.0)
                .unwrap_or(self.tuning.anomaly_factor),
            None => self.tuning.anomaly_factor,
        }
    }

    /// Take the active job, if any, and run it until it stops.
    ///
    /// `shutdown` is observed at every chunk boundary. When it fires mid-job the
    /// job is left `running` on purpose: the cursor is durable, so the next
    /// start resumes it without reconfirmation (ADR-042 §7 — requiring a human
    /// to re-approve after every restart would make a long activation
    /// impossible to complete unattended).
    pub async fn run_once(
        &self,
        shutdown: &tokio::sync::watch::Receiver<bool>,
    ) -> BulkActivationCycle {
        let detail = match self.jobs.active_job().await {
            Ok(Some(detail)) => detail,
            Ok(None) => return BulkActivationCycle::Idle,
            Err(error) => return defer("find the active job", error),
        };
        let job_id = detail.job.id;
        let planned = detail.projects.len();

        if let Err(error) = self.jobs.mark_job_running(job_id).await {
            return defer("mark the job running", error);
        }
        tracing::info!(
            %job_id,
            projects = planned,
            pending = detail.pending_projects(),
            "Running a bulk Temps Cloud telemetry activation job"
        );

        loop {
            if *shutdown.borrow() {
                tracing::info!(
                    %job_id,
                    "Stopping a bulk Temps Cloud telemetry activation job for shutdown; it will \
                     resume from its persisted cursor on the next start"
                );
                return BulkActivationCycle::Interrupted { job_id };
            }

            match self.jobs.cancel_requested(job_id).await {
                Ok(true) => return self.settle_cancelled(job_id).await,
                Ok(false) => {}
                Err(error) => return defer("read the cancellation flag", error),
            }

            let project = match self.jobs.next_pending_project(job_id).await {
                Ok(Some(project)) => project,
                Ok(None) => {
                    return match self.jobs.finish_job(job_id).await {
                        Ok(_) => BulkActivationCycle::Finished {
                            job_id,
                            projects: planned,
                        },
                        Err(error) => defer("finish the job", error),
                    }
                }
                Err(error) => return defer("find the next pending project", error),
            };

            match self.activate_project(job_id, &project, shutdown).await {
                ProjectOutcome::Settled => continue,
                ProjectOutcome::Abort(reason) => {
                    // The project the abort interrupted goes back to `pending`
                    // rather than `failed`: the link being down is not its
                    // fault, and its cursor is intact, so a resume continues
                    // from exactly where it stopped.
                    if let Err(error) = self
                        .jobs
                        .release_project_to_pending(job_id, project.project_id)
                        .await
                    {
                        tracing::warn!(
                            %job_id,
                            project_id = project.project_id,
                            %error,
                            "Could not release the in-flight project back to pending while \
                             aborting; it will be retried from its cursor on the next resume"
                        );
                    }
                    return match self.jobs.abort_job(job_id, reason).await {
                        Ok(_) => BulkActivationCycle::Aborted { job_id, reason },
                        Err(error) => defer("abort the job", error),
                    };
                }
                ProjectOutcome::Cancelled => {
                    if let Err(error) = self
                        .jobs
                        .release_project_to_pending(job_id, project.project_id)
                        .await
                    {
                        tracing::warn!(
                            %job_id,
                            project_id = project.project_id,
                            %error,
                            "Could not release the in-flight project back to pending while \
                             cancelling"
                        );
                    }
                    return self.settle_cancelled(job_id).await;
                }
                ProjectOutcome::Interrupted => return BulkActivationCycle::Interrupted { job_id },
            }
        }
    }

    async fn settle_cancelled(&self, job_id: Uuid) -> BulkActivationCycle {
        match self.jobs.mark_job_cancelled(job_id).await {
            Ok(_) => BulkActivationCycle::Cancelled { job_id },
            Err(error) => defer("mark the job cancelled", error),
        }
    }

    /// Switch one project, then ship its history (ADR-042 §5: switch first).
    ///
    /// Switching first means new spans go to Cloud from that instant while
    /// history arrives behind them, and the interval ledger records the exact
    /// boundary. The inverse order would keep writing new spans locally for the
    /// whole duration of the backfill, growing the very window the backfill is
    /// trying to close — it never converges on a busy project.
    async fn activate_project(
        &self,
        job_id: Uuid,
        project: &BulkJobProject,
        shutdown: &tokio::sync::watch::Receiver<bool>,
    ) -> ProjectOutcome {
        let project_id = project.project_id;

        let policy = match self.policies.resolve_project(project_id).await {
            Ok(policy) => policy,
            Err(CloudPolicyError::ProjectNotFound { .. }) => {
                return self
                    .skip(job_id, project_id, BulkSkipReason::ProjectNotFound)
                    .await
            }
            Err(error) => return self.fail(job_id, project, 0, 0, error.to_string()).await,
        };

        // The same gate `set_write_mode` enforces, checked first so an
        // ineligible project is recorded as `skipped` with the setting that
        // unblocks it rather than as a failure an operator has to decode.
        if !policy.fidelity.is_queryable() {
            return self
                .skip(job_id, project_id, BulkSkipReason::FidelityNotQueryable)
                .await;
        }

        if let Err(error) = self.jobs.mark_project_switching(job_id, project_id).await {
            tracing::warn!(%job_id, project_id, %error, "Could not mark a project switching");
        }

        if let Err(error) = self
            .write_modes
            .set_write_mode(
                project_id,
                CloudTelemetryWriteMode::Cloud,
                link_snapshot(&self.link),
            )
            .await
        {
            return match error {
                TelemetryWriteModeError::NotLinked { .. } => {
                    ProjectOutcome::Abort(BulkAbortReason::NotLinked)
                }
                TelemetryWriteModeError::CredentialRejected { .. } => {
                    ProjectOutcome::Abort(BulkAbortReason::CredentialRejected)
                }
                TelemetryWriteModeError::TelemetryExportDisabled { .. } => {
                    ProjectOutcome::Abort(BulkAbortReason::TelemetryExportDisabled)
                }
                TelemetryWriteModeError::ProjectNotFound { .. } => {
                    self.skip(job_id, project_id, BulkSkipReason::ProjectNotFound)
                        .await
                }
                // Raced with a fidelity change between the check above and the
                // call. Still a skip, not a failure: nothing was switched and
                // the fix is the same setting.
                TelemetryWriteModeError::FidelityTooLow { .. } => {
                    self.skip(job_id, project_id, BulkSkipReason::FidelityNotQueryable)
                        .await
                }
                other => self.fail(job_id, project, 0, 0, other.to_string()).await,
            };
        }

        if let Err(error) = self.jobs.mark_project_backfilling(job_id, project_id).await {
            tracing::warn!(%job_id, project_id, %error, "Could not mark a project backfilling");
        }

        self.backfill_project(job_id, project, &policy, shutdown)
            .await
    }

    /// Ship one project's window, chunk by chunk, persisting after each.
    async fn backfill_project(
        &self,
        job_id: Uuid,
        project: &BulkJobProject,
        policy: &CloudTelemetryPolicy,
        shutdown: &tokio::sync::watch::Receiver<bool>,
    ) -> ProjectOutcome {
        let project_id = project.project_id;
        let estimated_spans = project.estimated_spans.max(0) as u64;

        // The per-project surface the Console already renders, reused rather
        // than duplicated (ADR-042 §6/§8), stamped with the job that is driving
        // it so "already running" is never mysterious.
        if let Err(error) = self
            .progress
            .start_for_bulk_job(
                project_id,
                estimated_spans,
                project.window_from,
                project.window_to,
                job_id,
            )
            .await
        {
            // Bookkeeping about a transfer, never part of it.
            tracing::warn!(
                %job_id, project_id, %error,
                "Could not open the shared backfill progress record; the activation continues \
                 but the Console will not show this project's progress"
            );
        }

        // ADR-042 §3: the operator's throttle, as it stands right now. Read
        // once per project so a change made while the job is running takes
        // effect at the next project rather than at the next restart.
        let rate_limit = self.current_rate_limit().await;
        // ADR-042 §6.3, read on the same cadence and for the same reason: an
        // operator can widen or narrow the money guard mid-job.
        let anomaly_factor = self.current_anomaly_factor().await;
        let estimated_bytes = project.estimated_bytes.max(0) as u64;

        let mut cursor = cursor_of(project);
        let mut spans_shipped = project.spans_shipped.max(0) as u64;
        let mut bytes_shipped = project.bytes_shipped.max(0) as u64;

        for (chunk_from, chunk_to) in split_window(
            project.window_from,
            project.window_to,
            self.tuning.chunk_days,
        ) {
            // Chunks entirely behind the cursor were finished by an earlier
            // pass — before a restart, or before a cancellation.
            if let Some(last) = cursor.last_start_time {
                if chunk_to < last {
                    continue;
                }
            }

            if *shutdown.borrow() {
                return ProjectOutcome::Interrupted;
            }
            match self.jobs.cancel_requested(job_id).await {
                Ok(true) => return ProjectOutcome::Cancelled,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        %job_id, project_id, %error,
                        "Could not read the cancellation flag at a chunk boundary; continuing, \
                         and it will be read again at the next one"
                    );
                }
            }

            let report = match backfill_cloud_telemetry_window(
                self.source.as_ref(),
                self.link.as_ref(),
                policy,
                project_id,
                chunk_from,
                chunk_to,
                self.tuning.batch_size,
                cursor.clone(),
                rate_limit,
                |_| {},
            )
            .await
            {
                Ok(report) => report,
                Err(error) => {
                    return match self.classify(&error) {
                        Some(reason) => ProjectOutcome::Abort(reason),
                        None => {
                            self.fail(
                                job_id,
                                project,
                                spans_shipped,
                                bytes_shipped,
                                error.to_string(),
                            )
                            .await
                        }
                    }
                }
            };

            cursor = report.final_cursor.clone();
            spans_shipped = spans_shipped.saturating_add(report.spans_shipped);
            bytes_shipped = bytes_shipped.saturating_add(report.estimated_metered_bytes);

            // The cursor write is what makes a kill survivable. It happens
            // after the chunk is acknowledged and before the next one starts,
            // so the worst a crash can cost is the chunk in flight.
            if let Err(error) = self
                .jobs
                .record_project_progress(job_id, project_id, &cursor, spans_shipped, bytes_shipped)
                .await
            {
                // Losing the cursor means the next resume re-ships this chunk,
                // which costs money. Stopping the project here bounds that to
                // one chunk instead of the whole remaining window.
                return self
                    .fail(
                        job_id,
                        project,
                        spans_shipped,
                        bytes_shipped,
                        format!("could not persist the resume cursor after a shipped chunk, so the run stopped rather than risk re-shipping the rest of the window: {error}"),
                    )
                    .await;
            }

            if let Err(error) = self
                .progress
                .record_progress(
                    project_id,
                    spans_shipped,
                    estimated_spans.max(spans_shipped),
                )
                .await
            {
                tracing::warn!(
                    %job_id, project_id, %error,
                    "Could not update the shared backfill progress record; the transfer is \
                     unaffected but the Console will show stale progress"
                );
            }

            // ADR-042 §6.3: the byte-budget guard, checked *after* the cursor is
            // durable so stopping here costs nothing already paid for and the
            // untouched remainder of the window can be retried from exactly this
            // point. Per-project, never instance-wide (§7): one project's
            // estimate being wrong says nothing about the next project's, so the
            // job carries on rather than aborting.
            let budget = anomaly_byte_budget(estimated_bytes, anomaly_factor);
            if bytes_shipped > budget {
                // The guidance that does not fit inside the 300-character
                // `last_error` bound. Logged once, at the moment it becomes
                // true, so an operator debugging alone has the whole story
                // somewhere even though the UI shows the short form.
                if estimated_bytes == 0 {
                    tracing::error!(
                        %job_id,
                        project_id,
                        bytes_shipped,
                        budget,
                        anomaly_factor,
                        stand_in_estimate_bytes = UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES,
                        "Stopped a project during bulk Temps Cloud activation: its cost could \
                         not be measured before the activation started, so it was shipping \
                         against a conservative default budget and has now passed it. The rest \
                         of this project's history is still on this instance and unspent; the \
                         job continues with the next project. Either raise the Temps Cloud bulk \
                         activation anomaly factor in Settings and retry this project, or \
                         activate it from the Cloud telemetry status card, which estimates it \
                         first and shows the number before anything is sent."
                    );
                } else {
                    tracing::error!(
                        %job_id,
                        project_id,
                        estimated_bytes,
                        bytes_shipped,
                        budget,
                        anomaly_factor,
                        "Stopped a project during bulk Temps Cloud activation: it shipped far \
                         more than it was estimated to. An estimate this far out usually means \
                         the 1,000 spans it sampled were much smaller than the rest of the \
                         window — or a bug. The rest of this project's history is still on this \
                         instance and unspent; the job continues with the next project. If this \
                         budget is wrong for this instance, change the Temps Cloud bulk \
                         activation anomaly factor in Settings and retry this project."
                    );
                }
                let reason = if estimated_bytes == 0 {
                    unestimated_pause_reason(bytes_shipped, budget)
                } else {
                    anomaly_pause_reason(bytes_shipped, budget)
                };
                return self
                    .fail(job_id, project, spans_shipped, bytes_shipped, reason)
                    .await;
            }
        }

        if let Err(error) = self
            .progress
            .complete(
                project_id,
                spans_shipped,
                estimated_spans.max(spans_shipped),
            )
            .await
        {
            tracing::warn!(
                %job_id, project_id, %error,
                "Could not close the shared backfill progress record for a completed project"
            );
        }

        match self
            .jobs
            .mark_project_done(job_id, project_id, spans_shipped, bytes_shipped)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    %job_id,
                    project_id,
                    spans_shipped,
                    bytes_shipped,
                    "Activated a project on Temps Cloud and shipped its history"
                );
                ProjectOutcome::Settled
            }
            Err(error) => {
                // The work is done and paid for; only the bookkeeping failed.
                // Leaving the row non-terminal is correct — the next cycle
                // re-enters it, and the cursor means it re-ships nothing.
                tracing::error!(
                    %job_id, project_id, %error,
                    "Could not mark a fully backfilled project done; it will be re-entered from \
                     its cursor, which re-ships nothing"
                );
                ProjectOutcome::Settled
            }
        }
    }

    /// Decide whether a backfill failure is the link's fault or this project's.
    ///
    /// The first three cases are named by the backfill itself. The fourth is
    /// the one that needs the live link: `ship_batch` reports a revoked link as
    /// a `ShipmentRefused` carrying prose rather than a distinct variant, so a
    /// refusal is re-checked against the link's actual state instead of
    /// pattern-matching on a message. Getting this wrong in the safe direction
    /// costs one project a retry; getting it wrong the other way marks
    /// twenty-three projects `failed` for one revoked credential.
    fn classify(&self, error: &CloudBackfillError) -> Option<BulkAbortReason> {
        match error {
            CloudBackfillError::NotLinked { .. } => Some(BulkAbortReason::NotLinked),
            CloudBackfillError::TelemetryExportDisabled { .. } => {
                Some(BulkAbortReason::TelemetryExportDisabled)
            }
            CloudBackfillError::SubmissionScopeBusy { .. } => {
                Some(BulkAbortReason::SubmissionScopeBusy)
            }
            _ => instance_wide_link_failure(&self.link),
        }
    }

    async fn skip(&self, job_id: Uuid, project_id: i32, reason: BulkSkipReason) -> ProjectOutcome {
        match self
            .jobs
            .mark_project_skipped(job_id, project_id, reason)
            .await
        {
            Ok(_) => tracing::info!(
                %job_id,
                project_id,
                reason = reason.as_str(),
                setup_path = reason.setup_path(project_id),
                "Skipped a project during bulk Temps Cloud activation; it was not switched and \
                 nothing was shipped for it"
            ),
            Err(error) => tracing::error!(
                %job_id, project_id, %error,
                "Could not record a skipped project; it will be re-evaluated on the next cycle"
            ),
        }
        ProjectOutcome::Settled
    }

    async fn fail(
        &self,
        job_id: Uuid,
        project: &BulkJobProject,
        spans_shipped: u64,
        bytes_shipped: u64,
        reason: String,
    ) -> ProjectOutcome {
        let project_id = project.project_id;
        let estimated_spans = project.estimated_spans.max(0) as u64;

        if let Err(error) = self
            .progress
            .fail(
                project_id,
                spans_shipped,
                estimated_spans.max(spans_shipped),
                &reason,
            )
            .await
        {
            tracing::warn!(
                %job_id, project_id, %error,
                "Could not record the failure on the shared backfill progress record"
            );
        }

        match self
            .jobs
            .mark_project_failed(job_id, project_id, spans_shipped, bytes_shipped, &reason)
            .await
        {
            Ok(_) => tracing::error!(
                %job_id,
                project_id,
                spans_shipped,
                "Bulk Temps Cloud activation failed for this project; its write mode is left \
                 Cloud-primary on purpose and the job continues with the next project: {reason}"
            ),
            Err(error) => tracing::error!(
                %job_id, project_id, %error,
                "Could not record a failed project; it will be re-entered from its cursor"
            ),
        }
        ProjectOutcome::Settled
    }
}

/// The long-lived task, spawned by `OtelPlugin`.
///
/// Mirrors the outbox worker's lifecycle: a `watch` channel owned by the
/// spawning task is the shutdown signal, and the loop returns when it fires.
/// Resume-on-restart needs no separate entry point — the first cycle after boot
/// finds whatever job is still active and continues it.
pub async fn run(
    worker: CloudBulkActivationWorker,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let poll = worker.tuning().poll_interval;
    tracing::info!(
        poll_interval_secs = poll.as_secs(),
        chunk_days = worker.tuning().chunk_days,
        batch_size = worker.tuning().batch_size,
        "Bulk Temps Cloud telemetry activation worker starting"
    );

    loop {
        // Look for work before the first sleep, so a job left running by a
        // killed process resumes at startup rather than one poll later.
        match worker.run_once(&shutdown).await {
            BulkActivationCycle::Idle => {}
            cycle => tracing::debug!(?cycle, "bulk Cloud activation cycle finished"),
        }

        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("Bulk Temps Cloud telemetry activation worker stopped");
                    return;
                }
            }
        }

        if *shutdown.borrow() {
            tracing::info!("Bulk Temps Cloud telemetry activation worker stopped");
            return;
        }
    }
}

/// Read the link's gate inputs, in the same shape the settings handler uses.
fn link_snapshot(link: &CloudLink) -> CloudLinkSnapshot {
    CloudLinkSnapshot {
        linked: link.is_linked(),
        telemetry_enabled: link.telemetry_enabled(),
        credential_rejected: matches!(
            link.status(),
            temps_cloud_client::LinkStatus::CredentialRejected { .. }
        ),
    }
}

/// Whether the link itself is why something failed.
///
/// Free function so the classification is testable without a worker, a
/// database or a job.
fn instance_wide_link_failure(link: &CloudLink) -> Option<BulkAbortReason> {
    if !link.is_linked() {
        return Some(BulkAbortReason::NotLinked);
    }
    if matches!(
        link.status(),
        temps_cloud_client::LinkStatus::CredentialRejected { .. }
    ) {
        return Some(BulkAbortReason::CredentialRejected);
    }
    if !link.telemetry_enabled() {
        return Some(BulkAbortReason::TelemetryExportDisabled);
    }
    None
}

/// Slice `[from, to]` into non-overlapping chunks of `chunk_days`.
///
/// Identical in shape to the CLI's own window splitter, deliberately: the two
/// paths must advance their cursors over the same boundaries, or an operator
/// resuming a bulk job with the offline tool (and vice versa) would find the
/// cursor pointing into the middle of a chunk that the other side thinks it
/// already finished.
fn split_window(
    from: DBDateTime,
    to: DBDateTime,
    chunk_days: u32,
) -> Vec<(DBDateTime, DBDateTime)> {
    let chunk = chrono::Duration::days(chunk_days.max(1) as i64);
    let mut chunks = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let end = std::cmp::min(cursor + chunk, to);
        chunks.push((cursor, end));
        cursor = end + chrono::Duration::milliseconds(1);
    }
    if chunks.is_empty() {
        chunks.push((from, to));
    }
    chunks
}

/// A bookkeeping failure defers the cycle rather than killing the job.
///
/// A transient database error must not strand an activation the customer has
/// already paid for, and must not be silent either — so it is logged at ERROR
/// and the job stays active for the next cycle to pick up.
fn defer(operation: &str, error: CloudBulkActivationError) -> BulkActivationCycle {
    tracing::error!(
        %error,
        "Could not {operation} for the bulk Temps Cloud activation job; the job stays active \
         and the next cycle will retry"
    );
    BulkActivationCycle::Deferred {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DBDateTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc)
    }

    /// A link with a persisted, enrolled credential and telemetry export on.
    fn linked_cloud() -> (tempfile::TempDir, Arc<CloudLink>) {
        let directory = tempfile::tempdir().expect("temp dir");
        let state_path = directory.path().join("cloud-link/state.json");
        let mut state = temps_cloud_client::EnrollmentState::new("https://cloud.test/");
        state.token = Some("instance-token".to_string());
        state.tenant_id = Some(uuid::Uuid::new_v4());
        state.save(&state_path).expect("persist enrollment state");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("enable telemetry export");
        (directory, link)
    }

    #[test]
    fn the_window_splits_into_chunks_that_cover_it_without_overlap() {
        let from = at("2026-08-01T00:00:00Z");
        let to = at("2026-08-04T00:00:00Z");

        let chunks = split_window(from, to, 1);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, from);
        assert_eq!(chunks.last().expect("non-empty").1, to);
        for pair in chunks.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "chunks must not overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_zero_length_window_still_yields_one_chunk() {
        // A project with a degenerate window must still be visited and settled,
        // or the job would never reach a terminal state.
        let from = at("2026-08-01T00:00:00Z");
        assert_eq!(split_window(from, from, 1), vec![(from, from)]);
    }

    #[test]
    fn a_zero_chunk_width_is_clamped_rather_than_looping_forever() {
        let from = at("2026-08-01T00:00:00Z");
        let to = at("2026-08-03T00:00:00Z");
        assert_eq!(split_window(from, to, 0).len(), 2);
    }

    #[test]
    fn a_healthy_link_produces_no_instance_wide_abort() {
        let (_directory, link) = linked_cloud();
        assert_eq!(instance_wide_link_failure(&link), None);
    }

    #[test]
    fn an_unlinked_instance_aborts_the_whole_job_rather_than_one_project() {
        // The whole point of the distinction: 23 remaining projects would fail
        // identically, and the one real cause would be buried under 23 copies.
        let directory = tempfile::tempdir().expect("temp dir");
        let link = Arc::new(CloudLink::load(directory.path().to_path_buf(), "test"));

        assert_eq!(
            instance_wide_link_failure(&link),
            Some(BulkAbortReason::NotLinked)
        );
    }

    #[test]
    fn telemetry_export_switched_off_is_its_own_instance_wide_reason() {
        let (_directory, link) = linked_cloud();
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("disable telemetry export");

        assert_eq!(
            instance_wide_link_failure(&link),
            Some(BulkAbortReason::TelemetryExportDisabled)
        );
    }

    #[test]
    fn the_three_link_level_backfill_errors_abort_and_a_read_error_does_not() {
        let (_directory, link) = linked_cloud();
        let worker_classify = |error: &CloudBackfillError| match error {
            CloudBackfillError::NotLinked { .. } => Some(BulkAbortReason::NotLinked),
            CloudBackfillError::TelemetryExportDisabled { .. } => {
                Some(BulkAbortReason::TelemetryExportDisabled)
            }
            CloudBackfillError::SubmissionScopeBusy { .. } => {
                Some(BulkAbortReason::SubmissionScopeBusy)
            }
            _ => instance_wide_link_failure(&link),
        };

        assert_eq!(
            worker_classify(&CloudBackfillError::NotLinked { project_id: 7 }),
            Some(BulkAbortReason::NotLinked)
        );
        assert_eq!(
            worker_classify(&CloudBackfillError::TelemetryExportDisabled { project_id: 7 }),
            Some(BulkAbortReason::TelemetryExportDisabled)
        );
        assert_eq!(
            worker_classify(&CloudBackfillError::SubmissionScopeBusy {
                project_id: 7,
                reason: "held".into()
            }),
            Some(BulkAbortReason::SubmissionScopeBusy)
        );
        // A ClickHouse read failure on a healthy link is this project's
        // problem, and the job must carry on to the next one.
        assert_eq!(
            worker_classify(&CloudBackfillError::ClickHouse {
                project_id: 7,
                from: "a".into(),
                to: "b".into(),
                reason: "connection reset".into()
            }),
            None
        );
    }

    #[test]
    fn a_shipment_refused_on_a_dead_link_is_reclassified_as_instance_wide() {
        // `ship_batch` reports a revoked link as prose inside `ShipmentRefused`,
        // not as a distinct variant. Matching on the message would be brittle;
        // re-reading the link is what makes this correct.
        let (_directory, link) = linked_cloud();
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("disable telemetry export");

        let refused = CloudBackfillError::ShipmentRefused {
            project_id: 7,
            spans: 500,
            resume_from: "2026-08-01T00:00:00Z".into(),
            reason: "the link was revoked or telemetry export was switched off while the \
                     backfill was running"
                .into(),
        };

        assert_eq!(
            instance_wide_link_failure(&link),
            Some(BulkAbortReason::TelemetryExportDisabled),
            "{refused}"
        );
    }

    // ── ADR-042 §3: the operator's runtime throttle ──────────────────────

    #[test]
    fn no_throttle_means_no_pause_between_batches() {
        assert_eq!(rate_limit_pause(DEFAULT_BATCH_SIZE, None), None);
    }

    #[test]
    fn a_zero_throttle_is_treated_as_unthrottled_not_as_an_infinite_pause() {
        // Read literally, zero spans per second is a job that never finishes
        // while reporting itself as running.
        assert_eq!(rate_limit_pause(DEFAULT_BATCH_SIZE, Some(0)), None);
    }

    #[test]
    fn the_pause_holds_the_operators_spans_per_second_over_a_batch() {
        // 500 spans at 1000 spans/second is half a second per batch — the same
        // arithmetic `temps backfill cloud-telemetry --rate-limit-spans-per-sec`
        // uses, so the two paths mean the same thing by the same units.
        assert_eq!(
            rate_limit_pause(500, Some(1_000)),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            rate_limit_pause(500, Some(500)),
            Some(Duration::from_millis(1_000))
        );
        // A ceiling above one batch per second still yields a pause rather
        // than rounding down to "no throttle at all".
        assert_eq!(
            rate_limit_pause(500, Some(5_000)),
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn a_zero_batch_size_cannot_produce_a_zero_length_throttle() {
        // Defensive: `batch_size` is clamped to at least 1 by the backfill, and
        // a 0 here would make the throttle a no-op while claiming to throttle.
        assert_eq!(rate_limit_pause(0, Some(1)), Some(Duration::from_secs(1)));
    }

    #[test]
    fn the_default_batch_size_matches_the_transport_submission_size() {
        // A larger batch than the link's own submission size would leave spans
        // in a queue this worker then treats as "not drained".
        let tuning = BulkActivationTuning::default();
        assert_eq!(tuning.batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(tuning.chunk_days, DEFAULT_CHUNK_DAYS);
        assert_eq!(
            tuning.rate_limit, None,
            "ADR-042 §3: the default is unthrottled"
        );
    }

    // ── ADR-042 §6.3: the byte-budget anomaly guard ──────────────────────

    #[test]
    fn a_worker_with_no_settings_source_still_has_a_money_guard() {
        // The one property that must not regress: the purchase path spends
        // money with no human confirm, so "no settings service wired" cannot
        // mean "no budget". The default is a number, not an `Option`.
        let tuning = BulkActivationTuning::default();
        assert_eq!(
            tuning.anomaly_factor,
            temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
        );
        assert!(tuning.anomaly_factor > 1.0);
    }

    #[test]
    fn the_budget_is_the_estimate_times_the_factor_once_it_clears_the_floor() {
        // 1 MiB estimated at 5x is a 5 MiB budget: comfortably above the floor,
        // so the factor is what governs.
        let estimated = 1024 * 1024;
        assert_eq!(anomaly_byte_budget(estimated, 5.0), estimated * 5);
        assert_eq!(anomaly_byte_budget(estimated, 2.0), estimated * 2);
    }

    #[test]
    fn a_tiny_project_is_not_paused_for_an_amount_of_money_that_rounds_to_nothing() {
        // A window holding a handful of spans estimates at a few hundred bytes.
        // Five times that is still nothing, and a late-arriving span would trip
        // it — spending an operator's attention to protect no money at all.
        assert_eq!(anomaly_byte_budget(400, 5.0), MIN_ANOMALY_BUDGET_BYTES);
        assert!(!exceeds_anomaly_budget(400, 60_000, 5.0));
        // Above the floor the guard still fires for the same project.
        assert!(exceeds_anomaly_budget(
            400,
            MIN_ANOMALY_BUDGET_BYTES + 1,
            5.0
        ));
    }

    #[test]
    fn a_project_that_was_never_estimated_still_has_a_bounded_budget() {
        // The security fix this test exists for. Zero does not mean "this
        // project will ship nothing", it means *nobody counted* — which is
        // precisely the project least safe to hand an unbounded allowance to,
        // on the one path that spends a customer's money with no human confirm.
        //
        // Previously `anomaly_byte_budget(0, _)` returned `None` and
        // `exceeds_anomaly_budget` read `None` as "never exceeds", so an
        // unestimated project could ship without limit while the code read as
        // though a guard were present.
        let factor = temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR;
        let budget = anomaly_byte_budget(0, factor);

        assert_eq!(
            budget,
            UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES * factor as u64,
            "an unmeasured project is measured against the stand-in estimate"
        );
        // 10 GB out of a project nobody could count must stop.
        assert!(
            exceeds_anomaly_budget(0, 10_000_000_000, factor),
            "an unestimated project shipping 10 GB must be paused, not left unbounded"
        );
        // …and a project that stays inside the stand-in budget still finishes,
        // so the fix does not turn every failed estimate into a stopped job.
        assert!(!exceeds_anomaly_budget(0, budget, factor));
        assert!(exceeds_anomaly_budget(0, budget + 1, factor));
    }

    #[test]
    fn an_unestimated_project_still_answers_to_the_operators_factor() {
        // The remedy the pause message points at has to actually work: widening
        // the factor in Settings must widen an unestimated project's budget too,
        // or the message tells an operator to do something that changes nothing.
        let narrow = anomaly_byte_budget(0, temps_core::MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR);
        let wide = anomaly_byte_budget(0, temps_core::MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR);

        assert!(wide > narrow);
        // And the ceiling is still a ceiling: even at the widest factor an
        // operator may set, a blind spend is bounded.
        assert_eq!(
            wide,
            UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES
                * temps_core::MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR as u64
        );
    }

    #[test]
    fn an_unestimated_pause_says_the_zero_is_a_failed_measurement_not_an_empty_project() {
        // The row on screen shows `estimated_bytes: 0`. A reader told they blew
        // a budget derived from zero has been told something that reads like a
        // bug, so this message has one extra job the estimated one does not.
        let reason = unestimated_pause_reason(500_000_000, 320 * 1024 * 1024);

        assert!(
            reason.starts_with(&format!("{UNESTIMATED_PAUSE_CODE}: ")),
            "{reason}"
        );
        assert!(reason.contains("could not be measured"), "{reason}");
        assert!(reason.contains("anomaly factor"), "{reason}");
        assert!(reason.contains("Settings"), "{reason}");
        // The bound the stored column enforces, asserted at absurd magnitudes so
        // the tail — where the remedy lives — cannot be truncated away by a
        // large byte count.
        let worst_case = unestimated_pause_reason(u64::MAX, u64::MAX);
        assert_eq!(
            crate::services::truncate_failure_reason(&worst_case),
            worst_case,
            "the unestimated pause reason must fit within MAX_LAST_ERROR_CHARS"
        );
    }

    #[test]
    fn an_order_of_magnitude_over_run_is_stopped_and_ordinary_skew_is_not() {
        // The ADR's own line: an estimate wrong by an order of magnitude means
        // a bug, and a bug that costs money should stop. Sampling skew from a
        // 1,000-span head-of-window sample should not.
        let estimated = 100 * 1024 * 1024;
        assert!(
            !exceeds_anomaly_budget(estimated, estimated * 3, 5.0),
            "3x is plausible span-size skew, not a bug"
        );
        assert!(
            exceeds_anomaly_budget(estimated, estimated * 12, 5.0),
            "12x is the runaway this guard exists for"
        );
    }

    #[test]
    fn an_absurd_factor_saturates_rather_than_wrapping_into_a_tiny_budget() {
        // A hand-edited settings row could carry anything. Wrapping here would
        // turn "never pause" into "pause immediately", which is the exact
        // opposite of what the operator asked for.
        let budget = anomaly_byte_budget(u64::MAX, f32::MAX);
        assert_eq!(budget, u64::MAX);
        assert!(!exceeds_anomaly_budget(u64::MAX, u64::MAX, f32::MAX));
    }

    #[test]
    fn a_non_finite_factor_falls_back_to_the_default_rather_than_removing_the_budget() {
        // Belt and braces: `CloudSettings::effective_bulk_anomaly_factor` already
        // resolves these, but a `BulkAnomalyFactorSource` is a public trait and a
        // NaN comparison is false for everything, which would read as "no guard"
        // in one direction and "pause all" in the other depending on operand
        // order. Neither may be reachable by accident — so an unusable factor
        // resolves to the documented default here too.
        let default = anomaly_byte_budget(
            1_000_000,
            temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
        );
        assert_eq!(anomaly_byte_budget(1_000_000, f32::NAN), default);
        assert_eq!(anomaly_byte_budget(1_000_000, 0.0), default);
        assert_eq!(anomaly_byte_budget(1_000_000, -1.0), default);
        assert!(exceeds_anomaly_budget(1_000_000, u64::MAX, f32::NAN));
    }

    #[test]
    fn the_pause_reason_carries_the_code_the_numbers_and_what_to_do_next() {
        let reason = anomaly_pause_reason(40_000_000, 5_000_000);

        // The `"<code>: <sentence>"` shape the job's `abort_reason` already
        // uses, so an anomaly pause is distinguishable from a transport failure
        // in the one column both are recorded in — without a schema change.
        assert!(
            reason.starts_with(&format!("{ANOMALY_PAUSE_CODE}: ")),
            "{reason}"
        );
        assert!(reason.contains("40000000"), "{reason}");
        assert!(reason.contains("5000000"), "{reason}");
        // The one thing nothing else on the screen says: this budget is a
        // setting, and here is where to change it. A self-hosted operator
        // reading only this has nobody to ask what to do next.
        assert!(reason.contains("anomaly factor"), "{reason}");
        assert!(reason.contains("Settings"), "{reason}");
        assert!(reason.contains("retry this project"), "{reason}");
    }

    #[test]
    fn the_whole_pause_reason_survives_the_stored_length_bound() {
        // `last_error` is truncated to 300 characters before it is stored, and
        // the *tail* of this message is where the tuning pointer lives. A reason
        // that overflows would silently drop exactly the sentence that tells an
        // operator what to do, leaving them with a number and no next step.
        //
        // Asserted at `u64::MAX` rather than at a realistic size so the bound
        // holds by construction: nobody adding a clause later has to reason
        // about how many digits a byte count might have.
        let worst_case = anomaly_pause_reason(u64::MAX, u64::MAX);
        assert_eq!(
            crate::services::truncate_failure_reason(&worst_case),
            worst_case,
            "the pause reason must fit within MAX_LAST_ERROR_CHARS even at absurd magnitudes"
        );
    }

    /// One project's walk through the per-chunk decision, with only the pieces
    /// the guard actually depends on.
    ///
    /// Mirrors `backfill_project`'s loop: ship a chunk, add its bytes, then test
    /// the budget. Returns how many chunks were shipped and whether the project
    /// was paused, so a test can assert on "stopped *before* shipping the rest"
    /// rather than merely "stopped".
    fn walk_project(
        estimated_bytes: u64,
        bytes_per_chunk: u64,
        chunks: usize,
        factor: f32,
    ) -> (usize, bool) {
        let mut shipped = 0u64;
        for chunk in 0..chunks {
            shipped = shipped.saturating_add(bytes_per_chunk);
            if exceeds_anomaly_budget(estimated_bytes, shipped, factor) {
                return (chunk + 1, true);
            }
        }
        (chunks, false)
    }

    #[test]
    fn one_projects_anomaly_stops_that_project_early_and_leaves_the_others_alone() {
        // ADR-042 §7's distinction, in the smallest form that can be asserted
        // without a database: a per-project budget is per-project. The runaway
        // project stops after its first chunk — 9 of its 10 chunks, and the
        // money they would have cost, never leave the instance — while the
        // projects on either side of it in the same job finish all 10.
        let factor = temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR;
        let chunk_bytes = 10 * 1024 * 1024;

        // Honest estimates: 10 chunks each, well inside a 5x budget.
        let before = walk_project(chunk_bytes * 10, chunk_bytes, 10, factor);
        // A badly wrong estimate — one chunk's worth quoted for ten chunks of
        // history — is the runaway.
        let runaway = walk_project(chunk_bytes, chunk_bytes * 10, 10, factor);
        let after = walk_project(chunk_bytes * 10, chunk_bytes, 10, factor);

        assert_eq!(before, (10, false), "an honest project must finish");
        assert_eq!(
            runaway,
            (1, true),
            "the runaway must stop after the first chunk, not after all ten"
        );
        assert_eq!(
            after,
            (10, false),
            "the project after the runaway must be untouched by it"
        );
    }

    #[test]
    fn an_unestimated_project_stops_mid_window_instead_of_shipping_all_of_it() {
        // The same walk as above, for the case the guard used to skip entirely.
        // Before the fix this project shipped all ten chunks — every byte of a
        // window nobody had measured — because a zero estimate produced no
        // budget and "no budget" was read as "never exceeds".
        let factor = temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR;
        // Chunks large enough that the stand-in budget is reached partway
        // through, which is exactly the shape a runaway takes.
        let chunk_bytes = UNESTIMATED_ANOMALY_BUDGET_BASE_BYTES;

        let (chunks_shipped, paused) = walk_project(0, chunk_bytes, 10, factor);

        assert!(
            paused,
            "an unmeasured project must not ship without a bound"
        );
        assert!(
            chunks_shipped < 10,
            "the guard has to fire *before* the whole window ships, not after: shipped \
             {chunks_shipped} of 10 chunks"
        );
        // A project that stays inside the stand-in budget still completes, so
        // the fix does not turn every failed estimate into a stopped job.
        assert_eq!(walk_project(0, 1024, 10, factor), (10, false));
    }
}
