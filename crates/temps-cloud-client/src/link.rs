// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The object a running instance holds.
//!
//! Owns the link state, the spool and the HTTP client, and exposes the two
//! operations the rest of the instance needs: [`CloudLink::record`], which must
//! never block or fail, and [`CloudLink::flush`], which a background task calls
//! on an interval.
//!
//! # Why `record` cannot fail
//!
//! It is called from wherever the instance already produces telemetry. If it
//! could return an error, every call site would need a decision about what to
//! do — and one of them would eventually decide to propagate it, which would
//! make an outage in *our* backend into an incident in the operator's
//! application. So it takes `&self`, returns `()`, and the worst it can do is
//! silently... no: the worst it can do is *count a drop the operator can see*.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{mpsc, watch};

use temps_cloud_protocol::{
    BackupArtifact, BackupTarget, ManagedAiAnalysisRequest, ManagedAiAnalysisResponse,
    ManagedAiCapability, ManagedAiChatRequest, ManagedAiChatResponse, ManagedBackupCapability,
    ManagedNotificationAccepted, ManagedNotificationRequest, NativeSnapshot, NativeSnapshotRequest,
    SpanRecord, WalGObjectCompleted, WalGObjectTarget, WalGObjectTargetRequest, WalGSnapshot,
    WalGSnapshotCompleted, WalGSnapshotRequest,
};
use uuid::Uuid;

use crate::spool::Spool;
use crate::state::{EnrollmentState, PendingSubmission};
use crate::status::{LinkStatus, MirrorHealth};
use crate::{BackendUrl, CloudClient, CloudError, CloudFeatureSwitches};

/// Spans per shipment. Small enough that one failure loses little progress.
const BATCH_SIZE: usize = 500;
/// Number of producer batches accepted before the mirror starts shedding load.
/// The local telemetry store remains authoritative and is never affected.
const INCOMING_BATCH_CAPACITY: usize = 8;
/// Spans an open [`SubmissionScope`] may hold before it starts shedding.
///
/// Two shipments' worth. A scoped caller offers one batch and drains it before
/// offering the next, so this is headroom rather than a working set, and it
/// bounds what such a caller can cost a 4 GB instance.
const SCOPE_SPOOL_CAPACITY: usize = BATCH_SIZE * 2;

/// What a flush attempt did. Returned so a caller can log or schedule backoff.
#[derive(Debug, Clone, PartialEq)]
pub enum FlushOutcome {
    /// Nothing buffered.
    Idle,
    /// Not linked, so there is nothing to mirror to.
    NotLinked,
    Shipped {
        spans: usize,
    },
    /// Kept for a later attempt.
    Retained {
        spans: usize,
        reason: String,
    },
    /// Shipment needs operator action, but the batch remains retained.
    Blocked {
        spans: usize,
        reason: String,
    },
}

/// Why Cloud-primary projects are being handed back to local span storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudFallbackReason {
    /// `DELETE /cloud` — the operator disconnected the instance.
    Disconnected,
    /// The Cloud telemetry feature switch was turned off.
    TelemetryDisabled,
}

/// The instance's Cloud-primary write-mode owner, called when the link goes
/// away (ADR-041 §7c).
///
/// # Why this is a trait on the link rather than a call in `temps-cloud`
///
/// The write mode, its ledger and the durable outbox all live in `temps-otel`,
/// which depends on this crate and not the other way round. The link is the one
/// object both sides already hold, so it is where the "the link is going away"
/// event can be delivered without inverting that dependency or making this
/// crate know what a project is.
///
/// The implementation is registered at startup and is absent on an instance
/// that never wires Cloud-primary writes, in which case a disconnect has
/// nothing to undo.
#[async_trait::async_trait]
pub trait CloudTelemetryFallback: Send + Sync {
    /// Hand every Cloud-primary project back to local span storage, and spill
    /// whatever is still queued into the local store rather than dropping it.
    ///
    /// Must be idempotent: it is called on disconnect and again, at most once,
    /// by the outbox worker after a feature-switch change.
    ///
    /// Returns how many projects' declared write mode was actually rewritten.
    /// The disconnect path reports that number back to the operator when the
    /// Cloud-side revoke afterwards fails: the flip is deliberately not undone
    /// (local storage is always safe), but a failed disconnect that silently
    /// changed several projects' settings is exactly the kind of thing a
    /// self-hosted operator has nobody to ask about.
    async fn revert_to_local(&self, reason: CloudFallbackReason) -> usize;
}

/// What a Cloud-primary outbox shipment did (ADR-041 §3).
///
/// Deliberately a separate type from [`FlushOutcome`] even though the variants
/// rhyme. The two paths settle differently — `flush` owns its queue and this
/// one does not — and collapsing them would let a caller ack an outbox row on
/// an outcome that only ever meant "the spool kept it".
#[derive(Debug, Clone, PartialEq)]
pub enum OutboxShipOutcome {
    /// Nothing was offered.
    Idle,
    /// Not linked, or telemetry export is switched off. The rows stay pending;
    /// the write-mode fallback (ADR-041 §7) is what resolves this, not a retry.
    NotLinked,
    Shipped {
        spans: usize,
        /// Cloud accepted the batch but the tenant is degraded — over quota and
        /// sampling, most importantly. Under Cloud-primary writes this is not
        /// informational: sampling would be sampling away the only copy, so the
        /// caller must act on it (ADR-041 §7b).
        warning: Option<temps_cloud_protocol::Unavailable>,
    },
    /// Transient failure. Retry on the backoff curve.
    Retained { spans: usize, reason: String },
    /// Refused for a reason retrying cannot fix. The rows are still kept —
    /// never infer from a 4xx that a customer's telemetry is disposable.
    Blocked { spans: usize, reason: String },
}

struct IncomingBatch {
    generation: u64,
    spans: Vec<SpanRecord>,
}

/// Refused because a submission scope is already open on this link.
///
/// Scoped submissions are deliberately serialized: two of them would
/// reintroduce exactly the counter conflation a scope exists to remove, and
/// ADR-041 §3b forbids concurrent in-flight submissions from one instance until
/// Cloud confirms `/v1/telemetry`'s idempotency and metering tolerate them.
#[derive(Debug, thiserror::Error)]
#[error(
    "a Temps Cloud submission scope is already open on this link; only one \
     scoped submission may be in flight at a time, so wait for the running one \
     to finish and retry"
)]
pub struct SubmissionScopeBusy;

/// What a successful [`CloudLink::enroll`] actually did to this instance's link.
///
/// # Why this exists
///
/// `enroll` is not first-time-only, and must not become so: overwriting the
/// token on an instance that is already linked is the *only* way an operator
/// recovers from `CredentialRejected` after a credential is revoked or rotated
/// at the backend. So "enrollment succeeded" and "this instance just became
/// linked" are different facts, and until this type existed there was no way to
/// tell them apart from the outside.
///
/// That mattered because enrollment carries automatic side effects, and one of
/// them spends money: ADR-042's purchase-triggered activation switches every
/// local-mode project to Cloud-primary and ships its history. Firing that on
/// every successful enroll means an operator re-authenticating a link they have
/// had for months silently re-activates projects they deliberately kept local
/// after the first activation — a decision they made once and cannot see being
/// undone.
///
/// The distinction is drawn on the **tenant**, not on the token, because that is
/// what "a link that did not exist before" actually means. A fresh token for the
/// same tenant is the same customer proving themselves again. A different tenant
/// is a different customer, and binding to one is a new link no matter what
/// stale credential happened to be on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentKind {
    /// No credential was held before this call: the link is new.
    First,
    /// A credential was already held, and it was for a *different* tenant than
    /// the one just enrolled — or for no recorded tenant at all, which a
    /// half-written legacy state can produce and which cannot be shown to be
    /// the same customer.
    ReboundToNewTenant {
        /// The tenant this instance was bound to before, when it recorded one.
        previous_tenant_id: Option<Uuid>,
    },
    /// A credential was already held for this same tenant: credential recovery,
    /// not a new link.
    ReEnrolled { tenant_id: Uuid },
}

impl EnrollmentKind {
    fn classify(was_linked: bool, previous_tenant_id: Option<Uuid>, new_tenant_id: Uuid) -> Self {
        if !was_linked {
            return Self::First;
        }
        match previous_tenant_id {
            Some(previous) if previous == new_tenant_id => Self::ReEnrolled {
                tenant_id: new_tenant_id,
            },
            previous => Self::ReboundToNewTenant {
                previous_tenant_id: previous,
            },
        }
    }

    /// Whether this enrollment established a link that did not exist before.
    ///
    /// The single question every side effect that belongs to *linking* should
    /// ask, so no caller has to re-derive it and get the `ReboundToNewTenant`
    /// case — a new customer on an instance carrying a stale credential —
    /// wrong.
    pub fn establishes_new_link(&self) -> bool {
        matches!(self, Self::First | Self::ReboundToNewTenant { .. })
    }

    /// A stable, greppable name for logs and audit records.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::First => "first",
            Self::ReboundToNewTenant { .. } => "rebound_to_new_tenant",
            Self::ReEnrolled { .. } => "re_enrolled",
        }
    }
}

/// The queue behind an open [`SubmissionScope`].
///
/// Owned by [`CloudLink`] rather than by the handle so every path that must
/// forget exportable data — a telemetry-export revocation, a disconnect, an
/// origin change — can clear it exactly as it clears the mirror spool. A
/// scope's spans are customer data under the same consent rules as any other.
struct ScopedSubmission {
    spool: Spool,
    /// Kept across retries so a resubmission reuses its `submission_id` and
    /// Cloud's idempotency still covers it.
    pending: Option<PendingSubmission>,
}

