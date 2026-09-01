// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core OTel service orchestrating ingest and storage.
//!
//! Sampling is the responsibility of the client SDK (head-based sampling).
//! The server stores all spans it receives.

use std::future::Future;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use temps_core::retry::RetryConfig;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{error, warn};

use crate::error::OtelError;
use crate::ingest::auth::{IngestAuth, OtelAuthService, ProjectAuth};
use crate::ingest::quota_cache::{QuotaCache, QUOTA_CACHE_TTL};
use crate::ingest::rate_limit::RateLimiter;
use crate::services::cloud_fidelity::{CloudPolicyCache, CloudTelemetryPolicy};
use crate::storage::OtelStorage;
use crate::types::*;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;

/// Core OTel service that orchestrates ingest and storage.
pub struct OtelService {
    storage: Arc<dyn OtelStorage>,
    auth_service: Arc<OtelAuthService>,
    rate_limiter: Arc<RateLimiter>,
    /// Rate limiter for service-scoped (`si_`) OTLP ingest, keyed by
    /// `service_id`. Kept separate from the project limiter so the two token
    /// classes don't share counters. Sized generously (see [`SERVICE_INGEST_*`]
    /// constants): `si_` traffic is machine-to-machine and periodic (a RustFS
    /// container pushes every ~10–30s), so the ceiling exists only to cap a
    /// runaway/compromised exporter, not to shape normal traffic.
    service_rate_limiter: Arc<RateLimiter>,
    /// TTL cache of per-project storage quota, avoiding a `COUNT(*)` scan
    /// over the OTel hypertables on every ingest request. See
    /// [`crate::ingest::quota_cache`].
    quota_cache: Arc<QuotaCache>,
    ingest_semaphore: Arc<Semaphore>,
    /// The configured value backing `ingest_semaphore`'s capacity, kept
    /// alongside it so `IngestSaturated` errors report the limit that is
    /// actually in effect (which may differ from
    /// [`DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS`] when overridden via
    /// `TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS`).
    ingest_permit_limit: usize,
    cloud_link: Option<Arc<temps_cloud_client::CloudLink>>,
    /// Resolves each project's Temps Cloud egress consent (ADR-040 §1).
    ///
    /// `None` means the fidelity columns are not reachable from here, and every
    /// mirrored span uses [`CloudTelemetryPolicy::metered`] — i.e. exactly the
    /// pre-ADR-040 projection. Absent must always mean *less* egress, never
    /// more, which is why this is an `Option` with a `Metered` fallback rather
    /// than a required dependency that would fail startup.
    cloud_policy_cache: Option<Arc<CloudPolicyCache>>,
    /// ADR-041 §3: the durable queue Cloud-primary spans are written to instead
    /// of local storage.
    ///
    /// `None` means no project can be Cloud-primary on this instance, however
    /// its rows are configured — the partition in [`Self::ingest_spans`]
    /// requires one, so an unwired outbox degrades to today's local-first
    /// behaviour rather than to dropping spans.
    span_outbox: Option<Arc<temps_cloud_client::SpanOutbox>>,
    /// ADR-041 §7b: consulted per batch so a quota event resumes local writes
    /// immediately rather than up to one policy TTL later.
    write_mode_service: Option<Arc<crate::services::TelemetryWriteModeService>>,
    stats: PipelineStatsAtomic,
}

/// Default process-wide limit for OTLP requests that may authenticate,
/// decompress, decode, and write concurrently. The permit is acquired before
/// Axum buffers the request body, bounding both task and payload memory
/// during exporter retry storms.
///
/// This is a process-wide operational tuning knob, not per-tenant config, so
/// deployments that need a higher ceiling (larger hardware, many
/// projects/services sharing one instance) can override it via
/// `TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS` — see `OtelConfig::from_env`.
pub const DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS: usize = 64;

/// Max `si_` ingest requests per service per window. ~10 req/s — far above a
/// healthy exporter's cadence, low enough to stop a tight-loop flood from
/// filling TimescaleDB chunks / saturating the write channel.
const SERVICE_INGEST_MAX_REQUESTS: u32 = 600;
/// Sliding window for the service ingest limiter.
const SERVICE_INGEST_WINDOW: Duration = Duration::from_secs(60);

