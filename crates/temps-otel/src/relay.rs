// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extension point for observing or relaying decoded OTLP ingest batches.
//!
//! OSS registers [`NoopOtelRelay`] at startup, which discards every message.
//! A plugin can supply an alternative implementation — e.g. one that enqueues
//! decoded OTLP batches for delivery to an external OTLP-compatible collector —
//! by registering an `Arc<dyn OtelRelay>` via the service registry before the
//! OTel plugin wires up its background consumer in `initialize_plugin_services`.
//!
//! [`OtelRelaySlot`] exists because `register_services` runs in
//! plugin-registration order: the relay background consumer is started (and
//! captures its reference to the slot) before a later-registered plugin gets
//! a chance to provide an implementation — same two-phase handoff that
//! [`temps_core::RetentionResolverSlot`] and `DeploymentGate` use.

use std::sync::Arc;

use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

/// Maximum number of batches waiting in the process-wide relay handoff.
///
/// The consumer only dispatches into a plugin-owned queue, so a large message
/// backlog here adds memory without improving destination outage tolerance.
pub const RELAY_QUEUE_MAX_BATCHES: usize = 128;

/// Maximum decompressed OTLP payload bytes retained by the process-wide relay
/// handoff. This is independent of the queue's batch-count bound so a stream of
/// maximum-size requests cannot consume the count limit times request limit.
pub const RELAY_QUEUE_MAX_BYTES: usize = crate::memory::MAX_RELAY_QUEUE_BYTES;

/// Which OTLP signal type a relay batch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelSignal {
    /// OpenTelemetry Metrics (OTLP ExportMetricsServiceRequest).
    Metrics,
    /// OpenTelemetry Traces (OTLP ExportTraceServiceRequest).
    Traces,
    /// OpenTelemetry Logs (OTLP ExportLogsServiceRequest).
    Logs,
}

/// A single decoded OTLP ingest batch delivered to an [`OtelRelay`].
///
/// `payload` holds the **decompressed** OTLP protobuf bytes exactly as they
/// arrived after decompression, before any conversion to this crate's domain
/// types. A relaying plugin can therefore re-POST the bytes verbatim to any
/// OTLP/HTTP-compatible collector endpoint without lossy round-tripping through
/// this crate's internal representation. The `bytes::Bytes` clone produced on
/// the ingest path is O(1) — a refcount increment, not a data copy.
pub struct OtelRelayMessage {
    /// Project that owns this batch of telemetry.
    pub project_id: i32,
    /// Environment the telemetry was ingested for, when known.
    pub environment_id: Option<i32>,
    /// Deployment the telemetry was ingested for, when known.
    pub deployment_id: Option<i32>,
    /// OTLP signal type (Metrics / Traces / Logs).
    pub signal: OtelSignal,
    /// Number of signal items in the batch (spans, log records, or metric
    /// points). Relays use this for drop and throughput accounting without
    /// decoding the protobuf a second time.
    pub item_count: usize,
    /// Decompressed OTLP protobuf bytes (see module-level note).
    pub payload: bytes::Bytes,
}

/// Why a non-blocking enqueue into the process-wide relay handoff failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelRelayEnqueueError {
    /// The batch-count limit was reached.
    BatchCapacity,
    /// Retaining this payload would exceed the byte budget.
    ByteCapacity,
    /// The background relay consumer has exited.
    Closed,
}

impl OtelRelayEnqueueError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BatchCapacity => "batch_capacity",
            Self::ByteCapacity => "byte_capacity",
            Self::Closed => "closed",
        }
    }
}

struct QueuedOtelRelayMessage {
    message: OtelRelayMessage,
    _byte_permit: OwnedSemaphorePermit,
}

/// Non-blocking, byte-aware sender used by OTLP ingest handlers.
///
/// A byte permit lives with each queued message and is released when the relay
/// consumer dequeues that message. Both limits are therefore strict,
/// and enqueue never waits for capacity on the latency-sensitive ingest path.
#[derive(Clone)]
pub struct OtelRelayQueueSender {
    tx: mpsc::Sender<QueuedOtelRelayMessage>,
    byte_budget: Arc<Semaphore>,
}

pub struct OtelRelayQueueReceiver {
    rx: mpsc::Receiver<QueuedOtelRelayMessage>,
}

pub fn bounded_relay_queue(
    max_batches: usize,
    max_bytes: usize,
) -> (OtelRelayQueueSender, OtelRelayQueueReceiver) {
    let (tx, rx) = mpsc::channel(max_batches.max(1));
    let byte_budget = Arc::new(Semaphore::new(max_bytes.max(1)));
    (
        OtelRelayQueueSender { tx, byte_budget },
        OtelRelayQueueReceiver { rx },
    )
}