impl ScopedSubmission {
    fn new() -> Self {
        Self {
            spool: Spool::with_limits(SCOPE_SPOOL_CAPACITY, crate::spool::DEFAULT_CAPACITY_BYTES),
            pending: None,
        }
    }

    /// Spans offered to this scope that Cloud has not acknowledged yet.
    fn queued(&self) -> usize {
        self.spool.len()
            + self
                .pending
                .as_ref()
                .map_or(0, |pending| pending.spans.len())
    }

    fn clear(&mut self) {
        self.spool.clear();
        self.pending = None;
    }
}

pub struct CloudLink {
    state: RwLock<Option<EnrollmentState>>,
    /// Present when a state file existed but could not be decoded. Mutations
    /// stay blocked so recoverable credentials are never overwritten.
    unreadable_state_path: Option<String>,
    outbound_blocked_reason: RwLock<Option<String>>,
    incoming_tx: mpsc::Sender<IncomingBatch>,
    incoming_rx: Mutex<mpsc::Receiver<IncomingBatch>>,
    incoming_spans: AtomicUsize,
    incoming_dropped: AtomicU64,
    linked: AtomicBool,
    spool: Mutex<Spool>,
    /// The active submission stays here until a matching full acknowledgement
    /// arrives, preserving its id across retries.
    pending: Mutex<Option<PendingSubmission>>,
    /// The queue of the one open [`SubmissionScope`], if any (ADR-042 §2).
    ///
    /// `Some` is also the claim: [`CloudLink::submission_scope`] refuses while
    /// it is set, and dropping the handle clears it.
    scoped_submission: Mutex<Option<ScopedSubmission>>,
    health: RwLock<MirrorHealth>,
    state_path: PathBuf,
    agent_version: String,
    /// Set when the backend refuses our token. Distinct from mirror health:
    /// this one needs the operator, not time.
    credential_rejected: AtomicBool,
    generation: AtomicU64,
    /// Every telemetry revocation advances this channel. Flushes subscribe
    /// before starting I/O so disabling export drops the active HTTP future.
    telemetry_revocations: watch::Sender<u64>,
    flush_lock: tokio::sync::Mutex<()>,
    allow_loopback_development: bool,
    telemetry_enabled: AtomicBool,
    backups_enabled: AtomicBool,
    notifications_enabled: AtomicBool,
    encryption: Option<Arc<temps_core::EncryptionService>>,
    /// ADR-041 §7c. Set once at startup by whoever owns the write mode.
    telemetry_fallback: RwLock<Option<Arc<dyn CloudTelemetryFallback>>>,
    /// How many projects the most recent fallback handed back to local span
    /// storage.
    ///
    /// Read by the disconnect path when the Cloud-side revoke afterwards fails,
    /// so the error can name the settings change that already happened instead
    /// of leaving the operator to discover it in project settings later.
    telemetry_reverted_projects: AtomicUsize,
    /// Raised when telemetry export is switched off, so the outbox worker can
    /// run the (async) fallback that [`CloudLink::set_feature_switches`] cannot
    /// run itself.
    ///
    /// Correctness does not depend on how quickly this is observed: the ingest
    /// path checks `telemetry_enabled()` directly, so local span writes have
    /// already resumed by the time this flag is read. What the fallback adds is
    /// the ledger entry and the spill of anything still queued.
    telemetry_fallback_pending: AtomicBool,
}

impl CloudLink {
    /// Load from disk. An unlinked or absent state is a normal outcome, not an
    /// error — most instances never connect anything.
    pub fn load(data_dir: PathBuf, agent_version: impl Into<String>) -> Self {
        Self::load_inner(data_dir, agent_version, false, None)
    }

    pub fn load_encrypted(
        data_dir: PathBuf,
        agent_version: impl Into<String>,
        encryption: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self::load_inner(data_dir, agent_version, false, Some(encryption))
    }

    /// Local-test constructor. Production callers must use [`CloudLink::load`].
    pub fn load_for_loopback_development(
        data_dir: PathBuf,
        agent_version: impl Into<String>,
    ) -> Self {
        Self::load_inner(data_dir, agent_version, true, None)
    }

    pub fn load_encrypted_for_loopback_development(
        data_dir: PathBuf,
        agent_version: impl Into<String>,
        encryption: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self::load_inner(data_dir, agent_version, true, Some(encryption))
    }

    fn load_inner(
        data_dir: PathBuf,
        agent_version: impl Into<String>,
        allow_loopback_development: bool,
        encryption: Option<Arc<temps_core::EncryptionService>>,
    ) -> Self {
        // Credentials live in their own directory; state hardening must never
        // chmod an operator's shared TEMPS_DATA_DIR.
        let state_path = data_dir.join("cloud-link").join("state.json");
        let loaded = encryption.as_ref().map_or_else(
            || EnrollmentState::load(&state_path),
            |encryption| EnrollmentState::load_encrypted(&state_path, encryption),
        );
        let (state, unreadable_state_path) = match loaded {
            Ok(state) => (state, None),
            Err(error) => {
                // Corruption is reported, not silently reset: overwriting would
                // destroy a token the operator may still be able to recover.
                tracing::error!(%error, "link state unreadable; Cloud link mutations are blocked");
                (None, Some(state_path.display().to_string()))
            }
        };
        let linked = state.as_ref().is_some_and(EnrollmentState::is_linked);
        let pending_submission = state
            .as_ref()
            .and_then(|state| state.pending_submission.clone());
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_BATCH_CAPACITY);
        let (telemetry_revocations, _) = watch::channel(0);

