// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-project telemetry write mode, its gate, and its interval ledger
//! (ADR-041 §1, §7, §8).
//!
//! # The gate is here, not in the UI
//!
//! Setting `write_mode = cloud` is refused unless *all* of the project's
//! fidelity is `queryable`, the instance is linked, and the Cloud telemetry
//! switch is on. A Cloud-primary project at `Metered` fidelity would store
//! nothing readable anywhere — real spans discarded locally, unreadable
//! placeholders in Cloud — which is the single worst configuration this system
//! can be in. It has to be structurally unreachable rather than merely
//! discouraged, so the check lives in the service layer that every write path
//! goes through (API, UI, CLI), backed by a database `CHECK` constraint as the
//! second line and the ingest path's own
//! [`CloudTelemetryPolicy::is_cloud_primary`] as the third.
//!
//! Each refusal names a *different* missing prerequisite, because "raise the
//! fidelity", "link this instance" and "turn the telemetry switch on" are three
//! unrelated fixes and a self-hosted operator has nobody to ask which one they
//! need.
//!
//! # Why the mode and the ledger are separate
//!
//! `projects.cloud_telemetry_write_mode` is the operator's **declared intent**.
//! `project_telemetry_write_intervals` is where spans **actually went**. They
//! diverge on purpose: when Cloud's ingest allowance is exhausted, span writes
//! fall back to the local store immediately (ADR-041 §7b) while the operator's
//! declared intent is preserved, so the moment Cloud starts accepting again the
//! project returns to Cloud-primary without anyone having to remember to switch
//! it back.
//!
//! Only a disconnect changes the declared intent, and only because the gate
//! would refuse `cloud` anyway once the link is gone.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DbErr, EntityTrait, FromQueryResult, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait,
};
use temps_core::DBDateTime;
use temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use temps_entities::project_telemetry_write_intervals::{
    self as write_intervals, TelemetrySignalGroup, TelemetryWriteIntervalReason,
};
use temps_entities::{projects, telemetry_gap_windows};

use crate::services::cloud_fidelity::CloudPolicyCache;

/// Console path that owns the Cloud link and its telemetry switch.
pub const CLOUD_SETUP_PATH: &str = "/settings/cloud";

/// Writes a project's already-queued Cloud-bound spans back to the local span
/// store and settles the rows.
///
/// # Why this is a trait here rather than a direct dependency
///
/// Withdrawing consent has to reach the *durable* queue, not just the future
/// write path (ADR-041 §Risks, "fidelity downgrade racing a Cloud-primary
/// write" — now sharper, because the buffer survives restarts). But the spill
/// needs the local storage backend, which this service does not and should not
/// hold: it would drag the whole `OtelStorage` surface into the thing that
/// answers "what is this project's write mode".
///
/// So the implementation lives beside the disconnect path that already does
/// exactly this work
/// ([`OutboxSpiller`](crate::services::cloud_primary_fallback::OutboxSpiller)),
/// and is injected. When it is absent — an instance with no Cloud link, where
/// nothing can have been queued in the first place — the guard below falls back
/// to refusing a downgrade while rows remain, which is the safe direction.
#[async_trait::async_trait]
pub trait TelemetrySpiller: Send + Sync {
    /// Returns how many spans were written back to local storage.
    async fn spill_projects(&self, project_ids: &[i32]) -> usize;
}

/// Everything that can refuse a write-mode or fidelity change.
///
/// Every variant names the project and the specific prerequisite, because the
/// fixes are unrelated to each other.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryWriteModeError {
    #[error(
        "Project {project_id} does not exist on this instance — it was never created, or it has \
         been deleted. Check the project id (the Console shows it in the project's URL)."
    )]
    ProjectNotFound { project_id: i32 },

    #[error(
        "Project {project_id} cannot use Cloud-primary telemetry writes at `{fidelity}` fidelity. \
         A Cloud-primary project stores no spans on this instance, and `metered` spans are \
         pseudonymised placeholders that cannot be read back — the project's traces would exist \
         nowhere. Raise this project's Cloud telemetry fidelity to `queryable` first."
    )]
    FidelityTooLow {
        project_id: i32,
        fidelity: CloudTelemetryFidelity,
    },

    #[error(
        "Project {project_id} cannot use Cloud-primary telemetry writes: this instance is not \
         linked to Temps Cloud, so there is nowhere for its spans to go. Link the instance at \
         {setup_path}, then set the write mode."
    )]
    NotLinked {
        project_id: i32,
        setup_path: &'static str,
    },

    #[error(
        "Project {project_id} cannot use Cloud-primary telemetry writes: Temps Cloud telemetry \
         export is switched off for this instance, so no span would ever leave. Turn telemetry \
         export on at {setup_path}, then set the write mode."
    )]
    TelemetryExportDisabled {
        project_id: i32,
        setup_path: &'static str,
    },

    #[error(
        "Project {project_id} cannot use Cloud-primary telemetry writes: Temps Cloud rejected \
         this instance's credential, so nothing can be shipped. Re-enroll the instance at \
         {setup_path}, then set the write mode."
    )]
    CredentialRejected {
        project_id: i32,
        setup_path: &'static str,
    },

    #[error(
        "Project {project_id} cannot lower its Cloud telemetry fidelity to `{requested}` while \
         its telemetry write mode is `cloud`: its spans are not stored on this instance, and \
         `metered` spans cannot be read back, so the project's traces would exist nowhere. Set \
         the write mode back to `local` first."
    )]
    FidelityDowngradeBlockedByWriteMode {
        project_id: i32,
        requested: CloudTelemetryFidelity,
    },

    #[error(
        "Project {project_id} cannot lower its Cloud telemetry fidelity to `{requested}` yet: \
         {queued_spans} span(s) already captured at `queryable` fidelity are still queued for \
         delivery to Temps Cloud, and lowering the fidelity now would not stop them being sent. \
         This instance could not write them back to local storage \
         ({spill_blocked_reason}). Wait for the queue to drain — the Cloud telemetry status page \
         shows its depth — then lower the fidelity."
    )]
    FidelityDowngradeBlockedByQueuedSpans {
        project_id: i32,
        requested: CloudTelemetryFidelity,
        queued_spans: i64,
        spill_blocked_reason: &'static str,
    },

    #[error("Failed to read the telemetry write mode for project {project_id}: {source}")]
    Read {
        project_id: i32,
        #[source]
        source: DbErr,
    },

    #[error("Failed to update the telemetry write mode for project {project_id}: {source}")]
    Write {
        project_id: i32,
        #[source]
        source: DbErr,
    },

    #[error("Failed to update the telemetry write-mode ledger: {source}")]
    Ledger {
        #[source]
        source: DbErr,
    },
}

/// A snapshot of the Cloud link, taken by the caller.
///
/// Passed in rather than read from a `CloudLink` here so this service can be
/// unit-tested without an enrolled instance, and so the gate's inputs are
/// explicit at every call site instead of ambient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloudLinkSnapshot {
    pub linked: bool,
    pub telemetry_enabled: bool,
    pub credential_rejected: bool,
}

impl CloudLinkSnapshot {
    /// Whether Cloud can actually accept spans right now.
    pub fn can_accept_spans(&self) -> bool {
        self.linked && self.telemetry_enabled && !self.credential_rejected
    }
}