impl OtelRelayQueueSender {
    pub fn try_send(&self, message: OtelRelayMessage) -> Result<(), OtelRelayEnqueueError> {
        let payload_bytes = message.payload.len().max(1);
        let permits =
            u32::try_from(payload_bytes).map_err(|_| OtelRelayEnqueueError::ByteCapacity)?;
        let byte_permit = self
            .byte_budget
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|_| OtelRelayEnqueueError::ByteCapacity)?;

        self.tx
            .try_send(QueuedOtelRelayMessage {
                message,
                _byte_permit: byte_permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => OtelRelayEnqueueError::BatchCapacity,
                mpsc::error::TrySendError::Closed(_) => OtelRelayEnqueueError::Closed,
            })
    }
}

impl OtelRelayQueueReceiver {
    pub async fn recv(&mut self) -> Option<OtelRelayMessage> {
        self.rx.recv().await.map(|queued| queued.message)
    }
}

/// Extension point for observing or relaying decoded OTLP batches.
///
/// OSS registers [`NoopOtelRelay`] by default (a true no-op). A plugin that
/// wants to observe or forward batches registers an `Arc<dyn OtelRelay>` via
/// the service registry, and [`OtelRelaySlot`] swaps it in once all services
/// have registered.
///
/// # Contract
///
/// `relay` **must return promptly**. Implementations must enqueue `msg` onto
/// their own internal buffer or channel and return immediately. Performing
/// blocking network I/O, holding a mutex for an extended duration, or
/// synchronously awaiting a remote endpoint inside `relay` is forbidden: a
/// slow or unreachable destination must never add latency to OTLP ingestion.
/// The ingest path performs only an O(1) non-blocking `try_send` into a
/// bounded channel; implementations own the actual work in a separate
/// background task.
#[async_trait::async_trait]
pub trait OtelRelay: Send + Sync {
    /// Observe or relay a decoded OTLP batch.
    ///
    /// Implementations must return promptly. See the trait-level contract.
    async fn relay(&self, msg: OtelRelayMessage);
}

/// Default [`OtelRelay`] implementation — discards every message.
///
/// Registered at startup when no plugin provides an alternative. The OTel
/// ingest path has no observable relay side-effect when only this
/// implementation is active.
pub struct NoopOtelRelay;

#[async_trait::async_trait]
impl OtelRelay for NoopOtelRelay {
    async fn relay(&self, _msg: OtelRelayMessage) {
        // Intentional no-op.
    }
}

/// Deferred-registration handle for an [`OtelRelay`].
///
/// Constructed with [`NoopOtelRelay`] loaded by default and held by the OTel
/// plugin from `register_services` onwards. Once every plugin has finished
/// `register_services`, whichever plugin supplies an alternative relay calls
/// [`Self::set`] from `initialize_plugin_services` to swap it in — see the
/// module-level note for why this indirection exists. `relay` reads are
/// lock-free (`ArcSwap::load`).
///
/// **Write-once semantics:** only the first call to [`Self::set`] takes
/// effect. A second caller cannot silently overwrite a relay that was already
/// installed — the call is a no-op and returns `false`. This prevents a buggy
/// or late-registered plugin from replacing a correctly wired relay after the
/// fact.
pub struct OtelRelaySlot {
    /// Holds `Arc<Arc<dyn OtelRelay>>` so the outer Arc is what ArcSwap
    /// manages and the inner Arc is what callers clone on load.
    inner: arc_swap::ArcSwap<Arc<dyn OtelRelay>>,
    /// Flipped to `true` by the first successful [`Self::set`] call.
    claimed: std::sync::atomic::AtomicBool,
}