        Self {
            state: RwLock::new(state),
            unreadable_state_path,
            outbound_blocked_reason: RwLock::new(None),
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
            incoming_spans: AtomicUsize::new(0),
            incoming_dropped: AtomicU64::new(0),
            linked: AtomicBool::new(linked),
            spool: Mutex::new(Spool::with_default_capacity()),
            pending: Mutex::new(pending_submission),
            scoped_submission: Mutex::new(None),
            health: RwLock::new(MirrorHealth::Healthy),
            state_path,
            agent_version: agent_version.into(),
            credential_rejected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            telemetry_revocations,
            flush_lock: tokio::sync::Mutex::new(()),
            allow_loopback_development,
            telemetry_enabled: AtomicBool::new(false),
            backups_enabled: AtomicBool::new(false),
            notifications_enabled: AtomicBool::new(false),
            encryption,
            telemetry_fallback: RwLock::new(None),
            telemetry_reverted_projects: AtomicUsize::new(0),
            telemetry_fallback_pending: AtomicBool::new(false),
        }
    }

    /// Register the owner of Cloud-primary write modes (ADR-041 §7c).
    pub fn set_telemetry_fallback(&self, fallback: Arc<dyn CloudTelemetryFallback>) {
        *self
            .telemetry_fallback
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fallback);
    }

    fn telemetry_fallback(&self) -> Option<Arc<dyn CloudTelemetryFallback>> {
        self.telemetry_fallback
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Run the fallback queued by a feature-switch change, if any.
    ///
    /// Called by the outbox worker on each cycle. Clears the flag *before*
    /// running so a concurrent switch-off queues another run rather than being
    /// swallowed by this one.
    pub async fn run_pending_telemetry_fallback(&self) {
        if !self
            .telemetry_fallback_pending
            .swap(false, Ordering::AcqRel)
        {
            return;
        }
        if let Some(fallback) = self.telemetry_fallback() {
            let reverted = fallback
                .revert_to_local(CloudFallbackReason::TelemetryDisabled)
                .await;
            self.telemetry_reverted_projects
                .store(reverted, Ordering::Release);
        }
    }

    /// Projects the most recent fallback handed back to local span storage.
    ///
    /// Zero until one has run. Only meaningful immediately after
    /// [`Self::revoke`] or [`Self::run_pending_telemetry_fallback`]; it is a
    /// last-outcome record for the error path, not a running total.
    pub fn telemetry_projects_reverted(&self) -> usize {
        self.telemetry_reverted_projects.load(Ordering::Acquire)
    }

    fn save_state(&self, state: &EnrollmentState) -> Result<(), crate::state::StateError> {
        match &self.encryption {
            Some(encryption) => state.save_encrypted(&self.state_path, encryption),
            None => state.save(&self.state_path),
        }
    }

    fn persist_pending(
        &self,
        pending_submission: Option<PendingSubmission>,
    ) -> Result<(), crate::state::StateError> {
        let mut guard = self.state.write().unwrap_or_else(|p| p.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| crate::state::StateError::Corrupt {
                path: self.state_path.display().to_string(),
                reason: "cannot persist a telemetry retry without link state".into(),
            })?;
        let mut next = state.clone();
        next.pending_submission = pending_submission;
        self.save_state(&next)?;
        *guard = Some(next);
        Ok(())
    }

    fn ensure_state_readable(&self) -> Result<(), crate::state::StateError> {
        match &self.unreadable_state_path {
            Some(path) => {
                Err(crate::state::StateError::UnreadableStateBlocksMutation { path: path.clone() })
            }
            None => Ok(()),
        }
    }

    fn unreadable_cloud_error(&self) -> Option<CloudError> {
        self.unreadable_state_path
            .as_ref()
            .map(|path| CloudError::LinkStateUnreadable { path: path.clone() })
    }

    fn outbound_blocked_error(&self) -> Option<CloudError> {
        self.outbound_blocked_reason
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|reason| CloudError::ConfigurationBlocked {
                reason: reason.clone(),
            })
    }

    /// Block outbound submissions and attribute the block as the reason for
    /// any span that's already been, or is about to be, dropped.
    ///
    /// Without this, a block that fires before the flusher's next tick (e.g.
    /// startup reconciliation failing before any flush ever runs) leaves
    /// `self.health` at its default value while producer-side drops still
    /// accumulate. [`Self::health`]'s fallback then has no real reason to
    /// report and falls back to a placeholder that tells the operator nothing
    /// — exactly the failure mode `Dropping.reason` exists to prevent.
    pub fn block_outbound(&self, reason: impl Into<String>) {
        let reason = reason.into();
        *self
            .outbound_blocked_reason
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason.clone());
        let (spooled, dropped) = {
            let spool = self
                .spool
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                spool.len(),
                spool
                    .dropped()
                    .saturating_add(self.incoming_dropped.load(Ordering::Relaxed)),
            )
        };
        *self
            .health
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = if dropped > 0 {
            MirrorHealth::Dropping {
                spooled,
                dropped,
                reason,
            }
        } else {
            MirrorHealth::Buffering { spooled, reason }
        };
    }

    fn clear_outbound_block(&self) {
        *self
            .outbound_blocked_reason
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Apply persisted, operator-controlled export settings. This is lock-free
    /// on telemetry producers and deliberately independent of enrollment.
    pub fn set_feature_switches(
        &self,
        switches: CloudFeatureSwitches,
    ) -> Result<(), crate::state::StateError> {
        // Disable first, before taking any locks, so producers stop accepting
        // new export work immediately.
        let telemetry_was_enabled = self
            .telemetry_enabled
            .swap(switches.telemetry, Ordering::AcqRel);
        self.backups_enabled
            .store(switches.backups, Ordering::Release);
        self.notifications_enabled
            .store(switches.notifications, Ordering::Release);
        if switches.telemetry {
            return Ok(());
        }

        if telemetry_was_enabled {
            // ADR-041 §7c: Cloud-primary projects must go back to local span
            // storage. Local writes have *already* resumed — the ingest path
            // reads `telemetry_enabled()` directly and this store happened
            // above, before any lock was taken — so what is queued here is the
            // ledger entry and the spill of anything still in the outbox, both
            // of which need async work this synchronous method cannot do.
            self.telemetry_fallback_pending
                .store(true, Ordering::Release);
        }

        if telemetry_was_enabled {
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.telemetry_revocations.send_replace(generation);
        }

        // Persist the deletion before returning success. If persistence fails,
        // callers block outbound Cloud operations and surface the state error.
        let persistence = {
            let mut state = self.state.write().unwrap_or_else(|p| p.into_inner());
            match state.as_ref() {
                Some(current) if current.pending_submission.is_some() => {
                    let mut next = current.clone();
                    next.pending_submission = None;
                    let result = self.save_state(&next);
                    // Even on a disk failure, do not keep the revoked payload
                    // live in this process.
                    *state = Some(next);
                    result
                }
                _ => Ok(()),
            }
        };

        {
            let mut receiver = self.incoming_rx.lock().unwrap_or_else(|p| p.into_inner());
            while let Ok(batch) = receiver.try_recv() {
                self.incoming_spans
                    .fetch_sub(batch.spans.len(), Ordering::Relaxed);
            }
        }
        self.spool.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        // Consent revocation must not leave exportable customer data resident
        // anywhere in the process, including in an open submission scope.
        self.clear_scoped_submission();
        *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
        persistence
    }

    pub fn feature_switches(&self) -> CloudFeatureSwitches {
        CloudFeatureSwitches {
            telemetry: self.telemetry_enabled.load(Ordering::Acquire),
            backups: self.backups_enabled.load(Ordering::Acquire),
            notifications: self.notifications_enabled.load(Ordering::Acquire),
        }
    }

    pub fn backups_enabled(&self) -> bool {
        self.backups_enabled.load(Ordering::Acquire)
    }

    pub fn telemetry_enabled(&self) -> bool {
        self.telemetry_enabled.load(Ordering::Acquire)
    }

    pub fn notifications_enabled(&self) -> bool {
        self.notifications_enabled.load(Ordering::Acquire)
    }

    pub fn notifications_available(&self) -> bool {
        self.notifications_enabled()
            && self.linked.load(Ordering::Acquire)
            && !self.credential_rejected.load(Ordering::Acquire)
    }

    /// Stable per-link pseudonym for identifiers that must remain joinable in
    /// Cloud without disclosing the local trace/span identifiers.
    pub fn pseudonymize_telemetry_id(
        &self,
        domain: &'static str,
        value: &str,
    ) -> Result<String, CloudError> {
        if !self.telemetry_enabled() {
            return Err(CloudError::FeatureDisabled {
                feature: "telemetry",
            });
        }
        self.pseudonymize_linked_id(domain, value)
    }

    pub fn pseudonymize_notification_id(&self, value: &str) -> Result<String, CloudError> {
        if !self.notifications_enabled() {
            return Err(CloudError::FeatureDisabled {
                feature: "notifications",
            });
        }
        self.pseudonymize_linked_id("notification", value)
    }

    fn pseudonymize_linked_id(
        &self,
        domain: &'static str,
        value: &str,
    ) -> Result<String, CloudError> {
        use hmac::{Hmac, KeyInit, Mac};
        let (_, token) = self.linked_credential()?;
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes()).map_err(|error| {
            CloudError::ClientConfiguration {
                reason: format!("could not initialize telemetry identifier HMAC: {error}"),
            }
        })?;
        mac.update(domain.as_bytes());
        mac.update(&[0]);
        mac.update(value.as_bytes());
        Ok(mac.finalize().into_bytes().iter().fold(
            String::with_capacity(64),
            |mut output, byte| {
                use std::fmt::Write;
                let _ = write!(output, "{byte:02x}");
                output
            },
        ))
    }

    pub(crate) fn parse_backend(&self, value: &str) -> Result<BackendUrl, CloudError> {
        if self.allows_loopback_development() {
            BackendUrl::loopback_development(value)
        } else {
            BackendUrl::production(value)
        }
    }

    pub fn allows_loopback_development(&self) -> bool {
        self.allow_loopback_development
            || self
                .state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|state| state.allow_loopback_development)
    }

    pub fn status(&self) -> LinkStatus {
        if let Some(state_path) = &self.unreadable_state_path {
            return LinkStatus::StateUnreadable {
                state_path: state_path.clone(),
            };
        }
        match &*self.state.read().unwrap_or_else(|p| p.into_inner()) {
            None => LinkStatus::NotConfigured,
            Some(s) if s.is_linked() => {
                let base_url = s.base_url.clone();
                // A token that still exists but is no longer accepted is its
                // own state: the operator must re-enroll, and no amount of
                // waiting will fix it. Reporting it as plain `Linked` would
                // leave them watching a spool that never drains.
                if self.credential_rejected.load(Ordering::SeqCst) {
                    LinkStatus::CredentialRejected { base_url }
                } else {
                    LinkStatus::Linked { base_url }
                }
            }
            Some(s) => LinkStatus::AwaitingEnrollment {
                base_url: s.base_url.clone(),
            },
        }
    }

    pub fn health(&self) -> MirrorHealth {
        let spool_dropped = self
            .spool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .dropped();
        let dropped = spool_dropped.saturating_add(self.incoming_dropped.load(Ordering::Relaxed));
        if dropped > 0 {
            // Refresh the counts but keep whatever reason the last delivery
            // attempt (or producer backpressure, which never sees one) left
            // behind — never overwrite it with a blank one.
            let reason = match &*self.health.read().unwrap_or_else(|p| p.into_inner()) {
                MirrorHealth::Dropping { reason, .. } | MirrorHealth::Buffering { reason, .. } => {
                    reason.clone()
                }
                _ => "spans were discarded before a mirror delivery attempt could report why"
                    .to_string(),
            };
            return MirrorHealth::Dropping {
                spooled: self.spooled(),
                dropped,
                reason,
            };
        }
        self.health
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn instance_id(&self) -> Option<Uuid> {
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|s| s.instance_id)
    }

    pub fn tenant_id(&self) -> Option<Uuid> {
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|state| state.tenant_id)
    }

    pub fn account_email(&self) -> Option<String> {
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|state| state.account_email.clone())
    }

    /// Lock-free fast-path hint for telemetry producers. Enrollment can race
    /// with an offer; generation tagging prevents a raced batch crossing links.
    pub fn is_linked(&self) -> bool {
        self.linked.load(Ordering::Acquire)
    }

    /// Point this instance at a backend without linking it yet.
    pub fn configure(&self, backend: BackendUrl) -> Result<(), crate::state::StateError> {
        self.ensure_state_readable()?;
        let mut guard = self.state.write().unwrap_or_else(|p| p.into_inner());
        let next_url = backend.as_str().to_string();
        let next_allows_loopback = backend.allows_loopback_development();
        if let Some(existing) = guard.as_ref() {
            if existing.is_linked() && existing.base_url != next_url {
                return Err(crate::state::StateError::BackendChangeRequiresDisconnect {
                    current: existing.base_url.clone(),
                    requested: next_url,
                });
            }
        }
        let mut changed_origin = false;
        let next = match guard.as_ref() {
            Some(existing) => {
                let mut existing = existing.clone();
                if existing.base_url != next_url {
                    changed_origin = true;
                    // Credentials are origin-bound. Keeping a token while
                    // changing its destination would exfiltrate it on flush.
                    // Buffered telemetry is origin-bound for the same reason.
                    existing.unlink();
                }
                existing.base_url = next_url;
                existing.allow_loopback_development = next_allows_loopback;
                existing
            }
            None => {
                let mut state = EnrollmentState::new(next_url);
                state.allow_loopback_development = next_allows_loopback;
                state
            }
        };
        self.save_state(&next)?;
        if changed_origin {
            self.linked.store(false, Ordering::Release);
            self.spool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take(usize::MAX);
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            // Buffered telemetry is origin-bound, and a scope's queue is no
            // exception.
            self.clear_scoped_submission();
            self.credential_rejected.store(false, Ordering::SeqCst);
            *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
        }
        *guard = Some(next);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.linked.store(
            guard.as_ref().is_some_and(EnrollmentState::is_linked),
            Ordering::Release,
        );
        self.clear_outbound_block();
        Ok(())
    }

    /// Redeem an operator-pasted code and persist the resulting credential.
    ///
    /// Returns [`EnrollmentKind`], which is not decoration: this method is
    /// deliberately **not** first-time-only — overwriting `token`/`tenant_id` on
    /// an instance that is already linked is how credential recovery works after
    /// a `CredentialRejected` — so a caller with a side effect that belongs to
    /// *establishing* a link has no other way to tell the two apart. See
    /// [`EnrollmentKind`] for why guessing is not acceptable.
    pub async fn enroll(&self, code: &str) -> Result<EnrollmentKind, CloudError> {
        if let Some(error) = self.unreadable_cloud_error() {
            return Err(error);
        }
        if let Some(error) = self.outbound_blocked_error() {
            return Err(error);
        }
        let (base_url, instance_id, generation) = {
            let guard = self.state.read().unwrap_or_else(|p| p.into_inner());
            let s = guard.as_ref().ok_or(CloudError::NotEnrolled)?;
            (
                s.base_url.clone(),
                s.instance_id,
                self.generation.load(Ordering::SeqCst),
            )
        };

        let backend = self.parse_backend(&base_url)?;
        let res = CloudClient::new(backend)?
            .enroll(code, instance_id, &self.agent_version)
            .await?;

        let mut guard = self.state.write().unwrap_or_else(|p| p.into_inner());
        let current = guard
            .as_ref()
            .ok_or_else(|| CloudError::EnrollmentRefused {
                detail: "link state changed while enrollment was in progress; try again".into(),
            })?;
        if self.generation.load(Ordering::SeqCst) != generation
            || current.base_url != base_url
            || current.instance_id != instance_id
        {
            return Err(CloudError::EnrollmentRefused {
                detail: "link state changed while enrollment was in progress; try again".into(),
            });
        }
        // Read *before* the overwrite below: after it, there is no record on
        // this instance that a different credential was ever held, and a caller
        // asking "did this call establish the link?" would have to guess.
        let kind = EnrollmentKind::classify(current.is_linked(), current.tenant_id, res.tenant_id);

        let mut next = current.clone();
        next.token = Some(res.instance_token);
        next.tenant_id = Some(res.tenant_id);
        next.account_email = res.account_email;
        // Clone → save → swap: a failed disk write cannot leave a credential
        // alive only in memory.
        self.save_state(&next)
            .map_err(|e| CloudError::EnrollmentRefused {
                detail: format!("enrolled, but the credential could not be saved: {e}"),
            })?;
        *guard = Some(next);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.linked.store(true, Ordering::Release);
        self.credential_rejected.store(false, Ordering::SeqCst);
        *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
        Ok(kind)
    }

    /// Revoke the active credential at its issuing backend.
    ///
    /// This deliberately leaves local state untouched. The caller may only
    /// remove the local credential after this succeeds, or after the backend
    /// confirms that the credential is already invalid.
    pub async fn revoke(&self) -> Result<(), CloudError> {
        // ADR-041 §7c: before the credential goes away, hand every Cloud-primary
        // project back to local span storage and spill whatever is still queued
        // into the local store. This runs *first*, and unconditionally, because
        // it is the only point in the disconnect path that is both async and
        // still has a usable link — after this method the token is gone and the
        // outbox has nowhere to ship.
        //
        // Running it even when the revoke below then fails is deliberate: the
        // instance is left storing spans locally, which is always safe, and the
        // recovery path reopens `cloud` intervals automatically once Cloud is
        // accepting again (§7b). The opposite ordering would leave a window in
        // which the credential is gone and projects still believe they are
        // Cloud-primary.
        //
        // What is *not* acceptable is doing it quietly. When the revoke then
        // fails, the request the operator made returns an error while several of
        // their projects' write mode has already been permanently rewritten, so
        // the count is both recorded (for callers that can surface it) and
        // logged at ERROR here, naming what changed and how to put it back.
        let reverted = match self.telemetry_fallback() {
            Some(fallback) => {
                let reverted = fallback
                    .revert_to_local(CloudFallbackReason::Disconnected)
                    .await;
                self.telemetry_reverted_projects
                    .store(reverted, Ordering::Release);
                reverted
            }
            None => 0,
        };

        let outcome = self.revoke_credential().await;
        if let Err(error) = &outcome {
            if reverted > 0 {
                tracing::error!(
                    projects = reverted,
                    %error,
                    "Temps Cloud could not be told to revoke this instance's credential, but \
                     {reverted} project(s) had already been switched back to storing spans on \
                     this instance. Nothing is lost — their spans are on this machine — but that \
                     change was kept. If you did not mean to disconnect, set those projects back \
                     to Cloud-primary in their project settings once the link is healthy again."
                );
            }
        }
        outcome
    }

    /// The Cloud-side half of [`Self::revoke`], split out so the telemetry
    /// fallback's outcome is still in scope when this fails.
    async fn revoke_credential(&self) -> Result<(), CloudError> {
        if let Some(error) = self.unreadable_cloud_error() {
            return Err(error);
        }
        if let Some(error) = self.outbound_blocked_error() {
            return Err(error);
        }
        let (base_url, token) = {
            let guard = self.state.read().unwrap_or_else(|p| p.into_inner());
            let state = guard.as_ref().ok_or(CloudError::NotEnrolled)?;
            let token = state.token.clone().ok_or(CloudError::NotEnrolled)?;
            (state.base_url.clone(), token)
        };
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?.revoke(&token).await
    }

    pub async fn managed_ai_capability(&self) -> Result<ManagedAiCapability, CloudError> {
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .managed_ai_capability(&token)
            .await
    }

    /// Describe or vend a managed backup destination for this tenant. Does
    /// not require the `backups` feature switch: whether Cloud is *willing*
    /// to hand out a destination is a separate question from whether the
    /// operator has opted the local backup mirror into exporting to it.
    pub async fn managed_backup_credentials(&self) -> Result<ManagedBackupCapability, CloudError> {
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .managed_backup_credentials(&token)
            .await
    }

    pub async fn managed_ai_analysis(
        &self,
        request: &ManagedAiAnalysisRequest,
    ) -> Result<ManagedAiAnalysisResponse, CloudError> {
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .managed_ai_analysis(&token, request)
            .await
    }

    pub async fn managed_ai_chat(
        &self,
        request: &ManagedAiChatRequest,
    ) -> Result<ManagedAiChatResponse, CloudError> {
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .managed_ai_chat(&token, request)
            .await
    }

    /// Upload a completed local artifact without ever routing its bytes through
    /// the Cloud API process. The instance credential is read only for the two
    /// small lifecycle calls around the direct object-storage PUT.
    pub async fn upload_backup_file(
        &self,
        backup_id: Uuid,
        source: String,
        artifact: BackupArtifact,
        path: &Path,
    ) -> Result<BackupTarget, CloudError> {
        let (base_url, token, instance_id) = self.linked_backup_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .upload_backup_file(&token, instance_id, backup_id, source, artifact, path)
            .await
    }

    fn walg_client(&self) -> Result<(CloudClient, String, Uuid), CloudError> {
        let (base_url, token, instance_id) = self.linked_backup_credential()?;
        let backend = self.parse_backend(&base_url)?;
        Ok((CloudClient::new(backend)?, token, instance_id))
    }

    pub async fn declare_walg_snapshot(
        &self,
        request: &WalGSnapshotRequest,
    ) -> Result<WalGSnapshot, CloudError> {
        let (client, token, instance_id) = self.walg_client()?;
        if request.instance_id != instance_id {
            return Err(CloudError::Rejected {
                detail: "WAL-G snapshot instance_id does not match this Cloud link".into(),
            });
        }
        client.declare_walg_snapshot(&token, request).await
    }

    pub async fn declare_native_snapshot(
        &self,
        request: &NativeSnapshotRequest,
    ) -> Result<NativeSnapshot, CloudError> {
        let (client, token, instance_id) = self.walg_client()?;
        if request.instance_id != instance_id {
            return Err(CloudError::Rejected {
                detail: "native snapshot instance_id does not match this Cloud link".into(),
            });
        }
        client.declare_native_snapshot(&token, request).await
    }

    pub async fn native_object_target(
        &self,
        request: &WalGObjectTargetRequest,
    ) -> Result<WalGObjectTarget, CloudError> {
        let (client, token, instance_id) = self.walg_client()?;
        if request.instance_id != instance_id {
            return Err(CloudError::Rejected {
                detail: "native object instance_id does not match this Cloud link".into(),
            });
        }
        client.native_object_target(&token, request).await
    }

    pub async fn complete_native_object(
        &self,
        completion: &WalGObjectCompleted,
    ) -> Result<(), CloudError> {
        let (client, token, _) = self.walg_client()?;
        client.complete_native_object(&token, completion).await
    }

    pub async fn complete_native_snapshot(
        &self,
        completion: &WalGSnapshotCompleted,
    ) -> Result<(), CloudError> {
        let (client, token, _) = self.walg_client()?;
        client.complete_native_snapshot(&token, completion).await
    }

    pub async fn walg_object_target(
        &self,
        request: &WalGObjectTargetRequest,
    ) -> Result<WalGObjectTarget, CloudError> {
        let (client, token, instance_id) = self.walg_client()?;
        if request.instance_id != instance_id {
            return Err(CloudError::Rejected {
                detail: "WAL-G object instance_id does not match this Cloud link".into(),
            });
        }
        client.walg_object_target(&token, request).await
    }

    /// Stream a repository object to the exact short-lived destination Cloud
    /// issued. Destination validation and redirect prevention live in
    /// [`CloudClient`] and are therefore identical for WAL-G, native snapshots
    /// and local backup files.
    pub async fn upload_backup_object_reader<R>(
        &self,
        target: &WalGObjectTarget,
        reader: R,
        spooled_bytes: u64,
    ) -> Result<reqwest::Response, CloudError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let (client, _, _) = self.walg_client()?;
        client
            .upload_backup_object_reader(target, reader, spooled_bytes)
            .await
    }

    pub async fn complete_walg_object(
        &self,
        completion: &WalGObjectCompleted,
    ) -> Result<(), CloudError> {
        let (client, token, _) = self.walg_client()?;
        client.complete_walg_object(&token, completion).await
    }

    pub async fn complete_walg_snapshot(
        &self,
        completion: &WalGSnapshotCompleted,
    ) -> Result<(), CloudError> {
        let (client, token, _) = self.walg_client()?;
        client.complete_walg_snapshot(&token, completion).await
    }

    pub async fn send_notification(
        &self,
        request: &ManagedNotificationRequest,
    ) -> Result<ManagedNotificationAccepted, CloudError> {
        if !self.notifications_enabled.load(Ordering::Acquire) {
            return Err(CloudError::FeatureDisabled {
                feature: "notifications",
            });
        }
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        CloudClient::new(backend)?
            .send_notification(&token, request)
            .await
    }

    /// The one place the instance credential is read. `pub(crate)` so
    /// [`crate::query`] reuses it instead of introducing a second source for
    /// the same token.
    pub(crate) fn linked_credential(&self) -> Result<(String, String), CloudError> {
        if let Some(error) = self.unreadable_cloud_error() {
            return Err(error);
        }
        if let Some(error) = self.outbound_blocked_error() {
            return Err(error);
        }
        let guard = self.state.read().unwrap_or_else(|p| p.into_inner());
        let state = guard.as_ref().ok_or(CloudError::NotEnrolled)?;
        let token = state.token.clone().ok_or(CloudError::NotEnrolled)?;
        Ok((state.base_url.clone(), token))
    }

    fn linked_backup_credential(&self) -> Result<(String, String, Uuid), CloudError> {
        if !self.backups_enabled.load(Ordering::Acquire) {
            return Err(CloudError::FeatureDisabled { feature: "backups" });
        }
        if let Some(error) = self.unreadable_cloud_error() {
            return Err(error);
        }
        if let Some(error) = self.outbound_blocked_error() {
            return Err(error);
        }
        let guard = self.state.read().unwrap_or_else(|p| p.into_inner());
        let state = guard.as_ref().ok_or(CloudError::NotEnrolled)?;
        let token = state.token.clone().ok_or(CloudError::NotEnrolled)?;
        Ok((state.base_url.clone(), token, state.instance_id))
    }

    /// Forget the credential. Keeps the instance identity so re-linking later
    /// reattaches to the same record.
    pub fn disconnect(&self) -> Result<(), crate::state::StateError> {
        self.ensure_state_readable()?;
        self.linked.store(false, Ordering::Release);
        let mut guard = self.state.write().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = guard.as_mut() {
            let mut next = s.clone();
            next.unlink();
            self.save_state(&next)?;
            *s = next;
        }
        self.spool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(usize::MAX);
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.clear_scoped_submission();
        self.credential_rejected.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
        Ok(())
    }

    /// Open an isolated submission scope (ADR-042 §2).
    ///
    /// [`Self::record`] and [`Self::flush`] share one spool with the live
    /// mirror, which is correct for the mirror and wrong for anyone who needs to
    /// know whether *its own* batch drained. On a running instance the mirror
    /// records continuously, so a caller looping `flush` until
    /// [`Self::spooled`] reaches zero would count the mirror's spans as its own
    /// and, on a busy instance, might never see zero at all. The Cloud telemetry
    /// backfill is that caller.
    ///
    /// A scope gives it its own queue, its own pending submission and its own
    /// drain-complete test, while still sharing the link's credential, its
    /// `flush_lock` — so submission concurrency stays 1 globally, per
    /// ADR-041 §3b — and its revocation signal.
    ///
    /// At most one scope may be open at a time; a second call returns
    /// [`SubmissionScopeBusy`] rather than silently sharing one. Closing it is
    /// `drop`.
    pub fn submission_scope(&self) -> Result<SubmissionScope<'_>, SubmissionScopeBusy> {
        let mut guard = self
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Err(SubmissionScopeBusy);
        }
        *guard = Some(ScopedSubmission::new());
        Ok(SubmissionScope { link: self })
    }

    /// Forget whatever an open scope still holds, leaving the scope itself open.
    ///
    /// Called from the same places that clear the mirror spool. The handle stays
    /// valid on purpose: its next `flush` then reports `NotLinked`, so the
    /// caller learns the link went away instead of seeing an empty queue and
    /// concluding it shipped everything.
    fn clear_scoped_submission(&self) {
        if let Some(scope) = self
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            scope.clear();
        }
    }

    /// Offer spans to the mirror. Never blocks on IO, never fails.
    ///
    /// When the instance is not linked this is a no-op: buffering for a backend
    /// that does not exist would burn memory to no purpose. Telemetry is still
    /// stored locally by the instance itself — that path is untouched.
    pub fn record(&self, spans: Vec<SpanRecord>) {
        if spans.is_empty()
            || !self.telemetry_enabled.load(Ordering::Acquire)
            || !self.linked.load(Ordering::Acquire)
        {
            return;
        }
        let count = spans.len();
        self.incoming_spans.fetch_add(count, Ordering::Relaxed);
        let batch = IncomingBatch {
            generation: self.generation.load(Ordering::Acquire),
            spans,
        };
        if let Err(error) = self.incoming_tx.try_send(batch) {
            let dropped = error.into_inner().spans.len();
            self.incoming_spans.fetch_sub(dropped, Ordering::Relaxed);
            self.incoming_dropped
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
    }

    pub fn spooled(&self) -> usize {
        // Status reads are off the ingest path and may opportunistically move
        // accepted producer batches into the bounded spool for an exact count.
        self.drain_incoming();
        let queued = self.spool.lock().unwrap_or_else(|p| p.into_inner()).len();
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(0, |batch| batch.spans.len());
        self.incoming_spans.load(Ordering::Relaxed) + queued + pending
    }

    fn drain_incoming(&self) {
        let current_generation = self.generation.load(Ordering::Acquire);
        let mut receiver = self.incoming_rx.lock().unwrap_or_else(|p| p.into_inner());
        let mut spool = self.spool.lock().unwrap_or_else(|p| p.into_inner());
        while let Ok(batch) = receiver.try_recv() {
            self.incoming_spans
                .fetch_sub(batch.spans.len(), Ordering::Relaxed);
            if batch.generation == current_generation && self.linked.load(Ordering::Acquire) {
                spool.push(batch.spans);
            }
        }
    }

    /// Ship one already-durable batch straight to `POST /v1/telemetry`
    /// (ADR-041 §3).
    ///
    /// This is the Cloud-**primary** path and is deliberately *not*
    /// [`CloudLink::flush`]:
    ///
    /// - It never touches the in-memory [`Spool`] or the `pending_submission`
    ///   field in the encrypted state file. The durable record is the outbox
    ///   row, which already survives a restart; rewriting the whole state file
    ///   per batch on top of that would be a second, weaker copy and a
    ///   per-batch whole-file rewrite of a credential store.
    /// - It takes the batch as an argument rather than draining a queue, so the
    ///   caller owns claim/ack and the row is only settled after Cloud has
    ///   acknowledged it.
    ///
    /// Holds the same `flush_lock` as [`CloudLink::flush`] so the mirror path
    /// and the primary path never have two submissions in flight at once —
    /// ADR-041 §3b is explicit that sequential drain must be proven sufficient
    /// before any concurrency is added, because whether `/v1/telemetry`'s
    /// idempotency and metering tolerate concurrent submissions from one
    /// instance is an open question on the Cloud side.
    ///
    /// Subscribes to telemetry revocations before starting I/O, exactly as
    /// `flush` does, so switching export off drops the in-flight request rather
    /// than letting it complete after consent was withdrawn.
    pub async fn ship_outbox_batch(
        &self,
        submission_id: Uuid,
        spans: Vec<SpanRecord>,
    ) -> OutboxShipOutcome {
        if spans.is_empty() {
            return OutboxShipOutcome::Idle;
        }
        let _flush = self.flush_lock.lock().await;
        let mut telemetry_revocations = self.telemetry_revocations.subscribe();

        if !self.telemetry_enabled.load(Ordering::Acquire) {
            return OutboxShipOutcome::NotLinked;
        }
        let (base_url, token) = match self.linked_credential() {
            Ok(credential) => credential,
            Err(error) => {
                return match error {
                    CloudError::NotEnrolled => OutboxShipOutcome::NotLinked,
                    other => OutboxShipOutcome::Blocked {
                        spans: spans.len(),
                        reason: other.to_string(),
                    },
                }
            }
        };
        let generation = self.generation.load(Ordering::SeqCst);
        let count = spans.len();

        let backend = match self.parse_backend(&base_url) {
            Ok(backend) => backend,
            Err(error) => {
                return OutboxShipOutcome::Blocked {
                    spans: count,
                    reason: error.to_string(),
                }
            }
        };
        let client = match CloudClient::new(backend) {
            Ok(client) => client,
            Err(error) => {
                return OutboxShipOutcome::Blocked {
                    spans: count,
                    reason: error.to_string(),
                }
            }
        };

        let result = tokio::select! {
            biased;
            changed = telemetry_revocations.changed() => {
                let _ = changed;
                return OutboxShipOutcome::NotLinked;
            }
            result = client.ship(&token, submission_id, spans) => result,
        };

        if !self.telemetry_enabled.load(Ordering::Acquire)
            || self.generation.load(Ordering::SeqCst) != generation
        {
            // Consent was withdrawn, or the link changed origin, while the
            // request was in flight. The caller must keep the rows rather than
            // acking them against a link that no longer exists.
            return OutboxShipOutcome::NotLinked;
        }

        match result {
            Ok(ack) => {
                self.credential_rejected.store(false, Ordering::SeqCst);
                OutboxShipOutcome::Shipped {
                    spans: count,
                    warning: ack.warning,
                }
            }
            Err(error) => {
                if matches!(error, CloudError::CredentialRejected) {
                    self.credential_rejected.store(true, Ordering::SeqCst);
                }
                let reason = error.to_string();
                if error.is_retryable() {
                    OutboxShipOutcome::Retained {
                        spans: count,
                        reason,
                    }
                } else {
                    OutboxShipOutcome::Blocked {
                        spans: count,
                        reason,
                    }
                }
            }
        }
    }

    /// Ship one batch. Called on an interval by a background task.
    pub async fn flush(&self) -> FlushOutcome {
        let _flush = self.flush_lock.lock().await;
        let mut telemetry_revocations = self.telemetry_revocations.subscribe();
        self.drain_incoming();
        if !self.telemetry_enabled.load(Ordering::Acquire) {
            return FlushOutcome::NotLinked;
        }
        let (base_url, token, generation) = {
            let guard = self.state.read().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(s) if s.is_linked() => (
                    s.base_url.clone(),
                    s.token.clone().unwrap_or_default(),
                    self.generation.load(Ordering::SeqCst),
                ),
                _ => return FlushOutcome::NotLinked,
            }
        };

        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let pending = match pending {
            Some(pending) => Some(pending),
            None => {
                // Lock order is state -> spool -> pending everywhere. Keeping
                // that order prevents a concurrent disconnect from restoring
                // an origin-bound batch after it has cleared the link.
                let mut state_guard = self.state.write().unwrap_or_else(|p| p.into_inner());
                let Some(state) = state_guard.as_ref() else {
                    return FlushOutcome::NotLinked;
                };
                if !state.is_linked() || self.generation.load(Ordering::SeqCst) != generation {
                    return FlushOutcome::NotLinked;
                }
                let spans = self
                    .spool
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take(BATCH_SIZE);
                if spans.is_empty() {
                    None
                } else {
                    let next = PendingSubmission {
                        submission_id: Uuid::new_v4(),
                        spans,
                    };
                    let mut persisted = state.clone();
                    persisted.pending_submission = Some(next.clone());
                    if let Err(error) = self.save_state(&persisted) {
                        self.spool
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .requeue(next.spans);
                        return FlushOutcome::Blocked {
                            spans: 0,
                            reason: format!("persist telemetry retry before upload: {error}"),
                        };
                    }
                    *state_guard = Some(persisted);
                    *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(next.clone());
                    Some(next)
                }
            }
        };
        let Some(pending) = pending else {
            *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
            return FlushOutcome::Idle;
        };
        let count = pending.spans.len();
        let backend = match self.parse_backend(&base_url) {
            Ok(backend) => backend,
            Err(e) => {
                return FlushOutcome::Blocked {
                    spans: count,
                    reason: e.to_string(),
                }
            }
        };

        let client = match CloudClient::new(backend) {
            Ok(client) => client,
            Err(e) => {
                return FlushOutcome::Blocked {
                    spans: count,
                    reason: e.to_string(),
                }
            }
        };

        let result = tokio::select! {
            biased;
            changed = telemetry_revocations.changed() => {
                let _ = changed;
                return FlushOutcome::NotLinked;
            }
            result = client.ship(&token, pending.submission_id, pending.spans.clone()) => result,
        };
        if !self.telemetry_enabled.load(Ordering::Acquire)
            || self.generation.load(Ordering::SeqCst) != generation
        {
            return FlushOutcome::NotLinked;
        }

        match result {
            Ok(ack) => {
                if let Err(error) = self.persist_pending(None) {
                    *self.health.write().unwrap_or_else(|p| p.into_inner()) =
                        MirrorHealth::Buffering {
                            spooled: self.spooled(),
                            reason: format!(
                                "Cloud accepted the submission but its durable acknowledgement could not be saved: {error}"
                            ),
                        };
                    return FlushOutcome::Retained {
                        spans: count,
                        reason: format!("persist Cloud acknowledgement: {error}"),
                    };
                }
                let mut current = self.pending.lock().unwrap_or_else(|p| p.into_inner());
                if current
                    .as_ref()
                    .is_some_and(|value| value.submission_id == pending.submission_id)
                {
                    current.take();
                }
                self.credential_rejected.store(false, Ordering::SeqCst);
                *self.health.write().unwrap_or_else(|p| p.into_inner()) = match ack.warning {
                    Some(detail) => MirrorHealth::Degraded { detail },
                    None => MirrorHealth::Healthy,
                };
                FlushOutcome::Shipped { spans: count }
            }

            Err(e) if e.is_retryable() => {
                if matches!(e, CloudError::CredentialRejected) {
                    self.credential_rejected.store(true, Ordering::SeqCst);
                }
                let spool = self.spool.lock().unwrap_or_else(|p| p.into_inner());
                let spooled = spool.len() + count;
                let dropped = spool
                    .dropped()
                    .saturating_add(self.incoming_dropped.load(Ordering::Relaxed));

                *self.health.write().unwrap_or_else(|p| p.into_inner()) = if dropped > 0 {
                    MirrorHealth::Dropping {
                        spooled,
                        dropped,
                        reason: e.to_string(),
                    }
                } else {
                    MirrorHealth::Buffering {
                        spooled,
                        reason: e.to_string(),
                    }
                };
                FlushOutcome::Retained {
                    spans: count,
                    reason: e.to_string(),
                }
            }

            Err(e) => {
                // Never infer that a 4xx or version-skew response makes customer
                // telemetry disposable. Keep the bounded pending batch and make
                // the operator-visible state explicit.
                *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Buffering {
                    spooled: self.spooled(),
                    reason: e.to_string(),
                };
                FlushOutcome::Blocked {
                    spans: count,
                    reason: e.to_string(),
                }
            }
        }
    }
}