/// Project one locally-stored span into the record this instance is willing to
/// mirror to Temps Cloud, at the fidelity the owning project consented to
/// (ADR-040 §1).
///
/// Returns `None` when the record cannot be built — a link that is not
/// enrolled, or telemetry export switched off — in which case the span is
/// simply not mirrored. Local storage is authoritative and unaffected either
/// way.
pub(crate) fn cloud_span(
    link: &temps_cloud_client::CloudLink,
    span: &SpanRecord,
    policy: &CloudTelemetryPolicy,
) -> Option<temps_cloud_protocol::SpanRecord> {
    match policy.fidelity {
        // ── Metered: the default, and byte-identical to the pre-ADR-040
        //    projection. Every field added for `Queryable` is left at its
        //    empty value and is `skip_serializing_if`-elided, so the bytes on
        //    the wire are exactly what older instances shipped and an older
        //    gateway sees no new keys. `the_metered_projection_serializes_to_
        //    exactly_the_pre_adr_040_bytes` pins this against a literal.
        CloudTelemetryFidelity::Metered => Some(temps_cloud_protocol::SpanRecord {
            trace_id: link
                .pseudonymize_telemetry_id("trace", &span.trace_id)
                .ok()?,
            span_id: link.pseudonymize_telemetry_id("span", &span.span_id).ok()?,
            // Span names are application-controlled too. Instrumentation may put
            // raw URLs, SQL, email addresses or identifiers here, so the safe
            // continuous projection uses a neutral operation label.
            name: "span".to_string(),
            ts_millis: span.start_time.timestamp_millis(),
            duration_ms: span.duration_ms,
            // Default-deny at the OSS boundary. Arbitrary span attributes
            // routinely contain headers, SQL, user identifiers and other
            // application data.
            attributes: Default::default(),
            ..Default::default()
        }),

        // ── Queryable: the project opted in, so ship what makes a span
        //    renderable and correlatable. Trace and span identifiers go in the
        //    clear deliberately (ADR-040 §1): a pseudonymised id would render
        //    a tree but match nothing in the user's own logs, which is a defect
        //    rather than a privacy win. The project itself is still referenced
        //    only by a pseudonym.
        CloudTelemetryFidelity::Queryable => Some(temps_cloud_protocol::SpanRecord {
            trace_id: span.trace_id.clone(),
            span_id: span.span_id.clone(),
            name: span.name.clone(),
            ts_millis: span.start_time.timestamp_millis(),
            duration_ms: span.duration_ms,
            // Default-deny survives the opt-in: only exact-match allowlisted
            // keys are copied, and everything else is dropped here, before the
            // batch exists. An empty allowlist ships nothing.
            attributes: span
                .attributes
                .iter()
                .filter(|(key, _)| policy.allows_attribute(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            // Pseudonymous scoping key: Cloud can group by project without
            // learning its name, and nobody observing the payload can tie it to
            // a local project. It is not a secret *from Cloud* — see
            // `SpanRecord::project_ref`, which documents why the key holder can
            // invert a small-integer input.
            project_ref: link
                .pseudonymize_telemetry_id("project", &span.project_id.to_string())
                .ok()?,
            service_name: Some(span.resource.service_name.clone()),
            span_kind: Some(span.kind.to_string()),
            status_code: Some(span.status_code.to_string()),
            parent_span_id: span.parent_span_id.clone(),
            environment: span.resource.deployment_environment.clone(),
        }),
    }
}

/// Total attempts (first try + retries) for a single ingest storage write.
const STORAGE_WRITE_MAX_ATTEMPTS: u32 = 3;
/// Backoff base for ingest storage retries. Doubles per attempt, so the
/// worst case with [`STORAGE_WRITE_MAX_ATTEMPTS`] is 75 ms + 150 ms = 225 ms.
const STORAGE_WRITE_BASE_DELAY: Duration = Duration::from_millis(75);
/// Hard ceiling on any single ingest retry backoff.
const STORAGE_WRITE_MAX_DELAY: Duration = Duration::from_millis(300);

/// Backoff policy for the ingest storage write.
///
/// Deliberately an order of magnitude shorter than the git/DNS API retry
/// defaults elsewhere in the codebase (3 attempts, 1 s–10 s). The write runs
/// while the request still holds an ingest permit (see
/// [`OtelService::try_acquire_ingest_permit`]), so every millisecond spent
/// sleeping here is a millisecond that permit is unavailable to *other*
/// tenants sharing this process. A long backoff during a real ClickHouse
/// outage would convert a storage problem into a fleet-wide
/// [`OtelError::IngestSaturated`] (HTTP 503) problem. 225 ms worst case is
/// enough to ride out a socket blip or a pool hiccup and nothing more.
fn storage_retry_config() -> RetryConfig {
    RetryConfig::new(STORAGE_WRITE_MAX_ATTEMPTS)
        .with_base_delay(STORAGE_WRITE_BASE_DELAY)
        .with_max_delay(STORAGE_WRITE_MAX_DELAY)
}

/// Run an ingest storage write with a bounded retry that exits early on
/// terminal errors.
///
/// Hand-rolled rather than delegating to [`RetryConfig::retry`] because that
/// helper has no early-exit hook: it retries every error up to `max_attempts`,
/// which for a malformed batch or a schema mismatch means sleeping twice and
/// holding an ingest permit for 225 ms to reproduce the same failure three
/// times. `compute_delay` is public precisely so callers that need this
/// stop-on-terminal-error shape can reuse the backoff math — see its doc
/// comment in `temps-core`.
///
/// `operation` is invoked afresh per attempt (the caller clones the batch),
/// and `label` only appears in the retry warning so an operator can tell which
/// signal is flapping.
async fn store_with_retry<F, Fut>(label: &str, mut operation: F) -> Result<u64, OtelError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<u64, OtelError>>,
{
    let config = storage_retry_config();

    for attempt in 0..config.max_attempts {
        match operation().await {
            Ok(stored) => return Ok(stored),
            Err(e) => {
                let is_last_attempt = attempt + 1 >= config.max_attempts;
                // Terminal errors (malformed rows, schema drift, validation)
                // fail identically on every attempt — returning now keeps the
                // ingest permit held for the shortest possible time.
                if is_last_attempt || !e.is_transient() {
                    return Err(e);
                }
                let delay = config.compute_delay(attempt);
                warn!(
                    signal = label,
                    attempt = attempt + 1,
                    max_attempts = config.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "Transient OTel storage write failure, retrying"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }

    // Unreachable: `max_attempts` is a non-zero constant and the loop body
    // either returns a value or sleeps, so the final iteration always returns.
    // Kept as an explicit error rather than `unreachable!()` so a future
    // misconfiguration degrades instead of panicking on the ingest path.
    Err(OtelError::Internal {
        message: format!(
            "OTel storage retry loop for '{label}' exited without a result \
             (max_attempts = {})",
            config.max_attempts
        ),
    })
}

/// Atomic counters for pipeline observability.
struct PipelineStatsAtomic {
    metrics_received: AtomicU64,
    metrics_stored: AtomicU64,
    metrics_dropped: AtomicU64,
    spans_received: AtomicU64,
    spans_stored: AtomicU64,
    spans_dropped: AtomicU64,
    logs_received: AtomicU64,
    logs_stored_db: AtomicU64,
    logs_stored_s3: AtomicU64,
    logs_dropped: AtomicU64,
    ingest_errors: AtomicU64,
    /// Ingest requests rejected by the per-project rate limiter (→ HTTP 429).
    rate_limited_requests: AtomicU64,
    /// Ingest requests rejected because the per-project storage quota was
    /// exhausted (→ HTTP 413).
    quota_exceeded_requests: AtomicU64,
}

impl Default for PipelineStatsAtomic {
    fn default() -> Self {
        Self {
            metrics_received: AtomicU64::new(0),
            metrics_stored: AtomicU64::new(0),
            metrics_dropped: AtomicU64::new(0),
            spans_received: AtomicU64::new(0),
            spans_stored: AtomicU64::new(0),
            spans_dropped: AtomicU64::new(0),
            logs_received: AtomicU64::new(0),
            logs_stored_db: AtomicU64::new(0),
            logs_stored_s3: AtomicU64::new(0),
            logs_dropped: AtomicU64::new(0),
            ingest_errors: AtomicU64::new(0),
            rate_limited_requests: AtomicU64::new(0),
            quota_exceeded_requests: AtomicU64::new(0),
        }
    }
}

impl OtelService {
    pub fn new(
        storage: Arc<dyn OtelStorage>,
        auth_service: Arc<OtelAuthService>,
        rate_limiter: Arc<RateLimiter>,
        max_concurrent_ingest_requests: usize,
    ) -> Self {
        Self {
            storage,
            auth_service,
            rate_limiter,
            service_rate_limiter: Arc::new(RateLimiter::new(
                SERVICE_INGEST_MAX_REQUESTS,
                SERVICE_INGEST_WINDOW,
            )),
            quota_cache: Arc::new(QuotaCache::new(QUOTA_CACHE_TTL)),
            ingest_semaphore: Arc::new(Semaphore::new(max_concurrent_ingest_requests)),
            ingest_permit_limit: max_concurrent_ingest_requests,
            cloud_link: None,
            cloud_policy_cache: None,
            span_outbox: None,
            write_mode_service: None,
            stats: PipelineStatsAtomic::default(),
        }
    }

    /// Attach the optional managed telemetry mirror. The mirror is offered
    /// spans only after local durable storage succeeds.
    pub fn with_cloud_link(mut self, cloud_link: Arc<temps_cloud_client::CloudLink>) -> Self {
        self.cloud_link = Some(cloud_link);
        self
    }

    /// Attach the per-project Cloud telemetry fidelity resolver (ADR-040 §1).
    ///
    /// Without it every mirrored span keeps the `Metered` projection, so a
    /// deployment that never wires this cannot accidentally widen egress.
    pub fn with_cloud_policy_cache(mut self, cache: Arc<CloudPolicyCache>) -> Self {
        self.cloud_policy_cache = Some(cache);
        self
    }

    /// The per-project Cloud fidelity resolver, when one is wired.
    ///
    /// Exposed so the settings path can invalidate a project's cached decision
    /// the moment an operator changes it, rather than leaving them staring at
    /// unchanged behaviour for up to one TTL.
    pub fn cloud_policy_cache(&self) -> Option<&Arc<CloudPolicyCache>> {
        self.cloud_policy_cache.as_ref()
    }

    /// Attach the durable outbox that backs Cloud-primary span writes
    /// (ADR-041 §3).
    ///
    /// Without it no project can be Cloud-primary regardless of what its row
    /// says, because the partition in [`Self::ingest_spans`] requires an outbox
    /// to enqueue into. That is the correct degradation: a deployment that has
    /// not wired the queue keeps storing spans locally rather than discovering
    /// at runtime that it has nowhere to put them.
    pub fn with_span_outbox(mut self, outbox: Arc<temps_cloud_client::SpanOutbox>) -> Self {
        self.span_outbox = Some(outbox);
        self
    }

    /// Attach the write-mode service, whose suspension flag the ingest path
    /// consults per batch (ADR-041 §7b).
    pub fn with_write_mode_service(
        mut self,
        service: Arc<crate::services::TelemetryWriteModeService>,
    ) -> Self {
        self.write_mode_service = Some(service);
        self
    }

    /// The durable outbox, when one is wired. Exposed for the status surfaces
    /// and the disconnect drain.
    pub fn span_outbox(&self) -> Option<&Arc<temps_cloud_client::SpanOutbox>> {
        self.span_outbox.as_ref()
    }

    /// Acquire an ingest slot without queueing more work in memory.
    pub fn try_acquire_ingest_permit(&self) -> Result<OwnedSemaphorePermit, OtelError> {
        self.ingest_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| OtelError::IngestSaturated {
                limit: self.ingest_permit_limit,
            })
    }

    /// Authenticate a token (API key `tk_` or deployment token `dt_`).
    pub async fn authenticate(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        self.auth_service
            .authenticate(token, header_project_id)
            .await
    }

    /// Authenticate any ingest token — project (`tk_`/`dt_`) or service (`si_`).
    ///
    /// Routes `si_` tokens to the service auth path; all others fall through
    /// to the existing project auth path.
    pub async fn authenticate_any(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<IngestAuth, OtelError> {
        self.auth_service
            .authenticate_any(token, header_project_id)
            .await
    }

    /// Check rate limit for a project.
    pub fn check_rate_limit(&self, project_id: i32) -> Result<(), OtelError> {
        if !self.rate_limiter.check_and_increment(project_id) {
            // Report the limiter's actual configured limit (set via
            // `TEMPS_OTEL_RATE_LIMIT`) so the error matches reality.
            self.stats
                .rate_limited_requests
                .fetch_add(1, Ordering::Relaxed);
            return Err(OtelError::RateLimitExceeded {
                project_id,
                limit: self.rate_limiter.max_requests(),
            });
        }
        Ok(())
    }

    /// Check the rate limit for a service-scoped (`si_`) ingest token, keyed by
    /// `service_id`. Used by the OTLP service-metrics path, which is otherwise
    /// unthrottled. Generous ceiling — see [`SERVICE_INGEST_MAX_REQUESTS`].
    pub fn check_service_rate_limit(&self, service_id: i32) -> Result<(), OtelError> {
        if !self.service_rate_limiter.check_and_increment(service_id) {
            return Err(OtelError::ServiceRateLimitExceeded {
                service_id,
                limit: self.service_rate_limiter.max_requests(),
            });
        }
        Ok(())
    }

    /// Check storage quota for a project.
    ///
    /// Storage quota is backed by an exact `COUNT(*)` scan over the OTel
    /// hypertables (see `TimescaleDbStorage::get_storage_quota`), which is
    /// too expensive to run on every ingest request under load. Reuse the
    /// cached result within [`QUOTA_CACHE_TTL`] and only hit storage once
    /// that expires.
    pub async fn check_quota(&self, project_id: i32) -> Result<(), OtelError> {
        let quota = match self.quota_cache.get(project_id) {
            Some(quota) => quota,
            None => {
                let quota = self.storage.get_storage_quota(project_id).await?;
                self.quota_cache.put(project_id, quota.clone());
                quota
            }
        };

        if quota.usage_pct >= 100.0 {
            self.stats
                .quota_exceeded_requests
                .fetch_add(1, Ordering::Relaxed);
            return Err(OtelError::QuotaExceeded {
                project_id,
                used_bytes: quota.total_bytes,
                limit_bytes: quota.limit_bytes,
            });
        }
        Ok(())
    }

    // ── Ingest operations ───────────────────────────────────────────

    /// Persist *why* a batch was dropped, so the reason survives past the log
    /// line and can be surfaced on the dashboard.
    ///
    /// **Best-effort and non-fatal by contract.** This runs on a path that is
    /// already failing, and the most likely reason it is failing is that a
    /// storage backend is down — the same backend this write may touch. A
    /// failure here is therefore expected, not exceptional: it is logged at
    /// `warn!` and swallowed. It must never change the ingest call's return
    /// value, its counters, or its retry behaviour, exactly like the S3
    /// archival failure in [`OtelService::ingest_logs`].
    ///
    /// Deliberately *not* retried: the caller has already spent its retry
    /// budget and is still holding an ingest permit (see
    /// [`OtelService::try_acquire_ingest_permit`]). Spending more time here
    /// would delay releasing that permit for bookkeeping.
    async fn record_ingest_failure(&self, signal_type: &str, error: &OtelError) {
        let error_class = error.error_class();
        if let Err(record_err) = self
            .storage
            .record_ingest_error(signal_type, error_class, &error.to_string())
            .await
        {
            warn!(
                signal = signal_type,
                error_class,
                error = %record_err,
                "Could not persist the ingest failure reason (non-fatal); \
                 the dropped-batch counters are still accurate"
            );
        }
    }

    /// Recent ingest-failure groups for the operator-facing report.
    pub async fn recent_ingest_errors(
        &self,
        limit: u32,
    ) -> Result<Vec<IngestErrorSummary>, OtelError> {
        self.storage.recent_ingest_errors(limit).await
    }

    /// Ingest metric data points.
    pub async fn ingest_metrics(&self, points: Vec<MetricPoint>) -> Result<u64, OtelError> {
        let count = points.len() as u64;
        self.stats
            .metrics_received
            .fetch_add(count, Ordering::Relaxed);

        // Retry the write on transient storage failures before counting the
        // batch as dropped — see `store_with_retry`. The batch is cloned per
        // attempt because `store_metrics` consumes it.
        let result = store_with_retry("metrics", || {
            let points = points.clone();
            async move { self.storage.store_metrics(points).await }
        })
        .await;

        match result {
            Ok(stored) => {
                self.stats
                    .metrics_stored
                    .fetch_add(stored, Ordering::Relaxed);
                Ok(stored)
            }
            Err(e) => {
                self.stats
                    .metrics_dropped
                    .fetch_add(count, Ordering::Relaxed);
                self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                error!(count, error = %e, "Failed to store metrics");
                self.record_ingest_failure("metrics", &e).await;
                Err(e)
            }
        }
    }

    /// Resolve the Cloud egress policy for every distinct project in a batch.
    ///
    /// Returns an empty map when no resolver is wired, which callers read as
    /// "everything is `Metered`" — the pre-ADR-040 behaviour.
    async fn resolve_cloud_policies(
        &self,
        spans: &[SpanRecord],
    ) -> std::collections::HashMap<i32, CloudTelemetryPolicy> {
        let Some(cache) = self.cloud_policy_cache.as_ref() else {
            return std::collections::HashMap::new();
        };
        cache
            .policies_for(spans.iter().map(|span| span.project_id))
            .await
    }

    /// Ingest trace spans — stores all received spans.
    ///
    /// Sampling is the client SDK's responsibility (head-based).
    /// The server stores everything it receives.
    ///
    /// # ADR-041 §2: the batch is partitioned by write mode
    ///
    /// - Spans whose project is `Local` → `store_spans` exactly as before, then
    ///   `link.record(..)` after the local write succeeds. Ordering unchanged,
    ///   retry behaviour unchanged, `Metered` projection byte identical.
    /// - Spans whose project is Cloud-primary → enqueued to the durable outbox.
    ///   **No local span write happens for these.** That is the entire point of
    ///   the ADR.
    ///
    /// A batch that mixes projects is normal and is not a special case.
    ///
    /// **Ingestion never blocks on Cloud.** The enqueue is a local durable
    /// write; the shipping is a background worker. A Cloud outage cannot add
    /// latency to an OTLP request, cannot consume an ingest permit, and cannot
    /// produce a 5xx to the customer's exporter.
    ///
    /// **What a 2xx means changes for Cloud-primary projects**, and this is
    /// documented rather than discovered: it means "committed to this
    /// instance's durable telemetry outbox", not "committed to local storage".
    /// Strictly weaker than before, strictly stronger than fire-and-forget.
    pub async fn ingest_spans(&self, spans: Vec<SpanRecord>) -> Result<u64, OtelError> {
        let count = spans.len() as u64;
        self.stats
            .spans_received
            .fetch_add(count, Ordering::Relaxed);

        if spans.is_empty() {
            return Ok(0);
        }

        // ── Deployment shape A: no Cloud link ─────────────────────────────
        //
        // Nothing below this line runs for the ~default install. No policy
        // lookup, no partition, no outbox, no new query on the ingest path —
        // the same two statements this method has always executed.
        let Some(link) = self.cloud_link.as_ref() else {
            return self.store_spans_locally(spans, None).await;
        };
        if !link.is_linked() || !link.telemetry_enabled() {
            return self.store_spans_locally(spans, None).await;
        }

        // One lookup per *distinct* project in the batch, served from a TTL
        // cache — never one per span. Unresolved projects fall back to
        // `Metered` + `Local`, so a lookup problem can only narrow egress and
        // can only ever store *more*.
        let policies = self.resolve_cloud_policies(&spans).await;
        let metered = CloudTelemetryPolicy::metered();

        // Cloud-primary writes are suspended process-wide while Cloud is
        // refusing for a reason only the operator can fix (ADR-041 §7b). One
        // relaxed atomic load, evaluated per batch rather than cached with the
        // policy, so a quota event resumes local writes on the very next batch
        // instead of up to one TTL later.
        let cloud_writes_available = self
            .write_mode_service
            .as_ref()
            .is_none_or(|service| !service.suspension().is_suspended())
            && self.span_outbox.is_some();

        let mut local_spans: Vec<SpanRecord> = Vec::with_capacity(spans.len());
        let mut cloud_by_project: std::collections::HashMap<
            i32,
            Vec<temps_cloud_protocol::SpanRecord>,
        > = std::collections::HashMap::new();

        for span in &spans {
            let policy = policies.get(&span.project_id).unwrap_or(&metered);
            let cloud_primary = cloud_writes_available && policy.is_cloud_primary();
            if !cloud_primary {
                local_spans.push(span.clone());
                continue;
            }
            match cloud_span(link, span, policy) {
                Some(projected) => cloud_by_project
                    .entry(span.project_id)
                    .or_default()
                    .push(projected),
                None => {
                    // The projection refused — telemetry export was switched
                    // off between the policy read and here, or the link lost
                    // its credential. A Cloud-primary span with nowhere to go
                    // is stored locally rather than dropped: there is never a
                    // state in which the instance stores spans nowhere.
                    local_spans.push(span.clone());
                }
            }
        }

        // The mirror projection for `Local` projects, unchanged from before.
        let mirror: Vec<temps_cloud_protocol::SpanRecord> = local_spans
            .iter()
            .filter_map(|span| {
                let policy = policies.get(&span.project_id).unwrap_or(&metered);
                cloud_span(link, span, policy)
            })
            .collect();

        // Enqueue first, so a Cloud-primary project's spans are durable before
        // the request is acknowledged. This is a local Postgres write and does
        // not touch the network.
        let mut queued = 0u64;
        if !cloud_by_project.is_empty() {
            let Some(outbox) = self.span_outbox.as_ref() else {
                // Unreachable: `cloud_writes_available` already required one.
                // Kept as a branch rather than an `expect` because losing a
                // customer's spans to a panic on the ingest path is the worst
                // possible way to be wrong about that.
                return self.store_spans_locally(spans, Some((link, mirror))).await;
            };
            for (project_id, projected) in &cloud_by_project {
                match outbox.enqueue(*project_id, projected).await {
                    Ok(outcome) => {
                        queued += outcome.accepted as u64;
                        if outcome.refused_any() {
                            // Refused at the byte cap. Already recorded as a
                            // gap window with a start and an end; counted here
                            // so the pipeline's dropped total stays honest.
                            self.stats
                                .spans_dropped
                                .fetch_add(outcome.refused as u64, Ordering::Relaxed);
                            warn!(
                                project_id,
                                refused = outcome.refused,
                                refused_bytes = outcome.refused_bytes,
                                "Telemetry outbox is at its byte cap; these spans were not \
                                 captured and are recorded as a gap window"
                            );
                        }
                    }
                    Err(error) => {
                        let error: OtelError = error.into();
                        self.stats.spans_dropped.fetch_add(count, Ordering::Relaxed);
                        self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                        error!(
                            project_id,
                            spans = projected.len(),
                            error = %error,
                            "Failed to enqueue Cloud-primary spans; answering the exporter with \
                             an error so its retry can still save them"
                        );
                        self.record_ingest_failure("spans", &error).await;
                        return Err(error);
                    }
                }
            }
        }

        if local_spans.is_empty() {
            // Every project in this batch is Cloud-primary. No local span write
            // at all — the property the whole ADR exists to deliver.
            self.stats.spans_stored.fetch_add(queued, Ordering::Relaxed);
            return Ok(queued);
        }

        let stored = self
            .store_spans_locally(local_spans, Some((link, mirror)))
            .await?;
        Ok(stored + queued)
    }

    /// The pre-ADR-041 write path, unchanged: store locally with a bounded
    /// retry, then offer the mirror projection once the local write succeeded.
    ///
    /// Extracted verbatim so deployment shape A (no Cloud link) executes
    /// exactly the same statements in exactly the same order as before, and so
    /// that fact is checkable by reading one function rather than diffing a
    /// branch.
    async fn store_spans_locally(
        &self,
        spans: Vec<SpanRecord>,
        mirror: Option<(
            &Arc<temps_cloud_client::CloudLink>,
            Vec<temps_cloud_protocol::SpanRecord>,
        )>,
    ) -> Result<u64, OtelError> {
        let count = spans.len() as u64;

        // Retry on transient storage failures — see `store_with_retry`. Two
        // failed ClickHouse writes used to mean 1,024 permanently lost spans
        // with no recovery attempt at all.
        let result = store_with_retry("spans", || {
            let spans = spans.clone();
            async move { self.storage.store_spans(spans).await }
        })
        .await;

        match result {
            Ok(stored) => {
                self.stats.spans_stored.fetch_add(stored, Ordering::Relaxed);
                if let Some((link, mirror)) = mirror {
                    link.record(mirror);
                }
                Ok(stored)
            }
            Err(e) => {
                self.stats.spans_dropped.fetch_add(count, Ordering::Relaxed);
                self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                error!(count, error = %e, "Failed to store spans");
                self.record_ingest_failure("spans", &e).await;
                Err(e)
            }
        }
    }

    /// Ingest log records.
    ///
    /// Routes ERROR/WARN to DB for fast search, archives all to S3.
    pub async fn ingest_logs(&self, records: Vec<LogRecord>) -> Result<u64, OtelError> {
        let count = records.len() as u64;
        self.stats.logs_received.fetch_add(count, Ordering::Relaxed);

        // Store EVERY record in the queryable DB (all severities), so the Logs
        // explorer and trace↔log correlation surface INFO/DEBUG too — not just
        // WARN+. Apps log overwhelmingly at INFO, so a WARN+-only DB (the prior
        // behaviour) made the explorer show almost nothing and a trace show one
        // stray warning. All records are also archived to S3 for retention.
        // (If hot-storage cost becomes a concern, reintroduce a *configurable*
        // per-project minimum severity rather than a hard-coded WARN floor.)
        let db_count = count;
        // Retry the DB write on transient storage failures — see
        // `store_with_retry`. S3 archival below is left un-retried: it is
        // already non-fatal, and doubling the permit hold time for a
        // best-effort cold copy is the wrong trade.
        let db_result = store_with_retry("logs", || {
            let records = records.clone();
            async move { self.storage.store_logs(records).await }
        })
        .await;

        match db_result {
            Ok(stored) => {
                self.stats
                    .logs_stored_db
                    .fetch_add(stored, Ordering::Relaxed);
            }
            Err(e) => {
                self.stats
                    .logs_dropped
                    .fetch_add(db_count, Ordering::Relaxed);
                self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                error!(db_count, error = %e, "Failed to store log records in DB");
                self.record_ingest_failure("logs", &e).await;
            }
        }

        // Archive all records to S3
        match self.storage.archive_logs(records).await {
            Ok(archived) => {
                self.stats
                    .logs_stored_s3
                    .fetch_add(archived, Ordering::Relaxed);
            }
            Err(e) => {
                // S3 archival failure is non-fatal
                warn!(count, error = %e, "Failed to archive logs to S3");
            }
        }

        Ok(count)
    }

    // ── Query operations ────────────────────────────────────────────

    pub async fn query_metrics(&self, query: MetricQuery) -> Result<Vec<MetricBucket>, OtelError> {
        // Validate every label key used for filtering or grouping against the
        // same allowlist the ingest path enforces on metric/attribute names.
        // Keys flow into the store's SQL (as bound map indices); rejecting bad
        // keys here is a defence-in-depth trust boundary on top of the store's
        // own check. Values are always bound, never validated for content.
        for key in query
            .group_by
            .iter()
            .chain(query.label_filters.iter().map(|(k, _)| k))
        {
            if temps_metrics::validate_metric_name(key).is_err() {
                return Err(OtelError::Validation {
                    message: format!(
                        "metric query label key '{key}' contains characters outside the allowed set [a-zA-Z0-9_.:-]"
                    ),
                });
            }
        }
        self.storage.query_metrics(query).await
    }

    pub async fn list_metric_names(&self, project_id: i32) -> Result<Vec<String>, OtelError> {
        self.storage.list_metric_names(project_id).await
    }

    pub async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>, OtelError> {
        self.storage
            .list_metric_label_keys(project_id, metric_name, start_time, end_time)
            .await
    }

    pub async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>, OtelError> {
        // Defence-in-depth: the key flows into the store's SQL (bound, but mirror
        // the `query_metrics` trust boundary) — reject anything off the allowlist.
        if temps_metrics::validate_metric_name(label_key).is_err() {
            return Err(OtelError::Validation {
                message: format!(
                    "label key '{label_key}' contains characters outside the allowed set [a-zA-Z0-9_.:-]"
                ),
            });
        }
        self.storage
            .list_metric_label_values(project_id, metric_name, label_key, start_time, end_time)
            .await
    }

    pub async fn query_spans(&self, query: TraceQuery) -> Result<Vec<SpanRecord>, OtelError> {
        self.storage.query_spans(query).await
    }

    pub async fn query_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> Result<Vec<TraceSummary>, OtelError> {
        self.storage.query_trace_summaries(query).await
    }

    pub async fn count_traces(&self, query: TraceQuery) -> Result<u64, OtelError> {
        self.storage.count_traces(query).await
    }

    /// Whether `project_id` has ever received at least one span — the
    /// existence check backing onboarding/setup UI. Cheaper than
    /// `count_traces`/`query_trace_summaries` for that purpose: see
    /// [`crate::storage::OtelStorage::has_traces`].
    pub async fn has_traces(&self, project_id: i32) -> Result<bool, OtelError> {
        self.storage.has_traces(project_id).await
    }

    pub async fn get_trace(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<SpanRecord>, OtelError> {
        self.storage.get_trace(project_id, trace_id).await
    }

    /// Per-operation latency statistics for the queried window.
    ///
    /// Validates the window here rather than in the handler so every caller —
    /// including future internal ones — gets the same guarantee the storage
    /// backends rely on: a bounded, correctly-ordered time range over at least
    /// one project.
    pub async fn query_span_stats(
        &self,
        query: SpanStatsQuery,
    ) -> Result<Vec<SpanStats>, OtelError> {
        Self::validate_span_stats_query(&query)?;
        self.storage.query_span_stats(query).await
    }

    /// Count the operations a span-stats query matches, for pagination.
    pub async fn count_span_stats(&self, query: SpanStatsQuery) -> Result<u64, OtelError> {
        Self::validate_span_stats_query(&query)?;
        self.storage.count_span_stats(query).await
    }

    fn validate_span_stats_query(query: &SpanStatsQuery) -> Result<(), OtelError> {
        if query.project_ids.is_empty() {
            return Err(OtelError::Validation {
                message: "span-stats requires at least one project id".to_string(),
            });
        }
        if query.project_ids.len() > SPAN_STATS_MAX_PROJECTS {
            return Err(OtelError::Validation {
                message: format!(
                    "span-stats accepts at most {} projects per query, got {}",
                    SPAN_STATS_MAX_PROJECTS,
                    query.project_ids.len()
                ),
            });
        }
        if query.end_time <= query.start_time {
            return Err(OtelError::Validation {
                message: format!(
                    "span-stats time window is empty or inverted: start_time {} is not before \
                     end_time {}",
                    query.start_time.to_rfc3339(),
                    query.end_time.to_rfc3339()
                ),
            });
        }
        // Reject rather than silently truncate: a caller asking for 90 days and
        // getting 31 back would read the result as "the last 90 days", and the
        // whole point of the report is that the numbers mean what they say.
        let window = query.end_time - query.start_time;
        if window > chrono::Duration::days(SPAN_STATS_MAX_WINDOW_DAYS) {
            return Err(OtelError::Validation {
                message: format!(
                    "span-stats time window is {} days, which exceeds the {}-day maximum; \
                     narrow start_time/end_time",
                    window.num_days(),
                    SPAN_STATS_MAX_WINDOW_DAYS
                ),
            });
        }
        Ok(())
    }

    pub async fn query_logs(&self, query: LogQuery) -> Result<Vec<LogRecord>, OtelError> {
        self.storage.query_logs(query).await
    }

    pub async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> Result<Vec<GenAiTraceSummary>, OtelError> {
        self.storage.query_genai_trace_summaries(query).await
    }

    pub async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<GenAiSpanDetail>, OtelError> {
        self.storage
            .get_genai_trace_spans(project_id, trace_id)
            .await
    }

    pub async fn count_genai_traces(&self, query: TraceQuery) -> Result<u64, OtelError> {
        self.storage.count_genai_traces(query).await
    }

    pub async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<GenAiEvent>, OtelError> {
        self.storage
            .get_genai_trace_events(project_id, trace_id)
            .await
    }

    pub async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Insight>, OtelError> {
        self.storage
            .list_insights(project_id, status, limit, offset)
            .await
    }

    pub async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<HealthSummary>, OtelError> {
        self.storage
            .get_health_summaries(project_id, environment_id)
            .await
    }

    pub async fn get_storage_quota(&self, project_id: i32) -> Result<StorageQuota, OtelError> {
        self.storage.get_storage_quota(project_id).await
    }

    // ── Observability ───────────────────────────────────────────────

    /// Get pipeline statistics snapshot.
    pub fn pipeline_stats(&self) -> PipelineStats {
        PipelineStats {
            metrics_received: self.stats.metrics_received.load(Ordering::Relaxed),
            metrics_stored: self.stats.metrics_stored.load(Ordering::Relaxed),
            metrics_dropped: self.stats.metrics_dropped.load(Ordering::Relaxed),
            spans_received: self.stats.spans_received.load(Ordering::Relaxed),
            spans_stored: self.stats.spans_stored.load(Ordering::Relaxed),
            spans_dropped: self.stats.spans_dropped.load(Ordering::Relaxed),
            logs_received: self.stats.logs_received.load(Ordering::Relaxed),
            logs_stored_db: self.stats.logs_stored_db.load(Ordering::Relaxed),
            logs_stored_s3: self.stats.logs_stored_s3.load(Ordering::Relaxed),
            logs_dropped: self.stats.logs_dropped.load(Ordering::Relaxed),
            ingest_errors: self.stats.ingest_errors.load(Ordering::Relaxed),
            rate_limited_requests: self.stats.rate_limited_requests.load(Ordering::Relaxed),
            quota_exceeded_requests: self.stats.quota_exceeded_requests.load(Ordering::Relaxed),
        }
    }

    /// Access to storage for background jobs (anomaly detection, health computation).
    pub fn storage(&self) -> &Arc<dyn OtelStorage> {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::StorageErrorKind;
    use crate::ingest::decode;
    use crate::test_support::{self, MockOtelStorage};
    use std::time::Duration;

    fn linked_cloud() -> (tempfile::TempDir, Arc<temps_cloud_client::CloudLink>) {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("cloud-link/state.json");
        let mut state = temps_cloud_client::EnrollmentState::new("https://cloud.test/");
        state.token = Some("instance-token".to_string());
        state.tenant_id = Some(uuid::Uuid::new_v4());
        state.account_email = Some("owner@example.com".to_string());
        state.save(&state_path).unwrap();
        let link = Arc::new(temps_cloud_client::CloudLink::load(
            directory.path().to_path_buf(),
            "test",
        ));
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("enable telemetry export");
        (directory, link)
    }

    fn make_service(storage: MockOtelStorage) -> (OtelService, MockOtelStorage) {
        let storage_clone = storage.clone();
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let limiter = Arc::new(RateLimiter::new(1000, Duration::from_secs(60)));
        let svc = OtelService::new(
            Arc::new(storage) as Arc<dyn OtelStorage>,
            auth,
            limiter,
            DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
        );
        (svc, storage_clone)
    }

    #[test]
    fn test_pipeline_stats_default() {
        let stats = PipelineStatsAtomic::default();
        assert_eq!(stats.metrics_received.load(Ordering::Relaxed), 0);
        assert_eq!(stats.ingest_errors.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_ingest_spans_stores_and_tracks_stats() {
        let mock = MockOtelStorage::new();
        let (svc, _storage) = make_service(mock);

        let (_trace_id, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        assert_eq!(spans.len(), 4);

        let stored = svc.ingest_spans(spans).await.unwrap();

        // All received spans are stored (no server-side sampling)
        assert_eq!(stored, 4);
        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 4);
        assert_eq!(stats.spans_stored, 4);
    }

    #[tokio::test]
    async fn successful_local_ingest_offers_spans_to_the_cloud_mirror() {
        let mock = MockOtelStorage::new();
        let (svc, _) = make_service(mock);
        let (_directory, link) = linked_cloud();
        let svc = svc.with_cloud_link(link.clone());
        let (_, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();

        svc.ingest_spans(spans).await.unwrap();

        assert_eq!(link.spooled(), 4);
    }

    #[tokio::test]
    async fn linking_alone_does_not_offer_spans_to_cloud() {
        let mock = MockOtelStorage::new();
        let (svc, _) = make_service(mock);
        let (_directory, link) = linked_cloud();
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("disable telemetry export");
        let svc = svc.with_cloud_link(link.clone());
        let (_, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();

        svc.ingest_spans(spans).await.unwrap();

        assert_eq!(link.spooled(), 0);
    }

    #[test]
    fn cloud_mirror_strips_application_controlled_names_and_attributes() {
        let (_, encoded) = test_support::build_sample_trace_tree();
        let mut spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        spans[0].attributes.insert(
            "http.request.header.authorization".into(),
            "Bearer must-not-leave".into(),
        );
        spans[0].name = "SELECT * FROM users WHERE email='secret@example.com'".into();

        let (_directory, link) = linked_cloud();
        let mirrored = cloud_span(&link, &spans[0], &CloudTelemetryPolicy::metered()).unwrap();

        assert!(mirrored.attributes.is_empty());
        assert_ne!(mirrored.trace_id, spans[0].trace_id);
        assert_ne!(mirrored.span_id, spans[0].span_id);
        assert_eq!(mirrored.trace_id.len(), 64);
        assert_eq!(mirrored.span_id.len(), 64);
        assert_eq!(mirrored.name, "span");
        let serialized = serde_json::to_string(&mirrored).unwrap();
        assert!(!serialized.contains("secret@example.com"));
        assert!(!serialized.contains("must-not-leave"));
    }

    // ── ADR-040 §1 fidelity invariants ──────────────────────────────────
    //
    // These are the contract, not coverage. The first pins that opting *out*
    // (the default) changed nothing on the wire; the second pins that opting
    // *in* still ships no attributes unless a key was explicitly listed.

    /// A span carrying every field `Metered` must strip: an application-
    /// controlled name, an attribute, a real parent, an environment, a service
    /// name and an error status.
    ///
    /// Identifiers and timestamp are fixed, not decoded from a fixture, so the
    /// expected wire bytes below can be written out literally.
    fn metered_fixture_span(trace_id: &str, span_id: &str) -> SpanRecord {
        let start_time = chrono::DateTime::from_timestamp_millis(1_700_000_000_000)
            .expect("fixed timestamp is in range");
        SpanRecord {
            project_id: 7,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "checkout".to_string(),
                service_version: Some("1.4.2".to_string()),
                deployment_environment: Some("production".to_string()),
                attributes: Default::default(),
            },
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            parent_span_id: Some("00f067aa0ba902b6".to_string()),
            name: "SELECT * FROM users WHERE email='secret@example.com'".to_string(),
            kind: SpanKind::Server,
            start_time,
            end_time: start_time + chrono::Duration::milliseconds(13),
            duration_ms: 12.5,
            status_code: SpanStatusCode::Error,
            status_message: "boom".to_string(),
            attributes: [
                ("db.statement".to_string(), "must-not-leave".to_string()),
                ("http.route".to_string(), "/orders".to_string()),
            ]
            .into_iter()
            .collect(),
            events: Vec::new(),
        }
    }

    #[test]
    fn the_metered_projection_serializes_to_exactly_the_pre_adr_040_bytes() {
        // ADR-040 makes `Metered` the default for every existing and future
        // project. If this ever diverges, an upgrade silently changes what
        // leaves every linked instance that never opted in.
        //
        // The expectation is a literal, hand-written string rather than a
        // re-implementation of the old projection: a parallel implementation
        // edited in the same commit as `cloud_span` would agree with the bug,
        // and a `serde_json::Value` comparison would additionally depend on
        // `serde_json`'s `preserve_order` feature happening to be enabled
        // somewhere in the dependency tree. `temps-cloud-protocol`'s
        // `a_metered_record_serializes_without_any_queryable_keys` pins the
        // same property one layer down, the same way.
        //
        // The two 64-hex values are HMAC-SHA256(<the fixture's instance token>,
        // "trace"|"span" || 0x00 || <id>), i.e. what `pseudonymize_telemetry_id`
        // produces for `linked_cloud()`'s fixed token. Changing that token
        // changes these literals, deliberately: pseudonymisation is part of the
        // contract being pinned.
        let cases: [(SpanRecord, &str); 2] = [
            (
                metered_fixture_span("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7"),
                r#"{"trace_id":"4c80dab45fa7b33603dcdba132044a7186f0cbab5778a8f26f4cc7117728c328","span_id":"0da6b04cd7e98c634420df49c63d54dec287504a63c73683d95640a25131d045","name":"span","ts_millis":1700000000000,"duration_ms":12.5,"attributes":{}}"#,
            ),
            (
                metered_fixture_span("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331"),
                r#"{"trace_id":"f420acecc6b61aebb78f23b3332bb8d5a29703d7d300573fa2d4fcd2094db6f7","span_id":"7c3889741fdf60683a82e0216f810edd6473a66dae781e9cdc0a27d50020699a","name":"span","ts_millis":1700000000000,"duration_ms":12.5,"attributes":{}}"#,
            ),
        ];

        let (_directory, link) = linked_cloud();

        for (span, expected_json) in &cases {
            let actual = cloud_span(&link, span, &CloudTelemetryPolicy::metered())
                .expect("the metered projection must build for a linked instance");

            assert_eq!(
                serde_json::to_string(&actual).expect("record must serialize"),
                *expected_json,
                "the metered projection must be byte-identical to the pre-ADR-040 one"
            );

            // ...and field-by-field, so a serde attribute change cannot make
            // the byte comparison pass for the wrong reason.
            assert_ne!(actual.trace_id, span.trace_id);
            assert_ne!(actual.span_id, span.span_id);
            assert_eq!(actual.name, "span");
            assert_eq!(actual.ts_millis, 1_700_000_000_000);
            assert_eq!(actual.duration_ms, 12.5);
            assert!(actual.attributes.is_empty());
            assert_eq!(actual.project_ref, "");
            assert_eq!(actual.service_name, None);
            assert_eq!(actual.span_kind, None);
            assert_eq!(actual.status_code, None);
            assert_eq!(actual.parent_span_id, None);
            assert_eq!(actual.environment, None);
        }
    }

    #[test]
    fn queryable_fidelity_with_an_empty_allowlist_ships_no_attributes_at_all() {
        // The safe default the whole opt-in leans on: raising fidelity makes a
        // span renderable, it does NOT open the attribute map. An operator has
        // to list each key by hand.
        let (_, encoded) = test_support::build_sample_trace_tree();
        let mut spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        spans[0].attributes.insert(
            "http.request.header.authorization".into(),
            "Bearer must-not-leave".into(),
        );
        spans[0]
            .attributes
            .insert("db.statement".into(), "SELECT secret@example.com".into());
        spans[0]
            .attributes
            .insert("http.route".into(), "/orders".into());

        let (_directory, link) = linked_cloud();
        let mirrored = cloud_span(
            &link,
            &spans[0],
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
        )
        .expect("the queryable projection must build for a linked instance");

        assert!(
            mirrored.attributes.is_empty(),
            "an empty allowlist must ship zero attributes even at queryable fidelity"
        );
        let serialized = serde_json::to_string(&mirrored).expect("record must serialize");
        assert!(!serialized.contains("must-not-leave"));
        assert!(!serialized.contains("secret@example.com"));
        assert!(!serialized.contains("/orders"));
        // The rest of the queryable projection is still present — this is a
        // narrowing of attributes only, not a fallback to `Metered`.
        assert_eq!(mirrored.trace_id, spans[0].trace_id);
        assert_eq!(mirrored.name, spans[0].name);
    }

    #[test]
    fn queryable_fidelity_copies_only_exact_allowlisted_attribute_keys() {
        let (_, encoded) = test_support::build_sample_trace_tree();
        let mut spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        spans[0]
            .attributes
            .insert("http.route".into(), "/orders".into());
        spans[0]
            .attributes
            .insert("http.route.template".into(), "must-not-leave".into());
        spans[0]
            .attributes
            .insert("db.statement".into(), "must-not-leave".into());

        let (_directory, link) = linked_cloud();
        let policy = CloudTelemetryPolicy::queryable(["http.route".to_string()]);
        let mirrored = cloud_span(&link, &spans[0], &policy)
            .expect("the queryable projection must build for a linked instance");

        assert_eq!(mirrored.attributes.len(), 1);
        assert_eq!(
            mirrored.attributes.get("http.route").map(String::as_str),
            Some("/orders")
        );
        let serialized = serde_json::to_string(&mirrored).expect("record must serialize");
        assert!(
            !serialized.contains("must-not-leave"),
            "a near-miss key must not be treated as a prefix match: {serialized}"
        );
    }

    #[test]
    fn queryable_fidelity_emits_the_renderable_fields_and_a_pseudonymous_project_ref() {
        let (_, encoded) = test_support::build_sample_trace_tree();
        let mut spans = decode::decode_traces_request(&encoded, 7, None).unwrap();
        spans[0].parent_span_id = Some("00f067aa0ba902b6".into());
        spans[0].resource.service_name = "checkout".into();
        spans[0].resource.deployment_environment = Some("production".into());

        let (_directory, link) = linked_cloud();
        let mirrored = cloud_span(
            &link,
            &spans[0],
            &CloudTelemetryPolicy::queryable(std::iter::empty()),
        )
        .expect("the queryable projection must build for a linked instance");

        // Real identifiers, deliberately (ADR-040 §1): a pseudonym would match
        // nothing in the user's own logs.
        assert_eq!(mirrored.trace_id, spans[0].trace_id);
        assert_eq!(mirrored.span_id, spans[0].span_id);
        assert_eq!(mirrored.name, spans[0].name);
        assert_eq!(mirrored.parent_span_id, Some("00f067aa0ba902b6".into()));
        assert_eq!(mirrored.service_name, Some("checkout".into()));
        assert_eq!(mirrored.environment, Some("production".into()));
        assert_eq!(mirrored.span_kind, Some(spans[0].kind.to_string()));
        assert_eq!(mirrored.status_code, Some(spans[0].status_code.to_string()));

        // The project, by contrast, is referenced only by an HMAC pseudonym —
        // stable so Cloud can scope by it, opaque so Cloud never learns the
        // local project id or name.
        assert_eq!(mirrored.project_ref.len(), 64);
        assert!(mirrored.project_ref.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(mirrored.project_ref, "7");
        assert_eq!(
            mirrored.project_ref,
            link.pseudonymize_telemetry_id("project", "7")
                .expect("project pseudonym must derive"),
            "project_ref must reuse the existing pseudonymisation helper"
        );
    }

    #[test]
    fn the_project_pseudonym_is_domain_separated_from_trace_and_span_ids() {
        // Same input value, three domains: if they collided, a project_ref
        // would be forgeable from a trace id and vice versa.
        let (_directory, link) = linked_cloud();
        let project = link.pseudonymize_telemetry_id("project", "7").unwrap();
        let trace = link.pseudonymize_telemetry_id("trace", "7").unwrap();
        let span = link.pseudonymize_telemetry_id("span", "7").unwrap();

        assert_ne!(project, trace);
        assert_ne!(project, span);
        assert_ne!(trace, span);
    }

    #[tokio::test]
    async fn a_service_without_a_policy_cache_mirrors_at_metered_fidelity() {
        // The unwired case must be the *narrow* one. If this ever inverted,
        // any deployment that skipped the wiring would start shipping real
        // span names without anyone opting in.
        let mock = MockOtelStorage::new();
        let (svc, _) = make_service(mock);
        let (_directory, link) = linked_cloud();
        let svc = svc.with_cloud_link(link.clone());

        let (_, encoded) = test_support::build_sample_trace_tree();
        let mut spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        spans[0].name = "SELECT * FROM users WHERE email='secret@example.com'".into();

        assert!(svc.resolve_cloud_policies(&spans).await.is_empty());
        svc.ingest_spans(spans).await.unwrap();
        assert_eq!(link.spooled(), 4);
    }

    #[tokio::test]
    async fn test_ingest_spans_error_span_stored() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Build a trace with one error span
        let trace_id: [u8; 16] = [0xAA; 16];
        let span_id: [u8; 8] = [0xBB; 8];
        let error_span = test_support::span(
            &trace_id,
            &span_id,
            &[],
            "failing-op",
            2,
            1_700_000_000_000_000_000,
            1_700_000_000_050_000_000,
            2, // ERROR
        );
        let res = test_support::resource("err-service");
        let request = test_support::trace_request(res, vec![error_span]);
        let encoded = test_support::encode_proto(&request);

        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        let stored = svc.ingest_spans(spans).await.unwrap();

        assert_eq!(stored, 1);
        let stored_spans = storage.stored_spans();
        assert_eq!(stored_spans.len(), 1);
        assert_eq!(stored_spans[0].status_code, SpanStatusCode::Error);
    }

    #[tokio::test]
    async fn test_ingest_spans_storage_failure_tracks_stats() {
        let mock = MockOtelStorage::new();
        *mock.fail_store_spans.lock().unwrap() = Some("disk full".into());
        let (svc, _storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();

        let result = svc.ingest_spans(spans).await;
        assert!(result.is_err());

        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 1);
        assert!(stats.spans_dropped > 0 || stats.ingest_errors > 0);
    }

    // ── Bounded retry around the ingest storage write ───────────────────

    /// Encoded OTLP payload with exactly one ERROR span, for the retry tests.
    fn single_error_span_payload() -> Vec<u8> {
        let trace_id: [u8; 16] = [0xCC; 16];
        let span_id: [u8; 8] = [0xDD; 8];
        let error_span = test_support::span(
            &trace_id,
            &span_id,
            &[],
            "op",
            2,
            1_700_000_000_000_000_000,
            1_700_000_000_050_000_000,
            2,
        );
        let res = test_support::resource("svc");
        let request = test_support::trace_request(res, vec![error_span]);
        test_support::encode_proto(&request)
    }

    /// A transient blip on the first write must be healed by the retry: the
    /// batch lands, nothing is counted as dropped, and the caller sees a
    /// normal success.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_spans_retries_transient_failure_then_succeeds() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", Some(1));
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();

        let stored = svc
            .ingest_spans(spans)
            .await
            .expect("transient failure should be retried, not surfaced");

        assert_eq!(stored, 1);
        assert_eq!(
            storage.store_spans_call_count(),
            2,
            "expected one failed attempt plus one successful retry"
        );
        assert_eq!(storage.stored_spans().len(), 1, "batch must survive");

        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 1);
        assert_eq!(stats.spans_stored, 1);
        assert_eq!(stats.spans_dropped, 0, "a healed blip must not count drops");
        assert_eq!(stats.ingest_errors, 0);
    }

    /// A terminal error must fail on the very first attempt — no sleeping, no
    /// second call — because retrying a malformed batch only holds the ingest
    /// permit longer for the same outcome.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_spans_fatal_failure_does_not_retry() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_fatally("schema mismatch: column `duration_ms` is missing");
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();

        let started = tokio::time::Instant::now();
        let result = svc.ingest_spans(spans).await;
        let elapsed = started.elapsed();

        assert!(matches!(
            result,
            Err(OtelError::Storage {
                kind: StorageErrorKind::ClickHouseSchema,
                ..
            })
        ));
        assert_eq!(
            storage.store_spans_call_count(),
            1,
            "a terminal error must not be retried"
        );
        assert_eq!(
            elapsed,
            Duration::ZERO,
            "no backoff may be incurred for a terminal error"
        );

        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_dropped, 1);
        assert_eq!(stats.ingest_errors, 1);
    }

    /// A sustained outage still surfaces as an error after the bounded retry
    /// is exhausted, and the counters must reflect one *ingest* failure — not
    /// one per attempt — with the whole batch counted as dropped exactly once.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_spans_exhausts_retries_and_counts_once() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", None);
        let (svc, storage) = make_service(mock);

        let (_trace_id, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        assert_eq!(spans.len(), 4);

        let result = svc.ingest_spans(spans).await;

        assert!(matches!(
            result,
            Err(OtelError::Storage {
                kind: StorageErrorKind::ClickHouseNetwork,
                ..
            })
        ));
        assert_eq!(
            storage.store_spans_call_count(),
            STORAGE_WRITE_MAX_ATTEMPTS,
            "expected the full bounded attempt budget"
        );

        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 4);
        assert_eq!(stats.spans_stored, 0);
        assert_eq!(
            stats.spans_dropped, 4,
            "the batch is dropped once, not once per attempt"
        );
        assert_eq!(
            stats.ingest_errors, 1,
            "3 attempts are still a single ingest failure"
        );
    }

    // ── Metrics ingest: bounded retry around the storage write ──────────
    //
    // Mirrors the spans retry tests above (`test_ingest_spans_retries_*`,
    // `test_ingest_spans_fatal_failure_does_not_retry`,
    // `test_ingest_spans_exhausts_retries_and_counts_once`) — before this,
    // only the spans path had failure-injection coverage for
    // `store_with_retry`, even though `ingest_metrics` and `ingest_logs` go
    // through the exact same helper.

    /// A transient blip on the first metrics write must be healed by the
    /// retry: the batch lands, nothing is counted as dropped, and the caller
    /// sees a normal success.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_metrics_retries_transient_failure_then_succeeds() {
        let mock = MockOtelStorage::new();
        mock.fail_store_metrics_transiently("connection reset by peer", Some(1));
        let (svc, storage) = make_service(mock);

        let point = test_support::metric_point(1, "http.server.duration", chrono::Utc::now(), &[]);

        let stored = svc
            .ingest_metrics(vec![point])
            .await
            .expect("transient failure should be retried, not surfaced");

        assert_eq!(stored, 1);
        assert_eq!(
            storage.store_metrics_call_count(),
            2,
            "expected one failed attempt plus one successful retry"
        );
        assert_eq!(storage.stored_metrics().len(), 1, "batch must survive");

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_received, 1);
        assert_eq!(stats.metrics_stored, 1);
        assert_eq!(
            stats.metrics_dropped, 0,
            "a healed blip must not count drops"
        );
        assert_eq!(stats.ingest_errors, 0);
    }

    /// A terminal error must fail the metrics write on the very first
    /// attempt — no sleeping, no second call.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_metrics_fatal_failure_does_not_retry() {
        let mock = MockOtelStorage::new();
        mock.fail_store_metrics_fatally("schema mismatch: column `histogram_bounds` is missing");
        let (svc, storage) = make_service(mock);

        let point = test_support::metric_point(1, "http.server.duration", chrono::Utc::now(), &[]);

        let started = tokio::time::Instant::now();
        let result = svc.ingest_metrics(vec![point]).await;
        let elapsed = started.elapsed();

        assert!(matches!(
            result,
            Err(OtelError::Storage {
                kind: StorageErrorKind::ClickHouseSchema,
                ..
            })
        ));
        assert_eq!(
            storage.store_metrics_call_count(),
            1,
            "a terminal error must not be retried"
        );
        assert_eq!(
            elapsed,
            Duration::ZERO,
            "no backoff may be incurred for a terminal error"
        );

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_dropped, 1);
        assert_eq!(stats.ingest_errors, 1);
    }

    /// A sustained outage still surfaces as an error after the bounded retry
    /// is exhausted, and the counters must reflect one *ingest* failure — not
    /// one per attempt — with the whole batch counted as dropped exactly
    /// once. The failure reason must also be durable, mirroring the spans
    /// path's `test_exhausted_retries_record_the_failure_reason`.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_metrics_exhausts_retries_records_failure() {
        let mock = MockOtelStorage::new();
        mock.fail_store_metrics_transiently("connection reset by peer", None);
        let (svc, storage) = make_service(mock);

        let points = vec![
            test_support::metric_point(1, "http.server.duration", chrono::Utc::now(), &[]),
            test_support::metric_point(1, "http.server.duration", chrono::Utc::now(), &[]),
        ];

        let result = svc.ingest_metrics(points).await;

        assert!(matches!(
            result,
            Err(OtelError::Storage {
                kind: StorageErrorKind::ClickHouseNetwork,
                ..
            })
        ));
        assert_eq!(
            storage.store_metrics_call_count(),
            STORAGE_WRITE_MAX_ATTEMPTS,
            "expected the full bounded attempt budget"
        );

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_received, 2);
        assert_eq!(stats.metrics_stored, 0);
        assert_eq!(
            stats.metrics_dropped, 2,
            "the batch is dropped once, not once per attempt"
        );
        assert_eq!(
            stats.ingest_errors, 1,
            "3 attempts are still a single ingest failure"
        );

        let recorded = svc.recent_ingest_errors(20).await.unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].signal_type, "metrics");
        assert_eq!(recorded[0].error_class, "clickhouse_network");
    }

    // ── Logs ingest: the DB write is non-fatal to the overall result ────

    /// `ingest_logs` deliberately diverges from `ingest_spans`/`ingest_metrics`:
    /// even a *terminal* DB failure must not fail the call, because the S3
    /// archive path is attempted regardless — see the doc comment on
    /// `OtelService::ingest_logs`. The DB failure still counts as a drop and
    /// records an ingest-error group, but the method itself returns
    /// `Ok(count)`.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_logs_db_failure_is_non_fatal() {
        let mock = MockOtelStorage::new();
        mock.fail_store_logs_fatally("schema mismatch: column `severity` is missing");
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let make_log = |severity: LogSeverity, body: &str| LogRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            timestamp: now,
            observed_timestamp: now,
            severity,
            severity_text: severity.to_string(),
            body: body.to_string(),
            trace_id: None,
            span_id: None,
            attributes: Default::default(),
        };
        let logs = vec![
            make_log(LogSeverity::Error, "db is down"),
            make_log(LogSeverity::Info, "still archiving"),
        ];
        let count = logs.len() as u64;

        let result = svc.ingest_logs(logs).await;

        assert_eq!(
            result.unwrap(),
            count,
            "ingest_logs must return Ok(count) even when the DB write fails terminally"
        );
        assert_eq!(
            storage.store_logs_call_count(),
            1,
            "a terminal DB error must not be retried"
        );

        let stats = svc.pipeline_stats();
        assert_eq!(stats.logs_received, count);
        assert_eq!(
            stats.logs_dropped, count,
            "the DB write failure must count the whole batch as dropped"
        );
        assert_eq!(stats.ingest_errors, 1);
        assert_eq!(
            stats.logs_stored_s3, count,
            "the S3 archive path must still run despite the DB failure"
        );

        let recorded = svc.recent_ingest_errors(20).await.unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].signal_type, "logs");
    }

    // ── Mock ingest-error window filter matches the real backend ────────

    /// `MockOtelStorage::recent_ingest_errors` must apply the same 7-day
    /// window filter the real TimescaleDB backend does (see
    /// `timescaledb_storage_test.rs::test_recent_ingest_errors_excludes_entries_older_than_the_window`),
    /// or a unit test relying on the mock would get a false-green on window
    /// filtering.
    #[tokio::test]
    async fn test_mock_recent_ingest_errors_filters_stale_groups() {
        let mock = MockOtelStorage::new();
        let now = chrono::Utc::now();
        mock.ingest_errors.lock().unwrap().push(IngestErrorSummary {
            signal_type: "spans".into(),
            error_class: "clickhouse_network".into(),
            sample_message: "recent".into(),
            count: 1,
            first_seen: now,
            last_seen: now,
        });
        mock.ingest_errors.lock().unwrap().push(IngestErrorSummary {
            signal_type: "logs".into(),
            error_class: "postgres_conn".into(),
            sample_message: "stale".into(),
            count: 1,
            first_seen: now - chrono::Duration::days(8),
            last_seen: now - chrono::Duration::days(8),
        });

        let recorded = mock.recent_ingest_errors(50).await.unwrap();

        assert!(
            recorded.iter().any(|e| e.signal_type == "spans"),
            "a group within the 7-day window must still be returned"
        );
        assert!(
            !recorded.iter().any(|e| e.signal_type == "logs"),
            "a group whose last_seen is 8 days old must be filtered out by the mock too, \
             got {recorded:?}"
        );
    }

    // ── Duplicate-write safety (Greptile P1) ────────────────────────────

    /// The regression guard for silent duplicate rows.
    ///
    /// `otel_spans`/`otel_metrics`/`otel_log_events` have no unique key, so a
    /// retry after a *possibly-committed* write inserts the batch twice with
    /// no error. `DbErr::Conn` is exactly that case — the connection died at
    /// an unknown point — so the retry loop must call storage once and stop.
    #[tokio::test(start_paused = true)]
    async fn test_postgres_conn_failure_is_not_retried() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_with(
            "Connection Error: connection reset by peer",
            StorageErrorKind::PostgresConn,
            None,
        );
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        let result = svc.ingest_spans(spans).await;

        assert!(result.is_err());
        assert_eq!(
            storage.store_spans_call_count(),
            1,
            "a connection failure with an unknown outcome must not be re-sent — \
             the batch insert has no unique key, so a retry would duplicate it"
        );
    }

    /// The counterpart: the pool never handed out a connection, so nothing was
    /// transmitted and re-sending is provably safe. This must still heal.
    #[tokio::test(start_paused = true)]
    async fn test_postgres_pool_acquire_failure_is_retried() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_with(
            "Failed to acquire connection from pool: Connection pool timed out",
            StorageErrorKind::PostgresConnAcquire,
            Some(1),
        );
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        let stored = svc
            .ingest_spans(spans)
            .await
            .expect("a pool-acquire failure never reached the server, so retry is safe");

        assert_eq!(stored, 1);
        assert_eq!(storage.store_spans_call_count(), 2);
        assert_eq!(svc.pipeline_stats().spans_dropped, 0);
    }

    /// ClickHouse writes go to `ReplacingMergeTree`, which converges duplicate
    /// rows by design — so transport errors there keep their full retry
    /// coverage. Pinning this stops a future "make retries safer" change from
    /// pessimising the idempotent backend along with the non-idempotent one.
    #[tokio::test(start_paused = true)]
    async fn test_clickhouse_transport_failures_keep_retrying() {
        for kind in [
            StorageErrorKind::ClickHouseNetwork,
            StorageErrorKind::ClickHouseTimeout,
            StorageErrorKind::ClickHouseOther,
        ] {
            let mock = MockOtelStorage::new();
            mock.fail_store_spans_with("transient", kind, Some(1));
            let (svc, storage) = make_service(mock);

            let spans =
                decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
            assert!(
                svc.ingest_spans(spans).await.is_ok(),
                "{} must still be retried",
                kind.as_class()
            );
            assert_eq!(storage.store_spans_call_count(), 2, "{}", kind.as_class());
        }
    }

    // ── Ingest error recording ──────────────────────────────────────────

    /// The whole point of Phase 2: after retries are exhausted, the *reason*
    /// must be durable, not just the counter.
    #[tokio::test(start_paused = true)]
    async fn test_exhausted_retries_record_the_failure_reason() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", None);
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        assert!(svc.ingest_spans(spans).await.is_err());

        let recorded = svc.recent_ingest_errors(20).await.unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].signal_type, "spans");
        assert_eq!(recorded[0].error_class, "clickhouse_network");
        assert_eq!(recorded[0].count, 1);
        assert!(
            recorded[0]
                .sample_message
                .contains("connection reset by peer"),
            "sample must carry the backend detail: {:?}",
            recorded[0].sample_message
        );
        assert_eq!(storage.recorded_ingest_errors().len(), 1);
    }

    /// A batch that succeeds — even after a transient blip — must record
    /// nothing, or the report fills with noise that was already recovered.
    #[tokio::test(start_paused = true)]
    async fn test_recovered_blip_records_no_failure() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", Some(1));
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        assert!(svc.ingest_spans(spans).await.is_ok());

        assert!(
            storage.recorded_ingest_errors().is_empty(),
            "a healed retry is not an ingest failure"
        );
    }

    /// A terminal failure is recorded under its own class, so a schema
    /// mismatch is not misread as an outage.
    #[tokio::test(start_paused = true)]
    async fn test_fatal_failure_records_its_own_class() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_fatally("schema mismatch: column `duration_ms` is missing");
        let (svc, _storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        assert!(svc.ingest_spans(spans).await.is_err());

        let recorded = svc.recent_ingest_errors(20).await.unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].error_class, "clickhouse_schema");
    }

    /// Repeated failures of the same kind must aggregate into one group with a
    /// rising count, not one row per dropped batch.
    #[tokio::test(start_paused = true)]
    async fn test_repeated_failures_aggregate_into_one_group() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", None);
        let (svc, _storage) = make_service(mock);

        for _ in 0..3 {
            let spans =
                decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
            assert!(svc.ingest_spans(spans).await.is_err());
        }

        let recorded = svc.recent_ingest_errors(20).await.unwrap();
        assert_eq!(recorded.len(), 1, "same (signal, class) must be one group");
        assert_eq!(recorded[0].count, 3);
    }

    /// **The critical non-fatal contract.** Recording runs on an
    /// already-failing path, and the backend it writes to may be the thing
    /// that is down. A failure to record must not change the ingest result or
    /// any counter.
    #[tokio::test(start_paused = true)]
    async fn test_failure_to_record_is_non_fatal() {
        let mock = MockOtelStorage::new();
        mock.fail_store_spans_transiently("connection reset by peer", None);
        *mock.fail_record_ingest_error.lock().unwrap() =
            Some("could not acquire a Postgres connection".into());
        let (svc, storage) = make_service(mock);

        let spans = decode::decode_traces_request(&single_error_span_payload(), 1, None).unwrap();
        let result = svc.ingest_spans(spans).await;

        // The ingest error still surfaces unchanged — the bookkeeping failure
        // is swallowed, never substituted for the real cause.
        assert!(matches!(
            result,
            Err(OtelError::Storage {
                kind: StorageErrorKind::ClickHouseNetwork,
                ..
            })
        ));
        assert!(storage.recorded_ingest_errors().is_empty());

        // Counters are untouched by the recording failure.
        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_dropped, 1);
        assert_eq!(stats.ingest_errors, 1);
    }

    /// Each signal records under its own `signal_type`, so "logs are failing"
    /// is distinguishable from "spans are failing".
    #[tokio::test(start_paused = true)]
    async fn test_signal_types_are_recorded_separately() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Drive the recorder directly for two signals — the per-signal wiring
        // in ingest_* is covered by the span tests above.
        let err = OtelError::Storage {
            message: "timeout expired".into(),
            kind: StorageErrorKind::ClickHouseTimeout,
        };
        svc.record_ingest_failure("spans", &err).await;
        svc.record_ingest_failure("metrics", &err).await;
        svc.record_ingest_failure("logs", &err).await;

        let recorded = storage.recorded_ingest_errors();
        assert_eq!(recorded.len(), 3);
        let signals: std::collections::HashSet<&str> =
            recorded.iter().map(|e| e.signal_type.as_str()).collect();
        assert_eq!(
            signals,
            std::collections::HashSet::from(["spans", "metrics", "logs"])
        );
        assert!(recorded
            .iter()
            .all(|e| e.error_class == "clickhouse_timeout"));
    }

    /// The service must not hand the storage layer an unclamped limit.
    #[tokio::test]
    async fn test_recent_ingest_errors_limit_is_clamped() {
        let mock = MockOtelStorage::new();
        let (svc, _storage) = make_service(mock);

        let err = OtelError::Storage {
            message: "boom".into(),
            kind: StorageErrorKind::ClickHouseOther,
        };
        for signal in ["spans", "metrics", "logs"] {
            svc.record_ingest_failure(signal, &err).await;
        }

        // 0 means "unspecified" → default page, not an empty result.
        assert_eq!(svc.recent_ingest_errors(0).await.unwrap().len(), 3);
        // Oversized limits are capped rather than rejected.
        assert_eq!(svc.recent_ingest_errors(u32::MAX).await.unwrap().len(), 3);
        // An explicit small limit is honoured.
        assert_eq!(svc.recent_ingest_errors(2).await.unwrap().len(), 2);
    }

    /// Worst-case added latency must stay well under a second, since the
    /// ingest permit is held for the whole retry sequence.
    #[test]
    fn test_storage_retry_backoff_is_bounded() {
        let config = storage_retry_config();
        assert_eq!(config.max_attempts, STORAGE_WRITE_MAX_ATTEMPTS);

        let total: Duration = (0..config.max_attempts.saturating_sub(1))
            .map(|attempt| config.compute_delay(attempt))
            .sum();
        assert_eq!(total, Duration::from_millis(225));
        assert!(
            total < Duration::from_secs(1),
            "retry budget {total:?} must stay under 1s to avoid amplifying IngestSaturated"
        );
        for attempt in 0..config.max_attempts {
            assert!(config.compute_delay(attempt) <= STORAGE_WRITE_MAX_DELAY);
        }
    }

    /// `store_with_retry` must not sleep at all when the first attempt works.
    #[tokio::test(start_paused = true)]
    async fn test_store_with_retry_succeeds_without_backoff() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();

        let started = tokio::time::Instant::now();
        let result = store_with_retry("spans", || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(7)
            }
        })
        .await;

        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    /// Transient failures must back off between attempts, and the total wait
    /// must match the configured budget exactly.
    #[tokio::test(start_paused = true)]
    async fn test_store_with_retry_waits_configured_backoff_before_giving_up() {
        let started = tokio::time::Instant::now();
        let result = store_with_retry("logs", || async {
            Err(OtelError::Storage {
                message: "timeout expired".into(),
                kind: StorageErrorKind::ClickHouseTimeout,
            })
        })
        .await;

        assert!(result.is_err());
        assert_eq!(started.elapsed(), Duration::from_millis(225));
    }

    /// A transient `Database` error is retried too, not just `Storage` — the
    /// TimescaleDB backend surfaces connection failures as `DbErr`.
    #[tokio::test(start_paused = true)]
    async fn test_store_with_retry_retries_transient_database_error() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();

        let result = store_with_retry("metrics", || {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(OtelError::Database(sea_orm::DbErr::ConnectionAcquire(
                        sea_orm::ConnAcquireErr::Timeout,
                    )))
                } else {
                    Ok(3)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Metrics and logs go through the same helper; assert the metrics path
    /// heals a blip rather than counting the batch as dropped.
    #[tokio::test(start_paused = true)]
    async fn test_ingest_metrics_and_logs_use_the_same_bounded_retry() {
        // Metrics: the mock never fails, so this pins the no-retry-needed
        // path end-to-end through `ingest_metrics`.
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let point = test_support::metric_point(1, "http.server.duration", chrono::Utc::now(), &[]);
        let stored = svc.ingest_metrics(vec![point]).await.unwrap();
        assert_eq!(stored, 1);
        assert_eq!(storage.stored_metrics().len(), 1);

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_stored, 1);
        assert_eq!(stats.metrics_dropped, 0);
    }

    #[tokio::test]
    async fn failed_local_ingest_never_offers_spans_to_the_cloud_mirror() {
        let mock = MockOtelStorage::new();
        *mock.fail_store_spans.lock().unwrap() = Some("disk full".into());
        let (svc, _) = make_service(mock);
        let (_directory, link) = linked_cloud();
        let svc = svc.with_cloud_link(link.clone());
        let (_, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();

        assert!(svc.ingest_spans(spans).await.is_err());

        assert_eq!(link.spooled(), 0, "local storage must succeed first");
    }

    #[tokio::test]
    async fn test_ingest_and_query_trace_tree_roundtrip() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Build and ingest the sample trace tree
        let (trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 42, None).unwrap();

        // Force all spans through by marking one as error to ensure the trace is kept
        // Actually, let's just store directly to mock to test the query path
        storage.spans.lock().unwrap().extend(spans.clone());

        // Query the trace back
        let queried = svc.get_trace(42, &trace_id_hex).await.unwrap();
        assert_eq!(queried.len(), 4, "Should retrieve all 4 spans");

        // Verify tree structure
        let roots = test_support::find_roots(&queried);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "GET /api/users");

        let tree = test_support::build_tree(&queried);
        let root_children = tree.get(&roots[0].span_id).unwrap();
        assert_eq!(root_children.len(), 2, "Root should have 2 direct children");

        // Verify we can find the grandchild
        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            queried.iter().map(|s| (s.name.as_str(), s)).collect();
        let http_child = by_name["POST /external/validate"];
        let grandchildren = tree.get(&http_child.span_id).unwrap();
        assert_eq!(grandchildren.len(), 1);
    }

    #[tokio::test]
    async fn test_query_spans_filter_by_status() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Insert spans with mixed statuses
        let ok_span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "aaa".into(),
            span_id: "001".into(),
            parent_span_id: None,
            name: "ok-op".into(),
            kind: SpanKind::Server,
            start_time: chrono::Utc::now(),
            end_time: chrono::Utc::now(),
            duration_ms: 10.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: Default::default(),
            events: vec![],
        };

        let err_span = SpanRecord {
            status_code: SpanStatusCode::Error,
            span_id: "002".into(),
            name: "err-op".into(),
            ..ok_span.clone()
        };

        storage
            .spans
            .lock()
            .unwrap()
            .extend(vec![ok_span, err_span]);

        // Query only errors
        let results = svc
            .query_spans(TraceQuery {
                project_id: 1,
                status: Some(SpanStatusCode::Error),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "err-op");
    }

    #[tokio::test]
    async fn test_ingest_logs_stores_all_severities() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let make_log = |severity: LogSeverity, body: &str| LogRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            timestamp: now,
            observed_timestamp: now,
            severity,
            severity_text: severity.to_string(),
            body: body.to_string(),
            trace_id: None,
            span_id: None,
            attributes: Default::default(),
        };

        let logs = vec![
            make_log(LogSeverity::Debug, "debug msg"),
            make_log(LogSeverity::Info, "info msg"),
            make_log(LogSeverity::Warn, "warn msg"),
            make_log(LogSeverity::Error, "error msg"),
            make_log(LogSeverity::Fatal, "fatal msg"),
        ];

        svc.ingest_logs(logs).await.unwrap();

        // DB should now hold ALL severities (queryable), not just WARN+.
        let db_logs = storage.stored_logs();
        assert_eq!(db_logs.len(), 5, "All severities should go to the DB");
        assert!(db_logs.iter().any(|l| l.severity == LogSeverity::Info));
        assert!(db_logs.iter().any(|l| l.severity == LogSeverity::Debug));

        // S3 archive should still have all 5.
        let archived = storage.stored_archived_logs();
        assert_eq!(archived.len(), 5, "All logs should be archived to S3");

        let stats = svc.pipeline_stats();
        assert_eq!(stats.logs_received, 5);
        assert_eq!(stats.logs_stored_db, 5);
        assert_eq!(stats.logs_stored_s3, 5);
    }

    #[tokio::test]
    async fn test_ingest_metrics_roundtrip() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let mut point = MetricPoint::skeleton(
            1,
            None,
            ResourceInfo::default(),
            "http.duration".into(),
            MetricType::Gauge,
            "ms".into(),
            chrono::Utc::now(),
            Default::default(),
        );
        point.value = Some(42.5);

        let stored = svc.ingest_metrics(vec![point]).await.unwrap();
        assert_eq!(stored, 1);

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_received, 1);
        assert_eq!(stats.metrics_stored, 1);

        let stored_metrics = storage.stored_metrics();
        assert_eq!(stored_metrics.len(), 1);
        assert_eq!(stored_metrics[0].metric_name, "http.duration");
    }

    #[test]
    fn test_check_rate_limit_allows_within_limit() {
        let mock = MockOtelStorage::new();
        let (svc, _) = make_service(mock);

        // Should succeed
        assert!(svc.check_rate_limit(1).is_ok());
    }

    #[test]
    fn test_check_rate_limit_rejects_over_limit() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let limiter = Arc::new(RateLimiter::new(2, Duration::from_secs(60))); // only 2 allowed
        let storage = Arc::new(MockOtelStorage::new()) as Arc<dyn OtelStorage>;
        let svc = OtelService::new(
            storage,
            auth,
            limiter,
            DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
        );

        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_ok());
        assert_eq!(svc.pipeline_stats().rate_limited_requests, 0);
        let result = svc.check_rate_limit(1);
        // The error must report the limiter's actual configured limit (2),
        // not a hardcoded value.
        assert!(matches!(
            result,
            Err(OtelError::RateLimitExceeded { limit: 2, .. })
        ));
        // The rejection counter the pipeline-stats sampler reads must move —
        // this is what makes the rejection observable at all (dashboard,
        // metrics store, alarm). A regression here silently blinds both.
        assert_eq!(svc.pipeline_stats().rate_limited_requests, 1);

        // A second rejection accumulates rather than resetting.
        assert!(svc.check_rate_limit(1).is_err());
        assert_eq!(svc.pipeline_stats().rate_limited_requests, 2);
    }

    // ── service (`si_`) ingest rate limit (SECURITY metrics-security-2) ────────

    #[test]
    fn test_check_service_rate_limit_allows_within_limit() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Well within the generous SERVICE_INGEST_MAX_REQUESTS ceiling.
        for _ in 0..50 {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
    }

    #[test]
    fn test_check_service_rate_limit_rejects_over_limit() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Exhaust the per-service window, then expect a service-scoped 429.
        for _ in 0..SERVICE_INGEST_MAX_REQUESTS {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
        let result = svc.check_service_rate_limit(7);
        assert!(
            matches!(
                result,
                Err(OtelError::ServiceRateLimitExceeded {
                    service_id: 7,
                    limit
                }) if limit == SERVICE_INGEST_MAX_REQUESTS
            ),
            "expected ServiceRateLimitExceeded for service 7, got {result:?}"
        );
    }

    #[test]
    fn test_service_rate_limit_is_isolated_per_service() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Exhaust service 7's window.
        for _ in 0..SERVICE_INGEST_MAX_REQUESTS {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
        assert!(svc.check_service_rate_limit(7).is_err());
        // A different service must be unaffected.
        assert!(svc.check_service_rate_limit(8).is_ok());
    }

    #[test]
    fn test_service_rate_limit_does_not_consume_project_limit() {
        // The service limiter and the project limiter are separate instances:
        // hammering one must not reject the other.
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let project_limiter = Arc::new(RateLimiter::new(2, Duration::from_secs(60)));
        let storage = Arc::new(MockOtelStorage::new()) as Arc<dyn OtelStorage>;
        let svc = OtelService::new(
            storage,
            auth,
            project_limiter,
            DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS,
        );

        // Spend many service-ingest tokens for service 1...
        for _ in 0..10 {
            assert!(svc.check_service_rate_limit(1).is_ok());
        }
        // ...the project limiter (limit 2) for project 1 is untouched.
        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_err());
    }

    // ── quota check caching (avoid per-request COUNT(*) under load) ────────

    #[tokio::test]
    async fn test_check_quota_reuses_cached_result_within_ttl() {
        let (svc, storage) = make_service(MockOtelStorage::new());

        for _ in 0..5 {
            assert!(svc.check_quota(1).await.is_ok());
        }

        // 5 checks for the same project should only hit storage once — the
        // rest are served from the quota cache.
        assert_eq!(storage.get_storage_quota_call_count(), 1);
    }

    #[tokio::test]
    async fn test_check_quota_is_cached_per_project() {
        let (svc, storage) = make_service(MockOtelStorage::new());

        assert!(svc.check_quota(1).await.is_ok());
        assert!(svc.check_quota(2).await.is_ok());
        assert!(svc.check_quota(1).await.is_ok());
        assert!(svc.check_quota(2).await.is_ok());

        // Each distinct project gets its own cache entry, so two projects
        // still means exactly two storage calls, not four.
        assert_eq!(storage.get_storage_quota_call_count(), 2);
    }

    #[tokio::test]
    async fn test_check_quota_rejects_when_over_limit() {
        let mock = MockOtelStorage::new();
        *mock.quota_override.lock().unwrap() = Some(StorageQuota {
            project_id: 42,
            metrics_bytes: 0,
            traces_bytes: 0,
            logs_bytes: 0,
            total_bytes: 200,
            limit_bytes: 100,
            usage_pct: 200.0,
        });
        let (svc, _storage) = make_service(mock);

        assert_eq!(svc.pipeline_stats().quota_exceeded_requests, 0);
        let result = svc.check_quota(42).await;
        assert!(matches!(
            result,
            Err(OtelError::QuotaExceeded {
                project_id: 42,
                used_bytes: 200,
                limit_bytes: 100,
            })
        ));
        // Same counter contract as the rate-limit rejection path above: the
        // pipeline-stats sampler and the settings-page UI both read this
        // field, so a quota rejection that doesn't increment it is invisible
        // to an operator even though requests are actually being dropped.
        assert_eq!(svc.pipeline_stats().quota_exceeded_requests, 1);
    }

    #[tokio::test]
    async fn test_query_genai_traces_filters_by_gen_ai_system() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut genai_attrs = std::collections::BTreeMap::new();
        genai_attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        genai_attrs.insert("gen_ai.request.model".to_string(), "gpt-4".to_string());
        genai_attrs.insert("gen_ai.usage.input_tokens".to_string(), "100".to_string());

        let genai_span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "genai-trace-1".into(),
            span_id: "span-1".into(),
            parent_span_id: None,
            name: "chat".into(),
            kind: SpanKind::Client,
            start_time: now,
            end_time: now,
            duration_ms: 500.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: genai_attrs,
            events: vec![],
        };

        let normal_span = SpanRecord {
            trace_id: "normal-trace".into(),
            span_id: "span-2".into(),
            name: "GET /api".into(),
            attributes: std::collections::BTreeMap::new(),
            ..genai_span.clone()
        };

        storage
            .spans
            .lock()
            .unwrap()
            .extend(vec![genai_span, normal_span]);

        let summaries = svc
            .query_genai_trace_summaries(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].trace_id, "genai-trace-1");
        assert_eq!(summaries[0].gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(summaries[0].gen_ai_model.as_deref(), Some("gpt-4"));
    }

    #[tokio::test]
    async fn test_get_genai_trace_spans() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert("gen_ai.system".to_string(), "anthropic".to_string());
        attrs.insert(
            "gen_ai.request.model".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );
        attrs.insert("gen_ai.operation.name".to_string(), "chat".to_string());
        attrs.insert("gen_ai.usage.input_tokens".to_string(), "50".to_string());
        attrs.insert("gen_ai.usage.output_tokens".to_string(), "200".to_string());

        let span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "trace-abc".into(),
            span_id: "span-1".into(),
            parent_span_id: None,
            name: "chat".into(),
            kind: SpanKind::Client,
            start_time: now,
            end_time: now,
            duration_ms: 1200.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: attrs,
            events: vec![],
        };

        storage.spans.lock().unwrap().push(span);

        let details = svc.get_genai_trace_spans(1, "trace-abc").await.unwrap();

        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(
            details[0].gen_ai_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(details[0].gen_ai_operation.as_deref(), Some("chat"));
        assert_eq!(details[0].input_tokens, Some(50));
        assert_eq!(details[0].output_tokens, Some(200));
    }

    #[tokio::test]
    async fn test_genai_handles_deprecated_provider_name() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();

        // Use the new gen_ai.provider.name attribute (not deprecated gen_ai.system)
        let mut new_attrs = std::collections::BTreeMap::new();
        new_attrs.insert("gen_ai.provider.name".to_string(), "anthropic".to_string());
        new_attrs.insert(
            "gen_ai.request.model".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );
        new_attrs.insert("gen_ai.usage.input_tokens".to_string(), "100".to_string());

        // Use deprecated gen_ai.usage.prompt_tokens and gen_ai.system
        let mut old_attrs = std::collections::BTreeMap::new();
        old_attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        old_attrs.insert("gen_ai.request.model".to_string(), "gpt-4".to_string());
        old_attrs.insert("gen_ai.usage.prompt_tokens".to_string(), "50".to_string());
        old_attrs.insert(
            "gen_ai.usage.completion_tokens".to_string(),
            "150".to_string(),
        );

        for (tid, attrs) in [("new-trace", new_attrs), ("old-trace", old_attrs)] {
            storage.spans.lock().unwrap().push(SpanRecord {
                project_id: 1,
                deployment_id: None,
                resource: ResourceInfo::default(),
                trace_id: tid.into(),
                span_id: format!("{}-s1", tid),
                parent_span_id: None,
                name: "chat".into(),
                kind: SpanKind::Client,
                start_time: now,
                end_time: now,
                duration_ms: 100.0,
                status_code: SpanStatusCode::Ok,
                status_message: String::new(),
                attributes: attrs,
                events: vec![],
            });
        }

        // Both should be found
        let count = svc
            .count_genai_traces(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(count, 2);

        // Get details for the span with deprecated attributes
        let details = svc.get_genai_trace_spans(1, "old-trace").await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(details[0].input_tokens, Some(50));
        assert_eq!(details[0].output_tokens, Some(150));

        // Get details for the span with new attributes
        let details = svc.get_genai_trace_spans(1, "new-trace").await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(details[0].input_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_count_genai_traces() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut genai_attrs = std::collections::BTreeMap::new();
        genai_attrs.insert("gen_ai.system".to_string(), "openai".to_string());

        // Two GenAI traces, one normal trace
        for (tid, has_genai) in [("t1", true), ("t2", true), ("t3", false)] {
            let attrs = if has_genai {
                genai_attrs.clone()
            } else {
                std::collections::BTreeMap::new()
            };
            storage.spans.lock().unwrap().push(SpanRecord {
                project_id: 1,
                deployment_id: None,
                resource: ResourceInfo::default(),
                trace_id: tid.into(),
                span_id: format!("{}-s1", tid),
                parent_span_id: None,
                name: "op".into(),
                kind: SpanKind::Client,
                start_time: now,
                end_time: now,
                duration_ms: 100.0,
                status_code: SpanStatusCode::Ok,
                status_message: String::new(),
                attributes: attrs,
                events: vec![],
            });
        }

        let count = svc
            .count_genai_traces(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_list_metric_label_keys_and_values_in_window() {
        let mock = MockOtelStorage::new();
        let now = chrono::Utc::now();
        {
            let mut m = mock.metrics.lock().unwrap();
            m.push(test_support::metric_point(
                1,
                "http.server.duration",
                now,
                &[("http.method", "GET"), ("http.route", "/a")],
            ));
            m.push(test_support::metric_point(
                1,
                "http.server.duration",
                now,
                &[("http.method", "POST"), ("http.route", "/b")],
            ));
            // Different metric — must not leak into the results.
            m.push(test_support::metric_point(
                1,
                "other.metric",
                now,
                &[("region", "eu")],
            ));
        }
        let (svc, _storage) = make_service(mock);
        let start = now - chrono::Duration::hours(1);
        let end = now + chrono::Duration::hours(1);

        let keys = svc
            .list_metric_label_keys(1, "http.server.duration", start, end)
            .await
            .unwrap();
        assert_eq!(
            keys,
            vec!["http.method".to_string(), "http.route".to_string()]
        );

        let values = svc
            .list_metric_label_values(1, "http.server.duration", "http.method", start, end)
            .await
            .unwrap();
        assert_eq!(values, vec!["GET".to_string(), "POST".to_string()]);
    }

    #[tokio::test]
    async fn test_list_metric_label_values_rejects_invalid_key() {
        let (svc, _storage) = make_service(MockOtelStorage::new());
        let now = chrono::Utc::now();
        let err = svc
            .list_metric_label_values(1, "m", "bad key!", now - chrono::Duration::hours(1), now)
            .await
            .unwrap_err();
        assert!(matches!(err, OtelError::Validation { .. }));
    }

    #[test]
    fn ingest_concurrency_is_bounded_and_permits_are_released() {
        let (service, _storage) = make_service(MockOtelStorage::new());
        let permits: Vec<_> = (0..DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS)
            .map(|_| service.try_acquire_ingest_permit().unwrap())
            .collect();

        assert!(matches!(
            service.try_acquire_ingest_permit(),
            Err(OtelError::IngestSaturated {
                limit: DEFAULT_MAX_CONCURRENT_INGEST_REQUESTS
            })
        ));

        drop(permits);
        assert!(service.try_acquire_ingest_permit().is_ok());
    }

    // ── span-stats query validation ─────────────────────────────────

    fn span_stats_query(project_ids: Vec<i32>, window_days: i64) -> SpanStatsQuery {
        let end = chrono::Utc::now();
        SpanStatsQuery {
            project_ids,
            start_time: end - chrono::Duration::days(window_days),
            end_time: end,
            service_name: None,
            span_name: None,
            name_pattern: None,
            kind: None,
            status: None,
            environment_id: None,
            deployment_id: None,
            attributes: None,
            min_duration_ms: None,
            min_count: 1,
            sort_by: SpanStatsSortField::default(),
            sort_order: SortOrder::default(),
            limit: None,
            offset: None,
        }
    }

    #[test]
    fn span_stats_rejects_more_projects_than_the_cap() {
        // The handler checks a project's access one round-trip at a time, and
        // an instance admin skips those checks entirely — so the id list has to
        // be bounded before it reaches storage, not just before it reaches auth.
        let too_many: Vec<i32> = (1..=(SPAN_STATS_MAX_PROJECTS as i32 + 1)).collect();
        let err = OtelService::validate_span_stats_query(&span_stats_query(too_many, 1))
            .expect_err("over-cap project list must be rejected");
        assert!(matches!(err, OtelError::Validation { .. }), "got {err:?}");
        assert!(err.to_string().contains("at most"), "got {err}");

        let at_cap: Vec<i32> = (1..=SPAN_STATS_MAX_PROJECTS as i32).collect();
        assert!(OtelService::validate_span_stats_query(&span_stats_query(at_cap, 1)).is_ok());
    }

    #[test]
    fn span_stats_rejects_a_window_wider_than_the_cap() {
        // This report aggregates the whole window before it can rank anything,
        // so an unbounded window is an unbounded query on a small box.
        let err = OtelService::validate_span_stats_query(&span_stats_query(
            vec![1],
            SPAN_STATS_MAX_WINDOW_DAYS + 1,
        ))
        .expect_err("over-wide window must be rejected");
        assert!(matches!(err, OtelError::Validation { .. }), "got {err:?}");
        assert!(err.to_string().contains("exceeds"), "got {err}");

        // Rejected, never silently narrowed: a caller who asked for 90 days and
        // got 31 back would read the numbers as covering 90.
        assert!(OtelService::validate_span_stats_query(&span_stats_query(
            vec![1],
            SPAN_STATS_MAX_WINDOW_DAYS
        ))
        .is_ok());
    }

    #[test]
    fn span_stats_still_rejects_empty_and_inverted_windows() {
        assert!(OtelService::validate_span_stats_query(&span_stats_query(vec![], 1)).is_err());

        let now = chrono::Utc::now();
        let inverted = SpanStatsQuery {
            start_time: now,
            end_time: now - chrono::Duration::hours(1),
            ..span_stats_query(vec![1], 1)
        };
        assert!(OtelService::validate_span_stats_query(&inverted).is_err());
    }
}