impl OtelRelaySlot {
    /// Start with [`NoopOtelRelay`] loaded.
    pub fn new_default() -> Self {
        Self {
            inner: arc_swap::ArcSwap::new(Arc::new(Arc::new(NoopOtelRelay) as Arc<dyn OtelRelay>)),
            claimed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Swap in a relay provided by a plugin, but only once.
    ///
    /// Returns `true` if the swap was applied, `false` if a relay was already
    /// set and this call was a no-op.
    pub fn set(&self, relay: Arc<dyn OtelRelay>) -> bool {
        if self
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            self.inner.store(Arc::new(relay));
            true
        } else {
            false
        }
    }
}

#[async_trait::async_trait]
impl OtelRelay for OtelRelaySlot {
    async fn relay(&self, msg: OtelRelayMessage) {
        // Clone the inner Arc<dyn OtelRelay> so it stays alive across the
        // await without holding the ArcSwap guard. `*guard` derefs Guard to
        // Arc<Arc<dyn OtelRelay>>; `**guard` derefs that to Arc<dyn OtelRelay>.
        let current = Arc::clone(&**self.inner.load());
        current.relay(msg).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    fn sample_msg(signal: OtelSignal) -> OtelRelayMessage {
        OtelRelayMessage {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            signal,
            item_count: 1,
            payload: bytes::Bytes::from_static(b"test-payload"),
        }
    }

    fn sized_msg(payload_bytes: usize) -> OtelRelayMessage {
        OtelRelayMessage {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            signal: OtelSignal::Traces,
            item_count: 10,
            payload: bytes::Bytes::from(vec![0; payload_bytes]),
        }
    }

    #[tokio::test]
    async fn bounded_queue_enforces_and_releases_byte_budget() {
        let (tx, mut rx) = bounded_relay_queue(4, 8);

        assert!(tx.try_send(sized_msg(8)).is_ok());
        assert_eq!(
            tx.try_send(sized_msg(1)),
            Err(OtelRelayEnqueueError::ByteCapacity)
        );

        let received = rx.recv().await.expect("queued message");
        assert_eq!(received.payload.len(), 8);
        assert!(tx.try_send(sized_msg(8)).is_ok());
    }

    #[test]
    fn bounded_queue_rejects_batch_larger_than_byte_budget() {
        let (tx, _rx) = bounded_relay_queue(4, 8);
        assert_eq!(
            tx.try_send(sized_msg(9)),
            Err(OtelRelayEnqueueError::ByteCapacity)
        );
    }

    #[test]
    fn bounded_queue_enforces_batch_capacity_independently() {
        let (tx, _rx) = bounded_relay_queue(1, 1024);

        assert!(tx.try_send(sized_msg(8)).is_ok());
        assert_eq!(
            tx.try_send(sized_msg(8)),
            Err(OtelRelayEnqueueError::BatchCapacity)
        );
    }

    // ── Test double ──────────────────────────────────────────────────────

    /// A relay that records the signal type of every message it receives.
    struct RecordingRelay {
        received: Mutex<Vec<OtelSignal>>,
    }

    impl RecordingRelay {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                received: Mutex::new(Vec::new()),
            })
        }

        async fn signals(&self) -> Vec<OtelSignal> {
            self.received.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl OtelRelay for RecordingRelay {
        async fn relay(&self, msg: OtelRelayMessage) {
            self.received.lock().await.push(msg.signal);
        }
    }

    // ── NoopOtelRelay ────────────────────────────────────────────────────

    #[tokio::test]
    async fn noop_relay_does_not_panic() {
        let relay = NoopOtelRelay;
        relay.relay(sample_msg(OtelSignal::Metrics)).await;
        relay.relay(sample_msg(OtelSignal::Traces)).await;
        relay.relay(sample_msg(OtelSignal::Logs)).await;
    }

    // ── OtelRelaySlot defaults ───────────────────────────────────────────

    #[tokio::test]
    async fn slot_defaults_to_noop_and_does_not_panic() {
        let slot = OtelRelaySlot::new_default();
        // Calling relay on the default slot must not panic.
        slot.relay(sample_msg(OtelSignal::Metrics)).await;
        slot.relay(sample_msg(OtelSignal::Traces)).await;
        slot.relay(sample_msg(OtelSignal::Logs)).await;
    }

    // ── set() overrides the default ──────────────────────────────────────

    #[tokio::test]
    async fn slot_set_overrides_the_default() {
        let recorder = RecordingRelay::new();
        let slot = OtelRelaySlot::new_default();

        let swapped = slot.set(recorder.clone());
        assert!(swapped, "first set() must return true");

        slot.relay(sample_msg(OtelSignal::Traces)).await;
        slot.relay(sample_msg(OtelSignal::Logs)).await;

        let signals = recorder.signals().await;
        assert_eq!(signals, vec![OtelSignal::Traces, OtelSignal::Logs]);
    }

    // ── second set() is a no-op ──────────────────────────────────────────

    #[tokio::test]
    async fn slot_second_set_is_noop() {
        let first = RecordingRelay::new();
        let second = RecordingRelay::new();
        let slot = OtelRelaySlot::new_default();

        // First set: succeeds.
        let ok1 = slot.set(first.clone());
        assert!(ok1, "first set() must return true");

        // Second set: must be a no-op, returns false.
        let ok2 = slot.set(second.clone());
        assert!(!ok2, "second set() must return false (write-once)");

        // A message sent through the slot must reach only the first relay.
        slot.relay(sample_msg(OtelSignal::Metrics)).await;

        assert_eq!(
            first.signals().await,
            vec![OtelSignal::Metrics],
            "first relay must receive the message"
        );
        assert!(
            second.signals().await.is_empty(),
            "second relay must not receive messages after rejected set()"
        );
    }

    // ── OtelSignal derives ───────────────────────────────────────────────

    #[test]
    fn otel_signal_clone_copy_eq() {
        let s = OtelSignal::Traces;
        let s2 = s; // Copy
        assert_eq!(s, s2);
        assert_ne!(OtelSignal::Metrics, OtelSignal::Logs);
    }
}