/// One caller's private view of the link's submission path (ADR-042 §2).
///
/// Obtained from [`CloudLink::submission_scope`] and released by dropping it.
/// Everything it reports — [`Self::spooled`], [`Self::dropped`], the spans in
/// each [`FlushOutcome`] — is about spans offered through *this* handle. The
/// live mirror can be recording and flushing throughout and none of it appears
/// here, which is what lets a caller loop `flush` until its own queue is empty
/// and treat "still queued" as a real failure.
pub struct SubmissionScope<'link> {
    link: &'link CloudLink,
}

impl SubmissionScope<'_> {
    /// Offer spans to this scope. Never blocks on IO, never fails.
    ///
    /// Unlike [`CloudLink::record`] there is no bounded producer channel in
    /// front of the queue. A scoped caller is a control-plane loop that offers
    /// one bounded batch and waits for it, not a telemetry producer on the
    /// ingest path, so the channel's load-shedding would buy nothing and would
    /// make the caller's own accounting lossy.
    pub fn record(&self, spans: Vec<SpanRecord>) {
        if spans.is_empty() {
            return;
        }
        // A cleared scope — revocation, disconnect, origin change — drops what
        // is offered. The next `flush` reports `NotLinked`, which is what the
        // caller has to act on; buffering for a link that is gone would only
        // delay it.
        if let Some(scope) = self
            .link
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            scope.spool.push(spans);
        }
    }

    /// Spans this scope has offered that Cloud has not acknowledged yet.
    ///
    /// Counts only this scope's spans. Live mirror traffic recorded through
    /// [`CloudLink::record`] is invisible here, which is the entire point:
    /// `spooled() == 0` means *this caller's* batch drained.
    pub fn spooled(&self) -> usize {
        self.link
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(0, ScopedSubmission::queued)
    }

    /// Spans this scope discarded because its queue was full.
    ///
    /// A scoped caller ships history the operator is paying for, so it must be
    /// able to tell "everything drained" from "everything that survived
    /// drained". Zero in normal operation, since a caller offers one batch at a
    /// time and drains it.
    pub fn dropped(&self) -> u64 {
        self.link
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(0, |scope| scope.spool.dropped())
    }

    /// Ship one of this scope's batches.
    ///
    /// Takes the same `flush_lock` as [`CloudLink::flush`] and
    /// [`CloudLink::ship_outbox_batch`], so the mirror, the Cloud-primary outbox
    /// and a scoped caller never have two submissions in flight at once
    /// (ADR-041 §3b). Subscribes to telemetry revocations before starting I/O,
    /// exactly as those two do, so switching export off drops the in-flight
    /// request rather than letting it complete after consent was withdrawn.
    ///
    /// Deliberately does **not** touch [`MirrorHealth`] or the state file's
    /// `pending_submission`. The first is the mirror's operator-visible status
    /// and a backfill must not rewrite it — an operator reading "buffering,
    /// 4000 spooled" needs that to be about their live telemetry. The second is
    /// a per-batch whole-file rewrite of a credential store, which a scoped
    /// caller carrying its own durable cursor does not need, for the same
    /// reason [`CloudLink::ship_outbox_batch`] avoids it.
    pub async fn flush(&self) -> FlushOutcome {
        let _flush = self.link.flush_lock.lock().await;
        let mut telemetry_revocations = self.link.telemetry_revocations.subscribe();
        if !self.link.telemetry_enabled.load(Ordering::Acquire) {
            return FlushOutcome::NotLinked;
        }
        let (base_url, token, generation) = {
            let guard = self.link.state.read().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(state) if state.is_linked() => (
                    state.base_url.clone(),
                    state.token.clone().unwrap_or_default(),
                    self.link.generation.load(Ordering::SeqCst),
                ),
                _ => return FlushOutcome::NotLinked,
            }
        };

        let submission = {
            let mut guard = self
                .link
                .scoped_submission
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let Some(scope) = guard.as_mut() else {
                // The scope was cleared under us by a revocation or a
                // disconnect. Reporting `Idle` here would tell the caller its
                // batch shipped.
                return FlushOutcome::NotLinked;
            };
            match scope.pending.clone() {
                Some(pending) => pending,
                None => {
                    let spans = scope.spool.take(BATCH_SIZE);
                    if spans.is_empty() {
                        return FlushOutcome::Idle;
                    }
                    let next = PendingSubmission {
                        submission_id: Uuid::new_v4(),
                        spans,
                    };
                    scope.pending = Some(next.clone());
                    next
                }
            }
        };
        let count = submission.spans.len();

        let backend = match self.link.parse_backend(&base_url) {
            Ok(backend) => backend,
            Err(error) => {
                return FlushOutcome::Blocked {
                    spans: count,
                    reason: error.to_string(),
                }
            }
        };
        let client = match CloudClient::new(backend) {
            Ok(client) => client,
            Err(error) => {
                return FlushOutcome::Blocked {
                    spans: count,
                    reason: error.to_string(),
                }
            }
        };

        let result = tokio::select! {
            biased;
            changed = telemetry_revocations.changed() => {
                let _ = changed;
                return FlushOutcome::NotLinked;
            }
            result = client.ship(&token, submission.submission_id, submission.spans.clone()) => result,
        };

        if !self.link.telemetry_enabled.load(Ordering::Acquire)
            || self.link.generation.load(Ordering::SeqCst) != generation
        {
            // Consent was withdrawn, or the link changed origin, while the
            // request was in flight. The batch must not be counted as shipped
            // against a link that no longer exists.
            return FlushOutcome::NotLinked;
        }

        match result {
            Ok(ack) => {
                self.link.credential_rejected.store(false, Ordering::SeqCst);
                if let Some(scope) = self
                    .link
                    .scoped_submission
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_mut()
                {
                    if scope
                        .pending
                        .as_ref()
                        .is_some_and(|value| value.submission_id == submission.submission_id)
                    {
                        scope.pending = None;
                    }
                }
                if let Some(warning) = ack.warning {
                    // The mirror path records this as `MirrorHealth::Degraded`;
                    // a scope must not overwrite that surface, but the operator
                    // still has to be able to find out that Cloud accepted this
                    // submission while degraded.
                    tracing::warn!(
                        spans = count,
                        warning = ?warning,
                        "Temps Cloud accepted a scoped submission but the tenant is degraded"
                    );
                }
                FlushOutcome::Shipped { spans: count }
            }
            Err(error) => {
                if matches!(error, CloudError::CredentialRejected) {
                    self.link.credential_rejected.store(true, Ordering::SeqCst);
                }
                let reason = error.to_string();
                // The batch stays in the scope's pending slot with its
                // submission id, so a retry is idempotent. Never infer from a
                // refusal that the caller's spans are disposable.
                if error.is_retryable() {
                    FlushOutcome::Retained {
                        spans: count,
                        reason,
                    }
                } else {
                    FlushOutcome::Blocked {
                        spans: count,
                        reason,
                    }
                }
            }
        }
    }
}