/// A project's Cloud telemetry settings as the API renders them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTelemetryWriteSettings {
    pub project_id: i32,
    pub fidelity: CloudTelemetryFidelity,
    pub write_mode: CloudTelemetryWriteMode,
    pub attribute_allowlist: Vec<String>,
    /// Where spans are going *right now*, which can differ from `write_mode`
    /// during a fallback.
    pub effective_mode: CloudTelemetryWriteMode,
    /// Why, when it differs. `None` when intent and reality agree.
    pub effective_reason: Option<TelemetryWriteIntervalReason>,
    /// ADR-043 §1: the independent analytics (metrics under Phase C1) write
    /// mode. Orthogonal to `write_mode`; carried on the same lookup for the
    /// same reason `write_mode` is.
    pub analytics_write_mode: CloudAnalyticsWriteMode,
}

/// Which store answers a query for a given window (ADR-041 §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResolution {
    pub source: CloudTelemetryWriteMode,
    /// Set when the requested window straddled two intervals and was narrowed
    /// to the newest one it touches. Never `None` alongside a silently merged
    /// result — this is the field that makes ADR-040 §3's "never serve local
    /// rows under a Cloud label" checkable by the client.
    pub window_clamped_at: Option<DBDateTime>,
    /// The window actually served.
    pub from: DBDateTime,
    pub to: DBDateTime,
}

/// Whether the operator can decommission their local span store (ADR-041 §1).
///
/// Derived, never stored, so it cannot drift out of sync with the projects it
/// summarises. A partial cutover yields **zero** resource win — one project
/// left in `local` mode keeps the entire local span store running — and
/// operators will reasonably believe they have saved something before they
/// have, which is exactly why this is prominent rather than implied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSpanStoreRequirement {
    pub required: bool,
    /// The specific reason, written for a human. Always populated when
    /// `required` is true.
    pub reason: Option<String>,
    pub local_mode_projects: u64,
    pub cloud_primary_projects: u64,
    /// Newest `local` interval end still inside local retention, when that is
    /// what keeps the store required.
    pub local_history_until: Option<DBDateTime>,
}

/// Why Cloud-primary writes are currently suspended process-wide.
///
/// Encoded as a `u8` so the ingest path reads it with one relaxed atomic load
/// rather than taking a lock per batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CloudWriteSuspension {
    /// Cloud is accepting writes.
    None = 0,
    /// `Unavailable::QuotaExhausted` — the plan allowance is spent.
    QuotaExhausted = 1,
    /// The credential was refused.
    CredentialRejected = 2,
    /// The outbox reached its byte cap.
    QueueOverflow = 3,
}

impl CloudWriteSuspension {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => CloudWriteSuspension::QuotaExhausted,
            2 => CloudWriteSuspension::CredentialRejected,
            3 => CloudWriteSuspension::QueueOverflow,
            _ => CloudWriteSuspension::None,
        }
    }

    /// The ledger reason this suspension opens a `local` interval with.
    pub fn interval_reason(&self) -> TelemetryWriteIntervalReason {
        match self {
            // `None` never opens an interval; mapping it to the operator reason
            // keeps the function total without inventing a sixth variant.
            CloudWriteSuspension::None => TelemetryWriteIntervalReason::Operator,
            CloudWriteSuspension::QuotaExhausted => TelemetryWriteIntervalReason::QuotaExhausted,
            CloudWriteSuspension::CredentialRejected => {
                TelemetryWriteIntervalReason::CredentialRejected
            }
            CloudWriteSuspension::QueueOverflow => TelemetryWriteIntervalReason::QueueOverflowSpill,
        }
    }

    pub fn is_suspended(&self) -> bool {
        !matches!(self, CloudWriteSuspension::None)
    }
}

/// The service.
pub struct TelemetryWriteModeService {
    db: Arc<DatabaseConnection>,
    /// Invalidated whenever a project's mode or fidelity changes, so the ingest
    /// path picks the change up on the very next batch rather than up to one
    /// TTL later.
    policy_cache: Option<Arc<CloudPolicyCache>>,
    /// Drains a project's durable outbox back into local storage when it stops
    /// being Cloud-primary, so a consent withdrawal reaches the spans that were
    /// already serialized and queued rather than only the ones not yet written.
    spiller: Option<Arc<dyn TelemetrySpiller>>,
    /// Process-wide suspension of Cloud-primary writes (ADR-041 §7b).
    ///
    /// Read on the ingest path with one relaxed load; written by the outbox
    /// worker when Cloud refuses for a reason only the operator can fix.
    suspension: AtomicU8,
    suspension_detail: Mutex<Option<String>>,
}

#[derive(Debug, FromQueryResult)]
struct ProjectModeRow {
    id: i32,
    cloud_telemetry_fidelity: CloudTelemetryFidelity,
    cloud_telemetry_write_mode: CloudTelemetryWriteMode,
    cloud_analytics_write_mode: CloudAnalyticsWriteMode,
    cloud_telemetry_attribute_allowlist: Vec<String>,
}

#[derive(Debug, FromQueryResult)]
struct ModeCount {
    n: i64,
}

