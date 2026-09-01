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

    pub fn block_outbound(&self, reason: impl Into<String>) {
        *self
            .outbound_blocked_reason
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason.into());
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
    pub async fn enroll(&self, code: &str) -> Result<(), CloudError> {
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
        Ok(())
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
        self.credential_rejected.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.health.write().unwrap_or_else(|p| p.into_inner()) = MirrorHealth::Healthy;
        Ok(())
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