/// Names the type without reading its queue. Formatting deliberately takes no
/// lock: a `Debug` impl that did could deadlock whoever is holding one, and the
/// interesting numbers are already available through [`SubmissionScope::spooled`]
/// and [`SubmissionScope::dropped`].
impl std::fmt::Debug for SubmissionScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmissionScope").finish_non_exhaustive()
    }
}

impl Drop for SubmissionScope<'_> {
    /// Release the claim. Anything still queued is discarded: a scoped caller
    /// resumes from its own durable cursor, so holding a half-shipped batch
    /// after the caller has gone would be a leak, not a recovery.
    fn drop(&mut self) {
        *self
            .link
            .scoped_submission
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(test)]
mod enrollment_kind_tests {
    use super::*;

    #[test]
    fn an_instance_with_no_credential_is_a_first_enrollment() {
        let tenant = Uuid::new_v4();
        let kind = EnrollmentKind::classify(false, None, tenant);

        assert_eq!(kind, EnrollmentKind::First);
        assert!(kind.establishes_new_link());
        assert_eq!(kind.as_str(), "first");
    }

    #[test]
    fn re_authenticating_the_same_tenant_does_not_establish_a_new_link() {
        // The credential-recovery path: a token was revoked or rotated at the
        // backend and the operator pasted a fresh code for the *same* account.
        // Nothing about the link is new, so nothing that belongs to linking —
        // least of all a spend — may fire again.
        let tenant = Uuid::new_v4();
        let kind = EnrollmentKind::classify(true, Some(tenant), tenant);

        assert_eq!(kind, EnrollmentKind::ReEnrolled { tenant_id: tenant });
        assert!(!kind.establishes_new_link());
        assert_eq!(kind.as_str(), "re_enrolled");
    }