impl TelemetryWriteModeService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            policy_cache: None,
            spiller: None,
            suspension: AtomicU8::new(CloudWriteSuspension::None as u8),
            suspension_detail: Mutex::new(None),
        }
    }

    pub fn with_policy_cache(mut self, cache: Arc<CloudPolicyCache>) -> Self {
        self.policy_cache = Some(cache);
        self
    }

    pub fn with_spiller(mut self, spiller: Arc<dyn TelemetrySpiller>) -> Self {
        self.spiller = Some(spiller);
        self
    }

    // ── Suspension (ADR-041 §7b) ─────────────────────────────────────────

    /// Current suspension state. One relaxed atomic load; safe on the ingest
    /// path.
    pub fn suspension(&self) -> CloudWriteSuspension {
        CloudWriteSuspension::from_u8(self.suspension.load(Ordering::Relaxed))
    }

    /// Human-readable detail for the current suspension, for the status
    /// surfaces only.
    pub fn suspension_detail(&self) -> Option<String> {
        self.suspension_detail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Suspend Cloud-primary writes and move every affected project's ledger to
    /// a `local` interval.
    ///
    /// The declared `write_mode` on the project row is deliberately left alone:
    /// the operator's intent has not changed, only Cloud's willingness to
    /// accept. [`Self::resume_cloud_writes`] reverses this without them having
    /// to remember anything.
    ///
    /// Returns the projects whose ledger moved.
    pub async fn suspend_cloud_writes(
        &self,
        suspension: CloudWriteSuspension,
        detail: impl Into<String>,
    ) -> Result<Vec<i32>, TelemetryWriteModeError> {
        let detail = detail.into();
        let already = self.suspension();
        self.suspension.store(suspension as u8, Ordering::Relaxed);
        *self
            .suspension_detail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(detail);

        if already == suspension {
            // Already suspended for this reason; the ledger is already correct
            // and re-running the transaction each cycle would be pure churn.
            return Ok(Vec::new());
        }

        let reason = suspension.interval_reason();
        let moved = self
            .move_declared_cloud_projects_to(CloudTelemetryWriteMode::Local, reason)
            .await?;
        if !moved.is_empty() {
            tracing::warn!(
                projects = ?moved,
                reason = %reason,
                "Cloud-primary telemetry writes suspended; these projects are storing spans on \
                 this instance until Temps Cloud accepts again"
            );
        }
        Ok(moved)
    }

    /// Clear a suspension and reopen `cloud` intervals for projects whose
    /// declared mode is still `cloud`.
    pub async fn resume_cloud_writes(&self) -> Result<Vec<i32>, TelemetryWriteModeError> {
        if !self.suspension().is_suspended() {
            return Ok(Vec::new());
        }
        self.suspension
            .store(CloudWriteSuspension::None as u8, Ordering::Relaxed);
        *self
            .suspension_detail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let moved = self
            .move_declared_cloud_projects_to(
                CloudTelemetryWriteMode::Cloud,
                TelemetryWriteIntervalReason::CloudRecovered,
            )
            .await?;
        if !moved.is_empty() {
            tracing::info!(
                projects = ?moved,
                "Temps Cloud is accepting telemetry again; these projects are Cloud-primary once \
                 more"
            );
        }
        Ok(moved)
    }

    // ── The gate (ADR-041 §1) ────────────────────────────────────────────

    /// Read a project's Cloud telemetry settings, including where its spans are
    /// actually going right now.
    pub async fn settings(
        &self,
        project_id: i32,
    ) -> Result<ProjectTelemetryWriteSettings, TelemetryWriteModeError> {
        let row = self.project_row(project_id).await?;
        let open = self.open_interval(project_id).await?;

        let (effective_mode, effective_reason) = match open {
            Some(interval) if interval.mode != row.cloud_telemetry_write_mode => {
                (interval.mode, Some(interval.reason))
            }
            Some(interval) => (interval.mode, None),
            // No ledger row yet means the project has never left the default,
            // which *is* `local`. Reporting anything else would claim a cutover
            // that never happened.
            None => (row.cloud_telemetry_write_mode, None),
        };

        Ok(ProjectTelemetryWriteSettings {
            project_id,
            fidelity: row.cloud_telemetry_fidelity,
            write_mode: row.cloud_telemetry_write_mode,
            attribute_allowlist: row.cloud_telemetry_attribute_allowlist,
            effective_mode,
            effective_reason,
            analytics_write_mode: row.cloud_analytics_write_mode,
        })
    }

    /// Set a project's write mode, enforcing the §1 gate.
    ///
    /// Setting `local` is always allowed — an operator must always be able to
    /// bring a project's spans back to storage they control, whatever state
    /// Cloud is in — and it drains that project's durable outbox back to the
    /// local span store on the way (see [`Self::spill_project`]).
    pub async fn set_write_mode(
        &self,
        project_id: i32,
        requested: CloudTelemetryWriteMode,
        link: CloudLinkSnapshot,
    ) -> Result<ProjectTelemetryWriteSettings, TelemetryWriteModeError> {
        let row = self.project_row(project_id).await?;

        if requested.is_cloud_primary() {
            // Order matters only for which message the operator sees first, and
            // fidelity is the one they can fix without touching the instance's
            // Cloud link at all.
            if !row.cloud_telemetry_fidelity.is_queryable() {
                return Err(TelemetryWriteModeError::FidelityTooLow {
                    project_id,
                    fidelity: row.cloud_telemetry_fidelity,
                });
            }
            if !link.linked {
                return Err(TelemetryWriteModeError::NotLinked {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
            if link.credential_rejected {
                return Err(TelemetryWriteModeError::CredentialRejected {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
            if !link.telemetry_enabled {
                return Err(TelemetryWriteModeError::TelemetryExportDisabled {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
        }

        if row.cloud_telemetry_write_mode != requested {
            self.persist_mode_and_interval(
                project_id,
                requested,
                TelemetryWriteIntervalReason::Operator,
            )
            .await?;
            self.invalidate(project_id);

            // Leaving Cloud-primary has to reach what is already queued, not
            // only what has not been captured yet. The rows in
            // `cloud_telemetry_outbox` (entity_type = 'span') are serialized
            // `Queryable` projections of real spans; without this they keep
            // shipping to Cloud after the operator has moved the project back to
            // local storage, and — because the fidelity gate only blocks a
            // downgrade while the mode is still `cloud` — a two-step withdrawal
            // (`write_mode = local`, then `fidelity = metered`) would export up
            // to a full queue of real span data *after* consent was withdrawn.
            //
            // Deliberately after the flip, not before: once the mode is `local`
            // the ingest path stops enqueueing for this project, so the drain
            // below terminates instead of racing new arrivals.
            if !requested.is_cloud_primary() {
                self.spill_project(project_id).await;
            }
        }

        self.settings(project_id).await
    }

    /// Set a project's analytics write mode (ADR-043 §1), enforcing the same
    /// gate as `set_write_mode`.
    ///
    /// Setting `local` is always allowed — an operator must always be able to
    /// bring a project's analytics back to storage they control.
    pub async fn set_analytics_write_mode(
        &self,
        project_id: i32,
        requested: CloudAnalyticsWriteMode,
        link: CloudLinkSnapshot,
    ) -> Result<CloudAnalyticsWriteMode, TelemetryWriteModeError> {
        let row = self.project_row(project_id).await?;

        if requested.is_cloud_primary() {
            if !row.cloud_telemetry_fidelity.is_queryable() {
                return Err(TelemetryWriteModeError::FidelityTooLow {
                    project_id,
                    fidelity: row.cloud_telemetry_fidelity,
                });
            }
            if !link.linked {
                return Err(TelemetryWriteModeError::NotLinked {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
            if link.credential_rejected {
                return Err(TelemetryWriteModeError::CredentialRejected {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
            if !link.telemetry_enabled {
                return Err(TelemetryWriteModeError::TelemetryExportDisabled {
                    project_id,
                    setup_path: CLOUD_SETUP_PATH,
                });
            }
        }

        if row.cloud_analytics_write_mode != requested {
            self.persist_analytics_mode_and_interval(
                project_id,
                requested,
                TelemetryWriteIntervalReason::Operator,
            )
            .await?;
            self.invalidate(project_id);
        }

        Ok(requested)
    }

    /// Write this project's queued Cloud-bound spans back to local storage.
    ///
    /// Best effort by construction, and that is the right trade here: the
    /// alternative to a partial spill is refusing to let an operator move a
    /// project back to storage they control, which would trap the project as
    /// Cloud-primary and keep *new* spans leaving too. Anything the spill does
    /// not reach stays queued — never dropped — and
    /// [`Self::set_fidelity`]'s guard is what stops that residue turning into an
    /// egress after consent is withdrawn.
    async fn spill_project(&self, project_id: i32) {
        let Some(spiller) = self.spiller.as_ref() else {
            return;
        };
        let spilled = spiller.spill_projects(&[project_id]).await;
        if spilled > 0 {
            tracing::info!(
                project_id,
                spilled,
                "Wrote this project's queued Temps Cloud spans back to local storage after it \
                 stopped being Cloud-primary"
            );
        }
    }

    /// How many of this project's spans are still queued for Cloud.
    ///
    /// Read straight from the outbox table rather than from a `SpanOutbox`
    /// handle: the rows outlive the link that created them, so the answer has to
    /// be available on an instance that has since disconnected.
    pub async fn queued_span_count(&self, project_id: i32) -> Result<i64, TelemetryWriteModeError> {
        let row = ModeCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM cloud_telemetry_outbox \
             WHERE entity_type = 'span' AND project_id = $1 AND state = 'pending'",
            vec![project_id.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Read { project_id, source })?;
        Ok(row.map_or(0, |row| row.n.max(0)))
    }

    /// Set a project's fidelity, refusing a downgrade that would strand a
    /// Cloud-primary project or leak spans already captured under the higher
    /// tier.
    ///
    /// The error names the write mode as the thing to change first, rather than
    /// reporting a generic conflict: an operator who reads "cannot lower
    /// fidelity" without being told why will try again.
    ///
    /// # A downgrade is a consent withdrawal, and the queue is durable
    ///
    /// Blocking the downgrade only while `write_mode = cloud` closes the
    /// simultaneous case and not the sequential one. `write_mode = local`
    /// followed by `fidelity = metered` is two individually-allowed requests
    /// that together withdraw consent while up to a full byte cap of
    /// already-serialized `Queryable` spans sit in `cloud_span_outbox` waiting
    /// to ship — and the worker never re-checks either setting before shipping
    /// them.
    ///
    /// So a downgrade first drains that project's queue into local storage, and
    /// then refuses if anything is still there. Refusing is the correct
    /// direction for this one specifically (unlike returning to `write_mode =
    /// local`, which must always be allowed): the project keeps the *higher*
    /// consent tier it already had until the data captured under it has stopped
    /// being in flight, which is a delay rather than a loss, and the message
    /// names the queue depth and where to watch it drain.
    pub async fn set_fidelity(
        &self,
        project_id: i32,
        requested: CloudTelemetryFidelity,
        attribute_allowlist: Option<Vec<String>>,
    ) -> Result<ProjectTelemetryWriteSettings, TelemetryWriteModeError> {
        let row = self.project_row(project_id).await?;
        let downgrading = !requested.is_queryable() && row.cloud_telemetry_fidelity.is_queryable();

        if !requested.is_queryable() && row.cloud_telemetry_write_mode.is_cloud_primary() {
            return Err(
                TelemetryWriteModeError::FidelityDowngradeBlockedByWriteMode {
                    project_id,
                    requested,
                },
            );
        }

        if downgrading {
            self.spill_project(project_id).await;
            let queued_spans = self.queued_span_count(project_id).await?;
            if queued_spans > 0 {
                return Err(
                    TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
                        project_id,
                        requested,
                        queued_spans,
                        spill_blocked_reason: if self.spiller.is_some() {
                            "the local span store did not accept them"
                        } else {
                            "it has no local span store wired for Temps Cloud fallback"
                        },
                    },
                );
            }
        }

        let mut active: projects::ActiveModel = projects::ActiveModel {
            id: Set(project_id),
            ..Default::default()
        };
        active.cloud_telemetry_fidelity = Set(requested);
        if let Some(allowlist) = attribute_allowlist {
            active.cloud_telemetry_attribute_allowlist = Set(allowlist);
        }
        active
            .update(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Write { project_id, source })?;

        self.invalidate(project_id);
        self.settings(project_id).await
    }

    // ── Disconnect and fallback (ADR-041 §7c) ────────────────────────────

    /// Hand every Cloud-primary project back to local span storage, in one
    /// transaction, and change their declared intent too.
    ///
    /// Called when the link goes away — `DELETE /cloud`, or the Cloud telemetry
    /// switch being turned off. Unlike a quota fallback this *does* rewrite
    /// `projects.cloud_telemetry_write_mode`, because the gate would refuse
    /// `cloud` anyway once there is no link, and leaving the column claiming
    /// `cloud` would show the operator a setting that does nothing.
    ///
    /// There is never a state in which the instance is storing spans nowhere:
    /// the column and the ledger move together, and the ingest path's own
    /// link check (which does not read either) has already resumed local
    /// writes by the time this commits.
    pub async fn revert_all_to_local(
        &self,
        reason: TelemetryWriteIntervalReason,
    ) -> Result<Vec<i32>, TelemetryWriteModeError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        let cloud_projects = ProjectModeRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, cloud_telemetry_fidelity, cloud_telemetry_write_mode, \
                    cloud_telemetry_attribute_allowlist \
             FROM projects WHERE cloud_telemetry_write_mode = 'cloud' FOR UPDATE",
            vec![],
        ))
        .all(&txn)
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        if cloud_projects.is_empty() {
            txn.commit()
                .await
                .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
            return Ok(Vec::new());
        }

        let ids: Vec<i32> = cloud_projects.iter().map(|row| row.id).collect();

        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET cloud_telemetry_write_mode = 'local' WHERE id = ANY($1)",
            vec![ids.clone().into()],
        ))
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        close_and_open_intervals(&txn, &ids, CloudTelemetryWriteMode::Local, reason).await?;

        txn.commit()
            .await
            .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        for id in &ids {
            self.invalidate(*id);
        }
        tracing::info!(
            projects = ?ids,
            reason = %reason,
            "Cloud-primary projects reverted to local span storage"
        );
        Ok(ids)
    }

    /// Move the *ledger* for every project whose declared mode is `cloud`,
    /// without touching their declared intent.
    ///
    /// This is the quota/credential fallback, and its inverse on recovery.
    async fn move_declared_cloud_projects_to(
        &self,
        mode: CloudTelemetryWriteMode,
        reason: TelemetryWriteIntervalReason,
    ) -> Result<Vec<i32>, TelemetryWriteModeError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        let rows = ProjectModeRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, cloud_telemetry_fidelity, cloud_telemetry_write_mode, \
                    cloud_telemetry_attribute_allowlist \
             FROM projects WHERE cloud_telemetry_write_mode = 'cloud' FOR UPDATE",
            vec![],
        ))
        .all(&txn)
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

        let ids: Vec<i32> = rows.iter().map(|row| row.id).collect();
        if ids.is_empty() {
            txn.commit()
                .await
                .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
            return Ok(Vec::new());
        }

        close_and_open_intervals(&txn, &ids, mode, reason).await?;
        txn.commit()
            .await
            .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
        Ok(ids)
    }

    /// Project ids whose *declared* mode is `cloud`.
    /// Every live project still writing its spans to this instance, ascending.
    ///
    /// The candidate set for a bulk Cloud activation (ADR-042 §4). Projects
    /// already Cloud-primary are deliberately absent: there is nothing to
    /// switch, and including them would quote the operator for history that
    /// is already on the other side. Soft-deleted projects are excluded for the
    /// same reason every other lookup in this service excludes them — a
    /// deleted project is gone to the operator, and shipping its history would
    /// spend money on data nobody asked to keep.
    pub async fn local_mode_project_ids(&self) -> Result<Vec<i32>, TelemetryWriteModeError> {
        #[derive(FromQueryResult)]
        struct Id {
            id: i32,
        }
        let rows = Id::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM projects \
             WHERE cloud_telemetry_write_mode = 'local' AND deleted_at IS NULL \
             ORDER BY id",
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
        Ok(rows.into_iter().map(|row| row.id).collect())
    }

    pub async fn cloud_primary_project_ids(&self) -> Result<Vec<i32>, TelemetryWriteModeError> {
        #[derive(FromQueryResult)]
        struct Id {
            id: i32,
        }
        let rows = Id::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM projects \
             WHERE cloud_telemetry_write_mode = 'cloud' AND deleted_at IS NULL \
             ORDER BY id",
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
        Ok(rows.into_iter().map(|row| row.id).collect())
    }

    // ── The ledger (ADR-041 §8) ──────────────────────────────────────────

    /// The open interval for a project, if any.
    pub async fn open_interval(
        &self,
        project_id: i32,
    ) -> Result<Option<write_intervals::Model>, TelemetryWriteModeError> {
        write_intervals::Entity::find()
            .filter(write_intervals::Column::ProjectId.eq(project_id))
            .filter(write_intervals::Column::EffectiveTo.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Read { project_id, source })
    }

    /// Every interval for a project, newest first, bounded.
    ///
    /// For display only. Never use this to answer a routing question — see
    /// [`Self::intervals_covering`] for why a `LIMIT` is the wrong bound there.
    pub async fn intervals(
        &self,
        project_id: i32,
        limit: u64,
    ) -> Result<Vec<write_intervals::Model>, TelemetryWriteModeError> {
        write_intervals::Entity::find()
            .filter(write_intervals::Column::ProjectId.eq(project_id))
            .order_by_desc(write_intervals::Column::EffectiveFrom)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Read { project_id, source })
    }

    /// Every interval that overlaps `from..to`, as a real range query.
    ///
    /// # Why this is not `intervals(project_id, N)`
    ///
    /// Routing has to see *all* the intervals a window touches, and a
    /// `LIMIT`-bounded newest-first fetch does not guarantee that. Intervals are
    /// not only opened by an operator: a quota suspension closes the `cloud`
    /// interval and opens a `local` one automatically, and the recovery closes
    /// that and opens another (§7b), so a project on a flapping allowance
    /// accumulates them without anyone touching a setting. Past the limit, an
    /// older interval simply falls out of the fetched set — and a window that
    /// genuinely straddles the cutover then looks like it sits entirely inside
    /// the newest interval, so the read is served from Cloud, unclamped, with
    /// `window_clamped_at: None`, as a complete answer for a period that
    /// predates the cutover. That is precisely the implicit cross-boundary
    /// answer ADR-040 §3 forbids, and it fails *silently*.
    ///
    /// An open interval (`effective_to IS NULL`) extends to now, so it is
    /// treated as ending at infinity. A bound outside what `timestamptz` can
    /// represent — the read decorator uses `DateTime::MIN_UTC` to mean "whatever
    /// you have" — is treated as unbounded on that side rather than bound
    /// verbatim, which the database would reject.
    /// Every span-group interval that overlaps `from..to` (ADR-041 §8).
    ///
    /// Filters to `signal_group = 'spans'` so span and analytics ledgers remain
    /// independent — the routing decorator for each domain calls its own method.
    pub async fn intervals_covering(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
    ) -> Result<Vec<write_intervals::Model>, TelemetryWriteModeError> {
        self.intervals_covering_for_group(project_id, from, to, TelemetrySignalGroup::Spans)
            .await
    }

    /// Every analytics-group interval that overlaps `from..to` (ADR-043 §3).
    pub async fn analytics_intervals_covering(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
    ) -> Result<Vec<write_intervals::Model>, TelemetryWriteModeError> {
        self.intervals_covering_for_group(project_id, from, to, TelemetrySignalGroup::Analytics)
            .await
    }

    async fn intervals_covering_for_group(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
        group: TelemetrySignalGroup,
    ) -> Result<Vec<write_intervals::Model>, TelemetryWriteModeError> {
        write_intervals::Model::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id, project_id, signal_group, mode, effective_from, effective_to, reason \
             FROM project_telemetry_write_intervals \
             WHERE project_id = $1 \
               AND signal_group = $4 \
               AND effective_from <= COALESCE($3, 'infinity'::timestamptz) \
               AND COALESCE(effective_to, 'infinity'::timestamptz) \
                   >= COALESCE($2, '-infinity'::timestamptz) \
             ORDER BY effective_from",
            vec![
                project_id.into(),
                bindable_instant(from).into(),
                bindable_instant(to).into(),
                group.to_string().into(),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Read { project_id, source })
    }

    /// Which store answers a span query for `from..to`, clamping a straddle.
    ///
    /// Never merges. ADR-040 §3's reasons stand unchanged: the source badge can
    /// only name one source, and paginating across two stores is not coherently
    /// solvable with cursors. A clamped window with `window_clamped_at` set is
    /// an answer the client can render honestly; a merged one is not.
    pub async fn resolve_read_window(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
    ) -> Result<WindowResolution, TelemetryWriteModeError> {
        let intervals = self.intervals_covering(project_id, from, to).await?;
        Ok(resolve_window(&intervals, from, to))
    }

    /// Which store answers an analytics (metric/event) query for `from..to`.
    ///
    /// Uses the `analytics` signal group's independent ledger (ADR-043 §3).
    pub async fn resolve_analytics_read_window(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
    ) -> Result<WindowResolution, TelemetryWriteModeError> {
        let intervals = self
            .analytics_intervals_covering(project_id, from, to)
            .await?;
        Ok(resolve_window(&intervals, from, to))
    }

    /// Gap windows intersecting `from..to`, newest first.
    pub async fn gap_windows(
        &self,
        project_id: i32,
        from: DBDateTime,
        to: DBDateTime,
        limit: u64,
    ) -> Result<Vec<telemetry_gap_windows::Model>, TelemetryWriteModeError> {
        telemetry_gap_windows::Entity::find()
            .filter(telemetry_gap_windows::Column::ProjectId.eq(project_id))
            .filter(telemetry_gap_windows::Column::StartedAt.lte(to))
            .filter(telemetry_gap_windows::Column::EndedAt.gte(from))
            .order_by_desc(telemetry_gap_windows::Column::StartedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Read { project_id, source })
    }

    /// Recent gap windows across every project, for the Cloud settings page.
    pub async fn recent_gap_windows(
        &self,
        limit: u64,
    ) -> Result<Vec<telemetry_gap_windows::Model>, TelemetryWriteModeError> {
        telemetry_gap_windows::Entity::find()
            .order_by_desc(telemetry_gap_windows::Column::StartedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Ledger { source })
    }

    // ── The derived decommission signal (ADR-041 §1, §5) ─────────────────

    /// Whether a local span store is still needed, and why.
    ///
    /// `retention_days` is the instance's OTel span retention, which decides
    /// whether pre-cutover local history is still readable. It is passed in
    /// rather than read here so this service does not need `ConfigService` and
    /// the caller cannot accidentally use a different number than the one the
    /// retention job actually applies.
    pub async fn local_span_store_requirement(
        &self,
        retention_days: u32,
    ) -> Result<LocalSpanStoreRequirement, TelemetryWriteModeError> {
        let local = self
            .count_projects_with_mode(CloudTelemetryWriteMode::Local)
            .await?;
        let cloud = self
            .count_projects_with_mode(CloudTelemetryWriteMode::Cloud)
            .await?;

        if local > 0 {
            return Ok(LocalSpanStoreRequirement {
                required: true,
                reason: Some(format!(
                    "{local} project{} still write{} spans to this instance. Every project must be \
                     Cloud-primary before the local span store can be decommissioned — one \
                     project in `local` mode keeps the whole store running.",
                    if local == 1 { "" } else { "s" },
                    if local == 1 { "s" } else { "" },
                )),
                local_mode_projects: local,
                cloud_primary_projects: cloud,
                local_history_until: None,
            });
        }

        // Every project is Cloud-primary. Local history from before the cutover
        // is still real data the operator paid to collect, and it stays
        // readable until it ages out of retention.
        #[derive(FromQueryResult)]
        struct LatestLocal {
            latest: Option<DBDateTime>,
        }
        let latest = LatestLocal::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT MAX(COALESCE(effective_to, NOW())) AS latest \
             FROM project_telemetry_write_intervals \
             WHERE mode = 'local' \
               AND COALESCE(effective_to, NOW()) > NOW() - ($1 * INTERVAL '1 day')",
            vec![(retention_days as i64).into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?
        .and_then(|row| row.latest);

        match latest {
            Some(until) => Ok(LocalSpanStoreRequirement {
                required: true,
                reason: Some(format!(
                    "All {cloud} project{} are Cloud-primary, but local span history from before \
                     the cutover is still within this instance's {retention_days}-day retention \
                     (through {}). Decommissioning the local span store now destroys that history \
                     permanently.",
                    if cloud == 1 { "" } else { "s" },
                    until.format("%-d %b %Y"),
                )),
                local_mode_projects: 0,
                cloud_primary_projects: cloud,
                local_history_until: Some(until),
            }),
            None => Ok(LocalSpanStoreRequirement {
                required: false,
                reason: None,
                local_mode_projects: 0,
                cloud_primary_projects: cloud,
                local_history_until: None,
            }),
        }
    }

    // ── Internals ────────────────────────────────────────────────────────

    async fn count_projects_with_mode(
        &self,
        mode: CloudTelemetryWriteMode,
    ) -> Result<u64, TelemetryWriteModeError> {
        let row = ModeCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM projects \
             WHERE cloud_telemetry_write_mode = $1 AND deleted_at IS NULL",
            vec![mode.to_string().into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| TelemetryWriteModeError::Ledger { source })?;
        Ok(row.map_or(0, |row| row.n.max(0) as u64))
    }

    async fn project_row(
        &self,
        project_id: i32,
    ) -> Result<ProjectModeRow, TelemetryWriteModeError> {
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::CloudTelemetryFidelity)
            .column(projects::Column::CloudTelemetryWriteMode)
            .column(projects::Column::CloudAnalyticsWriteMode)
            .column(projects::Column::CloudTelemetryAttributeAllowlist)
            .filter(projects::Column::Id.eq(project_id))
            // Soft-deleted projects are "gone" to an operator naming one, and
            // silently editing a deleted project's telemetry settings would be
            // a write nobody can see the result of.
            .filter(projects::Column::DeletedAt.is_null())
            .into_model::<ProjectModeRow>()
            .one(self.db.as_ref())
            .await
            .map_err(|source| TelemetryWriteModeError::Read { project_id, source })?
            .ok_or(TelemetryWriteModeError::ProjectNotFound { project_id })
    }

    async fn persist_mode_and_interval(
        &self,
        project_id: i32,
        mode: CloudTelemetryWriteMode,
        reason: TelemetryWriteIntervalReason,
    ) -> Result<(), TelemetryWriteModeError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| TelemetryWriteModeError::Write { project_id, source })?;

        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET cloud_telemetry_write_mode = $2 WHERE id = $1",
            vec![project_id.into(), mode.to_string().into()],
        ))
        .await
        .map_err(|source| TelemetryWriteModeError::Write { project_id, source })?;

        close_and_open_intervals(&txn, &[project_id], mode, reason).await?;

        txn.commit()
            .await
            .map_err(|source| TelemetryWriteModeError::Write { project_id, source })
    }

    async fn persist_analytics_mode_and_interval(
        &self,
        project_id: i32,
        mode: CloudAnalyticsWriteMode,
        reason: TelemetryWriteIntervalReason,
    ) -> Result<(), TelemetryWriteModeError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| TelemetryWriteModeError::Write { project_id, source })?;

        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET cloud_analytics_write_mode = $2 WHERE id = $1",
            vec![project_id.into(), mode.to_string().into()],
        ))
        .await
        .map_err(|source| TelemetryWriteModeError::Write { project_id, source })?;

        // Map CloudAnalyticsWriteMode → CloudTelemetryWriteMode for the shared
        // interval ledger. The ledger table uses CloudTelemetryWriteMode for
        // both signal groups because the binary local/cloud split is the same.
        let ledger_mode = if mode.is_cloud_primary() {
            CloudTelemetryWriteMode::Cloud
        } else {
            CloudTelemetryWriteMode::Local
        };
        close_and_open_analytics_intervals(&txn, &[project_id], ledger_mode, reason).await?;

        txn.commit()
            .await
            .map_err(|source| TelemetryWriteModeError::Write { project_id, source })
    }

    fn invalidate(&self, project_id: i32) {
        if let Some(cache) = &self.policy_cache {
            cache.invalidate(project_id);
        }
    }
}