    #[test]
    fn binding_to_a_different_tenant_is_a_new_link_even_with_a_credential_on_disk() {
        // Functionally a new customer. The stale credential on disk says
        // nothing about them, so treating this as "already linked" would deny
        // a genuinely new link the side effects it is entitled to.
        let previous = Uuid::new_v4();
        let next = Uuid::new_v4();
        let kind = EnrollmentKind::classify(true, Some(previous), next);

        assert_eq!(
            kind,
            EnrollmentKind::ReboundToNewTenant {
                previous_tenant_id: Some(previous)
            }
        );
        assert!(kind.establishes_new_link());
        assert_eq!(kind.as_str(), "rebound_to_new_tenant");
    }

    #[test]
    fn a_token_with_no_recorded_tenant_cannot_be_shown_to_be_the_same_customer() {
        // A half-written legacy state can carry a token and no tenant. There is
        // no evidence it is the same tenant, and the failure directions are not
        // symmetric: treating it as new costs one extra activation the operator
        // can cancel, treating it as the same silently withholds one from a
        // customer who just paid.
        let tenant = Uuid::new_v4();
        let kind = EnrollmentKind::classify(true, None, tenant);

        assert_eq!(
            kind,
            EnrollmentKind::ReboundToNewTenant {
                previous_tenant_id: None
            }
        );
        assert!(kind.establishes_new_link());
    }
}

#[cfg(test)]
mod unreadable_state_tests {
    use super::*;
    use crate::state::StateError;

    #[test]
    fn explicit_loopback_policy_survives_encrypted_restart() {
        let directory = tempfile::tempdir().unwrap();
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "restart-loopback-policy-key",
        ));
        let first = CloudLink::load_encrypted_for_loopback_development(
            directory.path().to_path_buf(),
            "test-agent",
            encryption.clone(),
        );
        first
            .configure(BackendUrl::loopback_development("http://127.0.0.1:19202").unwrap())
            .unwrap();
        drop(first);

        let restarted =
            CloudLink::load_encrypted(directory.path().to_path_buf(), "test-agent", encryption);
        assert!(restarted.allows_loopback_development());
        assert!(restarted.parse_backend("http://127.0.0.1:19202").is_ok());
        assert!(matches!(
            restarted.status(),
            LinkStatus::AwaitingEnrollment { base_url }
                if base_url == "http://127.0.0.1:19202/"
        ));
    }

    #[tokio::test]
    async fn corrupt_state_is_visible_and_cannot_be_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("cloud-link/state.json");
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let corrupt = b"{recoverable-but-currently-corrupt";
        std::fs::write(&state_path, corrupt).unwrap();

        let link =
            CloudLink::load_for_loopback_development(directory.path().to_path_buf(), "test-agent");
        assert!(matches!(link.status(), LinkStatus::StateUnreadable { .. }));

        let backend = BackendUrl::loopback_development("http://127.0.0.1:19200").unwrap();
        assert!(matches!(
            link.configure(backend),
            Err(StateError::UnreadableStateBlocksMutation { .. })
        ));
        assert!(matches!(
            link.disconnect(),
            Err(StateError::UnreadableStateBlocksMutation { .. })
        ));
        assert!(matches!(
            link.enroll("code").await,
            Err(CloudError::LinkStateUnreadable { .. })
        ));
        assert_eq!(std::fs::read(&state_path).unwrap(), corrupt);
    }

    #[test]
    fn encryption_key_mismatch_blocks_mutation_and_preserves_ciphertext() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("cloud-link/state.json");
        let original_encryption =
            temps_core::EncryptionService::new_from_password("original-cloud-link-key");
        EnrollmentState::new("http://127.0.0.1:19200")
            .save_encrypted(&state_path, &original_encryption)
            .unwrap();
        let ciphertext = std::fs::read(&state_path).unwrap();

        let wrong_encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "different-cloud-link-key",
        ));
        let link = CloudLink::load_encrypted_for_loopback_development(
            directory.path().to_path_buf(),
            "test-agent",
            wrong_encryption,
        );

        assert!(matches!(link.status(), LinkStatus::StateUnreadable { .. }));
        let backend = BackendUrl::loopback_development("http://127.0.0.1:19201").unwrap();
        assert!(matches!(
            link.configure(backend),
            Err(StateError::UnreadableStateBlocksMutation { .. })
        ));
        assert_eq!(std::fs::read(&state_path).unwrap(), ciphertext);
    }
}