/// Close each project's open span-group interval and open a new one, inside
/// the caller's transaction.
///
/// Skips projects whose open interval already has the requested mode, so a
/// repeated call is a no-op rather than a zero-length interval — the ledger is
/// read as "where did spans go between these two instants", and a run of empty
/// intervals makes that unreadable.
async fn close_and_open_intervals<C: ConnectionTrait>(
    txn: &C,
    project_ids: &[i32],
    mode: CloudTelemetryWriteMode,
    reason: TelemetryWriteIntervalReason,
) -> Result<(), TelemetryWriteModeError> {
    close_and_open_intervals_for_group(txn, project_ids, mode, reason, TelemetrySignalGroup::Spans)
        .await
}

/// Close each project's open analytics-group interval and open a new one.
async fn close_and_open_analytics_intervals<C: ConnectionTrait>(
    txn: &C,
    project_ids: &[i32],
    mode: CloudTelemetryWriteMode,
    reason: TelemetryWriteIntervalReason,
) -> Result<(), TelemetryWriteModeError> {
    close_and_open_intervals_for_group(
        txn,
        project_ids,
        mode,
        reason,
        TelemetrySignalGroup::Analytics,
    )
    .await
}

async fn close_and_open_intervals_for_group<C: ConnectionTrait>(
    txn: &C,
    project_ids: &[i32],
    mode: CloudTelemetryWriteMode,
    reason: TelemetryWriteIntervalReason,
    group: TelemetrySignalGroup,
) -> Result<(), TelemetryWriteModeError> {
    if project_ids.is_empty() {
        return Ok(());
    }

    // Close every open interval for this signal_group that disagrees with the
    // requested mode. The signal_group filter is required: without it, closing
    // a span interval would also close an open analytics interval for the same
    // project, and vice versa.
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE project_telemetry_write_intervals \
         SET effective_to = NOW() \
         WHERE project_id = ANY($1) AND signal_group = $3 \
           AND effective_to IS NULL AND mode <> $2",
        vec![
            project_ids.to_vec().into(),
            mode.to_string().into(),
            group.to_string().into(),
        ],
    ))
    .await
    .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

    // Open one for every project that now has none for this signal_group. The
    // partial unique index on `(project_id, signal_group) WHERE effective_to IS
    // NULL` is what makes this safe under a concurrent writer.
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO project_telemetry_write_intervals \
             (project_id, signal_group, mode, effective_from, effective_to, reason) \
         SELECT id, $4, $2, NOW(), NULL, $3 FROM UNNEST($1::int[]) AS t(id) \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM project_telemetry_write_intervals open \
             WHERE open.project_id = t.id AND open.signal_group = $4 \
               AND open.effective_to IS NULL \
         )",
        vec![
            project_ids.to_vec().into(),
            mode.to_string().into(),
            reason.to_string().into(),
            group.to_string().into(),
        ],
    ))
    .await
    .map_err(|source| TelemetryWriteModeError::Ledger { source })?;

    Ok(())
}