#[cfg(test)]
mod submission_scope_tests {
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use axum::{extract::State, routing::post, Json, Router};
    use temps_cloud_protocol::TelemetryBatch;

    use super::*;

    /// Counts accepted spans per producer, keyed by the prefix each test gives
    /// its span names. The whole question a scope answers is "whose spans were
    /// those", so the stub has to be able to answer it too.
    #[derive(Clone, Default)]
    struct Stub {
        mirror_spans: Arc<AtomicUsize>,
        scoped_spans: Arc<AtomicUsize>,
    }

    async fn serve(stub: Stub) -> Option<String> {
        let app = Router::new()
            .route(
                "/v1/enroll",
                post(|| async {
                    Json(serde_json::json!({
                        "tenant_id": Uuid::new_v4(),
                        "instance_token": "inst_submission_scope_test"
                    }))
                }),
            )
            .route(
                "/v1/telemetry",
                post(
                    |State(stub): State<Stub>, Json(batch): Json<TelemetryBatch>| async move {
                        let spans = batch.spans.len();
                        for span in &batch.spans {
                            if span.name.starts_with("scoped") {
                                stub.scoped_spans.fetch_add(1, Ordering::SeqCst);
                            } else {
                                stub.mirror_spans.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Json(serde_json::json!({
                            "submission_id": batch.submission_id,
                            "processed_spans": spans,
                            "stored_spans": spans,
                            "metered_bytes": 1
                        }))
                    },
                ),
            )
            .with_state(stub);
        let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address must parse"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping submission scope network test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("test server must bind: {error}"),
        };
        let address = listener.local_addr().expect("test server has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(format!("http://{address}"))
    }

    fn span(name: String) -> SpanRecord {
        SpanRecord {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: format!("span-{name}"),
            name,
            ts_millis: 1,
            duration_ms: 1.0,
            attributes: Default::default(),
            ..Default::default()
        }
    }

    fn spans(prefix: &str, count: usize) -> Vec<SpanRecord> {
        (0..count).map(|i| span(format!("{prefix}-{i}"))).collect()
    }

    async fn linked_test_link(stub: Stub) -> Option<(Arc<CloudLink>, tempfile::TempDir)> {
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let backend = serve(stub).await?;
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "submission-scope-test",
        ));
        link.configure(
            crate::BackendUrl::loopback_development(&backend)
                .expect("stub backend URL must be accepted"),
        )
        .expect("test link must be configured");
        link.enroll("scope-code")
            .await
            .expect("test link must enroll");
        link.set_feature_switches(crate::CloudFeatureSwitches {
            telemetry: true,
            ..Default::default()
        })
        .expect("enable telemetry export");
        Some((link, directory))
    }

    /// Poll until `condition` holds, so the test never depends on a sleep being
    /// long enough on a loaded machine.
    async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..2_000 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_scope_drains_its_own_batch_while_the_mirror_is_recording() {
        // ADR-042 P0. This is the whole reason a scope exists: the shared spool
        // is being written to and drained by the live mirror throughout, and the
        // scoped caller must still be able to say "my batch shipped".
        let stub = Stub::default();
        let Some((link, _directory)) = linked_test_link(stub.clone()).await else {
            return;
        };

        let mirror_running = Arc::new(AtomicBool::new(true));
        let mirror = tokio::spawn({
            let link = Arc::clone(&link);
            let mirror_running = Arc::clone(&mirror_running);
            async move {
                while mirror_running.load(Ordering::SeqCst) {
                    link.record(spans("mirror", 5));
                    let _ = link.flush().await;
                    tokio::task::yield_now().await;
                }
            }
        });

        // Do not start measuring until the mirror is demonstrably shipping, or
        // a green test would prove nothing about concurrency.
        assert!(
            wait_until(|| stub.mirror_spans.load(Ordering::SeqCst) > 0).await,
            "the mirror must be shipping before the scoped submission starts"
        );
        let mirror_baseline = stub.mirror_spans.load(Ordering::SeqCst);

        let scope = link
            .submission_scope()
            .expect("a fresh link has no open scope");
        // More than one `BATCH_SIZE`, so the drain loop runs several times with
        // the mirror interleaving between iterations.
        let offered = BATCH_SIZE + 137;
        scope.record(spans("scoped", offered));

        let mut shipped = 0usize;
        for _ in 0..8 {
            match scope.flush().await {
                FlushOutcome::Shipped { spans } => shipped += spans,
                FlushOutcome::Idle => break,
                other => panic!("scoped flush must not fail: {other:?}"),
            }
            if scope.spooled() == 0 {
                break;
            }
        }

        assert_eq!(
            scope.spooled(),
            0,
            "the scope's drain-complete test must settle even while the mirror records"
        );
        assert_eq!(
            shipped, offered,
            "the scope must count exactly the spans it offered, not the mirror's"
        );
        assert_eq!(scope.dropped(), 0, "nothing may be shed at this size");
        assert_eq!(
            stub.scoped_spans.load(Ordering::SeqCst),
            offered,
            "Cloud must have received exactly the scoped batch"
        );
        assert!(
            stub.mirror_spans.load(Ordering::SeqCst) > mirror_baseline,
            "the mirror must have kept shipping during the scoped submission"
        );

        mirror_running.store(false, Ordering::SeqCst);
        let _ = tokio::time::timeout(Duration::from_secs(5), mirror).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mirror_traffic_is_invisible_to_a_scope_and_the_scope_to_the_mirror() {
        // The two counters must not see each other in either direction: a
        // backfill that counted mirror spans would over-report progress, and a
        // mirror whose `spooled()` included a backfill would tell the operator
        // their live telemetry is backlogged when it is not.
        let stub = Stub::default();
        let Some((link, _directory)) = linked_test_link(stub.clone()).await else {
            return;
        };

        let scope = link
            .submission_scope()
            .expect("a fresh link has no open scope");
        scope.record(spans("scoped", 40));
        link.record(spans("mirror", 25));

        assert_eq!(scope.spooled(), 40, "the scope sees only its own spans");
        assert_eq!(link.spooled(), 25, "the mirror sees only its own spans");

        assert_eq!(scope.flush().await, FlushOutcome::Shipped { spans: 40 });
        assert_eq!(
            scope.spooled(),
            0,
            "the scope drained without touching the mirror's queue"
        );
        assert_eq!(
            link.spooled(),
            25,
            "a scoped flush must never ship the mirror's spans"
        );
        assert_eq!(stub.scoped_spans.load(Ordering::SeqCst), 40);
        assert_eq!(stub.mirror_spans.load(Ordering::SeqCst), 0);

        assert_eq!(link.flush().await, FlushOutcome::Shipped { spans: 25 });
        assert_eq!(stub.mirror_spans.load(Ordering::SeqCst), 25);
    }

    #[tokio::test]
    async fn only_one_scope_may_be_open_at_a_time() {
        // Two scoped callers would reintroduce exactly the conflation a scope
        // removes, so the second one is refused rather than quietly sharing.
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let link = CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "submission-scope-test",
        );

        let first = link.submission_scope().expect("the first scope opens");
        let busy = link
            .submission_scope()
            .expect_err("a second concurrent scope must be refused");
        assert!(
            busy.to_string().contains("already open"),
            "the refusal must say what is in the way: {busy}"
        );

        drop(first);
        assert!(
            link.submission_scope().is_ok(),
            "dropping a scope must release the claim"
        );
    }

    #[tokio::test]
    async fn revoking_telemetry_export_empties_an_open_scope() {
        // Consent withdrawal must not leave exportable customer data resident in
        // a scope any more than in the mirror spool, and the caller must learn
        // why rather than seeing an empty queue and assuming success.
        let stub = Stub::default();
        let Some((link, _directory)) = linked_test_link(stub.clone()).await else {
            return;
        };
        let scope = link
            .submission_scope()
            .expect("a fresh link has no open scope");
        scope.record(spans("scoped", 12));
        assert_eq!(scope.spooled(), 12);

        link.set_feature_switches(crate::CloudFeatureSwitches::default())
            .expect("disable telemetry export");

        assert_eq!(scope.spooled(), 0, "revocation must drop the queued spans");
        assert_eq!(
            scope.flush().await,
            FlushOutcome::NotLinked,
            "the caller must be told the link went away, not that it drained"
        );
        assert_eq!(stub.scoped_spans.load(Ordering::SeqCst), 0);
    }
}