/// `None` for an instant PostgreSQL's `timestamptz` cannot hold, so the caller
/// can bind it as "unbounded on that side" instead of as a value the database
/// will reject.
///
/// This is not hypothetical: `CloudRoutedOtelStorage` deliberately uses
/// `DateTime::<Utc>::MIN_UTC` (year −262143) to mean "everything you have" for a
/// query with no lower bound, which is thousands of years outside the column's
/// range. Binding it verbatim turns every unbounded trace query into a ledger
/// read failure — and a ledger read failure resolves to *local*, so a
/// Cloud-primary project's traces would silently come back empty.
fn bindable_instant(value: DBDateTime) -> Option<DBDateTime> {
    use chrono::Datelike;
    // PostgreSQL's `timestamptz` spans 4713 BC to 294276 AD. This is a
    // deliberately conservative subset of it: no real interval boundary falls
    // outside year 1..=9999, and anything that does is a sentinel, not a date.
    (value.year() >= 1 && value.year() <= 9999).then_some(value)
}

/// Resolve a read window against a project's intervals (ADR-041 §8).
///
/// Pure, so the clamping rule — the part ADR-040 §3's no-merge contract rests
/// on — is testable without a database.
///
/// Rules, in order:
/// 1. No ledger at all → `Local`. A project that never cut over has always
///    written locally, and inventing a Cloud interval for it would send the
///    query to a store that has never held its spans.
/// 2. Window entirely inside one interval → that interval's mode, unclamped.
/// 3. Window straddles → clamp to the **newest** interval it touches and report
///    where it was cut. Newest rather than oldest because a user asking for
///    "the last 24 hours" during a cutover wants the data that exists now, and
///    because the clamp is visible either way.
pub fn resolve_window(
    intervals: &[write_intervals::Model],
    from: DBDateTime,
    to: DBDateTime,
) -> WindowResolution {
    // Intervals that overlap the requested window at all.
    let mut touched: Vec<&write_intervals::Model> = intervals
        .iter()
        .filter(|interval| {
            let ends = interval.effective_to.unwrap_or(DBDateTime::MAX_UTC);
            interval.effective_from <= to && ends >= from
        })
        .collect();

    if touched.is_empty() {
        return WindowResolution {
            source: CloudTelemetryWriteMode::Local,
            window_clamped_at: None,
            from,
            to,
        };
    }

    touched.sort_by_key(|interval| interval.effective_from);
    let newest = touched
        .last()
        .copied()
        .expect("touched is non-empty, checked above");

    // Distinct modes across the touched intervals decide whether this is a
    // straddle. Two adjacent `local` intervals (an operator flipped away and
    // back) are not a straddle: both answers come from the same store.
    let straddles = touched.iter().any(|interval| interval.mode != newest.mode);

    if !straddles {
        return WindowResolution {
            source: newest.mode,
            window_clamped_at: None,
            from,
            to,
        };
    }

    let clamp_at = newest.effective_from.max(from);
    WindowResolution {
        source: newest.mode,
        window_clamped_at: Some(clamp_at),
        from: clamp_at,
        to,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};

    fn interval(
        id: i64,
        mode: CloudTelemetryWriteMode,
        from_mins_ago: i64,
        to_mins_ago: Option<i64>,
    ) -> write_intervals::Model {
        let now = Utc::now();
        write_intervals::Model {
            id,
            project_id: 7,
            signal_group: TelemetrySignalGroup::Spans,
            mode,
            effective_from: now - ChronoDuration::minutes(from_mins_ago),
            effective_to: to_mins_ago.map(|mins| now - ChronoDuration::minutes(mins)),
            reason: TelemetryWriteIntervalReason::Operator,
        }
    }

    fn mins_ago(mins: i64) -> DBDateTime {
        Utc::now() - ChronoDuration::minutes(mins)
    }

    // ── §8: window resolution never merges two sources ───────────────────

    #[test]
    fn a_project_with_no_ledger_reads_locally() {
        // A project that never cut over has always written locally. Inventing a
        // Cloud interval for it would send the query to a store that has never
        // held one of its spans and return a confident, empty answer.
        let resolution = resolve_window(&[], mins_ago(60), Utc::now());
        assert_eq!(resolution.source, CloudTelemetryWriteMode::Local);
        assert!(resolution.window_clamped_at.is_none());
    }

    #[test]
    fn a_window_inside_one_interval_is_not_clamped() {
        let intervals = vec![interval(1, CloudTelemetryWriteMode::Cloud, 240, None)];
        let resolution = resolve_window(&intervals, mins_ago(60), Utc::now());

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Cloud);
        assert!(
            resolution.window_clamped_at.is_none(),
            "a window that does not straddle must not report a clamp"
        );
    }

    #[test]
    fn a_window_straddling_two_modes_is_clamped_to_the_newest() {
        // The single most important behaviour in §8: the answer names one
        // source and says exactly where it stopped, rather than stitching two
        // stores into one page the badge cannot label.
        let intervals = vec![
            interval(1, CloudTelemetryWriteMode::Local, 240, Some(120)),
            interval(2, CloudTelemetryWriteMode::Cloud, 120, None),
        ];
        let resolution = resolve_window(&intervals, mins_ago(200), Utc::now());

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Cloud);
        let clamped = resolution
            .window_clamped_at
            .expect("a straddle must report where it was cut");
        assert_eq!(clamped, intervals[1].effective_from);
        assert_eq!(
            resolution.from, clamped,
            "the served window must start at the clamp, not at the request"
        );
    }

    #[test]
    fn a_window_entirely_in_the_older_interval_serves_that_one() {
        let intervals = vec![
            interval(1, CloudTelemetryWriteMode::Local, 240, Some(120)),
            interval(2, CloudTelemetryWriteMode::Cloud, 120, None),
        ];
        let resolution = resolve_window(&intervals, mins_ago(220), mins_ago(180));

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Local);
        assert!(resolution.window_clamped_at.is_none());
    }

    #[test]
    fn two_adjacent_intervals_of_the_same_mode_are_not_a_straddle() {
        // An operator who flipped away and back, or a quota fallback that
        // resolved, produces several intervals with the same destination.
        // Clamping there would truncate a query for no reason at all.
        let intervals = vec![
            interval(1, CloudTelemetryWriteMode::Local, 240, Some(180)),
            interval(2, CloudTelemetryWriteMode::Local, 180, None),
        ];
        let resolution = resolve_window(&intervals, mins_ago(220), Utc::now());

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Local);
        assert!(resolution.window_clamped_at.is_none());
    }

    #[test]
    fn a_straddle_across_three_intervals_still_names_exactly_one_source() {
        let intervals = vec![
            interval(1, CloudTelemetryWriteMode::Local, 300, Some(240)),
            interval(2, CloudTelemetryWriteMode::Cloud, 240, Some(60)),
            interval(3, CloudTelemetryWriteMode::Local, 60, None),
        ];
        let resolution = resolve_window(&intervals, mins_ago(280), Utc::now());

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Local);
        assert_eq!(
            resolution.window_clamped_at,
            Some(intervals[2].effective_from)
        );
    }

    #[test]
    fn the_clamp_never_moves_the_window_start_backwards() {
        // If the request already begins inside the newest interval there is
        // nothing to clamp *to* — reporting the interval's start would widen
        // the window the caller asked for.
        let intervals = vec![
            interval(1, CloudTelemetryWriteMode::Local, 240, Some(120)),
            interval(2, CloudTelemetryWriteMode::Cloud, 120, None),
        ];
        let resolution = resolve_window(&intervals, mins_ago(30), Utc::now());

        assert_eq!(resolution.source, CloudTelemetryWriteMode::Cloud);
        assert!(resolution.window_clamped_at.is_none());
        assert!(resolution.from >= intervals[1].effective_from);
    }

    // ── §7b: suspension mapping ──────────────────────────────────────────

    #[test]
    fn each_suspension_opens_the_ledger_interval_that_explains_it() {
        assert_eq!(
            CloudWriteSuspension::QuotaExhausted.interval_reason(),
            TelemetryWriteIntervalReason::QuotaExhausted
        );
        assert_eq!(
            CloudWriteSuspension::CredentialRejected.interval_reason(),
            TelemetryWriteIntervalReason::CredentialRejected
        );
        assert_eq!(
            CloudWriteSuspension::QueueOverflow.interval_reason(),
            TelemetryWriteIntervalReason::QueueOverflowSpill
        );
    }

    #[test]
    fn only_a_real_suspension_counts_as_suspended() {
        assert!(!CloudWriteSuspension::None.is_suspended());
        assert!(CloudWriteSuspension::QuotaExhausted.is_suspended());
        assert!(CloudWriteSuspension::CredentialRejected.is_suspended());
        assert!(CloudWriteSuspension::QueueOverflow.is_suspended());
    }

    #[test]
    fn an_unknown_suspension_discriminant_reads_as_not_suspended() {
        // The atomic is a `u8`; an out-of-range value must never be read as a
        // suspension nobody can clear.
        assert_eq!(
            CloudWriteSuspension::from_u8(200),
            CloudWriteSuspension::None
        );
    }

    // ── The gate's messages ──────────────────────────────────────────────

    #[test]
    fn each_refusal_names_a_different_prerequisite_and_its_fix() {
        // Three unrelated fixes. An operator who reads a generic "cannot enable"
        // has to guess which of the three they need.
        let fidelity = TelemetryWriteModeError::FidelityTooLow {
            project_id: 7,
            fidelity: CloudTelemetryFidelity::Metered,
        }
        .to_string();
        assert!(fidelity.contains("queryable"), "{fidelity}");
        assert!(
            fidelity.contains("project 7") || fidelity.contains("Project 7"),
            "{fidelity}"
        );

        let unlinked = TelemetryWriteModeError::NotLinked {
            project_id: 7,
            setup_path: CLOUD_SETUP_PATH,
        }
        .to_string();
        assert!(unlinked.contains("not linked"), "{unlinked}");
        assert!(unlinked.contains(CLOUD_SETUP_PATH), "{unlinked}");

        let switched_off = TelemetryWriteModeError::TelemetryExportDisabled {
            project_id: 7,
            setup_path: CLOUD_SETUP_PATH,
        }
        .to_string();
        assert!(switched_off.contains("switched off"), "{switched_off}");
        assert!(switched_off.contains(CLOUD_SETUP_PATH), "{switched_off}");

        // All three must be distinguishable from each other, not just non-empty.
        assert_ne!(fidelity, unlinked);
        assert_ne!(unlinked, switched_off);
        assert_ne!(fidelity, switched_off);
    }

    #[test]
    fn a_straddle_is_still_detected_when_the_older_interval_is_far_back() {
        // The reason `resolve_read_window` fetches by *range* rather than by a
        // `LIMIT`-bounded newest-first list. Quota suspend/resume opens and
        // closes intervals on its own (§7b), so a project on a flapping
        // allowance accumulates hundreds without anyone touching a setting. Past
        // a limit, the oldest ones fall out of the fetched set — and a window
        // that genuinely straddles the cutover then looks like it sits inside
        // the newest interval, so it is served from Cloud, *unclamped*, as a
        // complete answer for a period that predates the cutover.
        let mut intervals = vec![interval(
            1,
            CloudTelemetryWriteMode::Local,
            100_000,
            Some(600),
        )];
        // 300 short `cloud` intervals since, of the shape a flapping quota
        // produces — more than any fixed `LIMIT` this code used to apply.
        for i in 0..300 {
            let from = 600 - i * 2;
            intervals.push(interval(
                2 + i,
                CloudTelemetryWriteMode::Cloud,
                from,
                if i == 299 { None } else { Some(from - 1) },
            ));
        }

        let resolution = resolve_window(&intervals, mins_ago(90_000), Utc::now());
        assert_eq!(resolution.source, CloudTelemetryWriteMode::Cloud);
        assert!(
            resolution.window_clamped_at.is_some(),
            "a window reaching back into the pre-cutover local interval must be clamped, \
             however many intervals the project has accumulated since"
        );
    }

    #[test]
    fn the_sentinel_the_read_decorator_uses_for_an_unbounded_window_is_not_bound_verbatim() {
        // `CloudRoutedOtelStorage` passes `MIN_UTC` to mean "everything you
        // have". PostgreSQL cannot hold it, and binding it would turn every
        // unbounded trace query into a ledger read failure — which resolves to
        // *local*, so a Cloud-primary project's traces would come back silently
        // empty.
        assert!(bindable_instant(DBDateTime::MIN_UTC).is_none());
        assert!(bindable_instant(DBDateTime::MAX_UTC).is_none());
        let now = Utc::now();
        assert_eq!(bindable_instant(now), Some(now));
    }

    #[test]
    fn the_queued_span_refusal_names_the_depth_and_where_to_watch_it() {
        // "Try again later" with no number is a dead end. The operator needs to
        // know whether they are waiting on twelve spans or on a queue that is
        // not moving.
        let message = TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
            project_id: 7,
            requested: CloudTelemetryFidelity::Metered,
            queued_spans: 4_096,
            spill_blocked_reason: "the local span store did not accept them",
        }
        .to_string();

        assert!(message.contains("4096"), "{message}");
        assert!(message.contains("Project 7"), "{message}");
        assert!(
            message.contains("would not stop them being sent"),
            "the refusal must say why lowering the fidelity now is not enough: {message}"
        );
        assert!(
            message.contains("status page"),
            "must point at where the queue depth is visible: {message}"
        );
    }

    #[test]
    fn the_two_downgrade_refusals_are_distinguishable() {
        // They have different fixes: one is "change the write mode", the other
        // is "wait". A shared message would send an operator to change a setting
        // that is already correct.
        let by_mode = TelemetryWriteModeError::FidelityDowngradeBlockedByWriteMode {
            project_id: 7,
            requested: CloudTelemetryFidelity::Metered,
        }
        .to_string();
        let by_queue = TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
            project_id: 7,
            requested: CloudTelemetryFidelity::Metered,
            queued_spans: 3,
            spill_blocked_reason: "the local span store did not accept them",
        }
        .to_string();
        assert_ne!(by_mode, by_queue);
    }

    #[test]
    fn the_downgrade_refusal_names_the_write_mode_as_the_thing_to_change() {
        let message = TelemetryWriteModeError::FidelityDowngradeBlockedByWriteMode {
            project_id: 7,
            requested: CloudTelemetryFidelity::Metered,
        }
        .to_string();

        assert!(
            message.contains("write mode"),
            "must name the write mode, not just refuse: {message}"
        );
        assert!(
            message.contains("`local`"),
            "must say what to set it to: {message}"
        );
        assert!(message.contains("cloud"), "{message}");
    }

    #[test]
    fn a_link_that_cannot_accept_spans_is_recognised_in_every_broken_state() {
        assert!(CloudLinkSnapshot {
            linked: true,
            telemetry_enabled: true,
            credential_rejected: false
        }
        .can_accept_spans());

        for snapshot in [
            CloudLinkSnapshot::default(),
            CloudLinkSnapshot {
                linked: true,
                telemetry_enabled: false,
                credential_rejected: false,
            },
            CloudLinkSnapshot {
                linked: true,
                telemetry_enabled: true,
                credential_rejected: true,
            },
        ] {
            assert!(
                !snapshot.can_accept_spans(),
                "{snapshot:?} must not be treated as able to accept spans"
            );
        }
    }
}
