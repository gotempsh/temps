// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bounded hand-off from envelope ingest to the attachment uploader.
//!
//! Ingest never writes an attachment itself: it offers the envelope's
//! attachments here with one `try_send`, and a single background task
//! stores them. The queue is bounded twice, by batches and by bytes, so a
//! burst of large screenshots cannot hold more than [`QUEUE_BYTES`] in
//! memory. Past either bound the batch is dropped and counted.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Bytes;
use tokio::sync::mpsc;

/// Envelopes with attachments waiting for the uploader.
pub const QUEUE_BATCHES: usize = 256;
/// Attachment bytes waiting for the uploader, across every batch.
pub const QUEUE_BYTES: usize = 16 * 1024 * 1024;

/// One file sent with an event or feedback, validated by the envelope reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingAttachment {
    pub filename: String,
    pub content_type: String,
    pub attachment_type: String,
    pub body: Bytes,
}

/// The attachments of one envelope, for the event or feedback with
/// `sdk_event_id` (32 lowercase hex digits).
#[derive(Debug, Clone)]
pub struct AttachmentBatch {
    pub project_id: i32,
    pub sdk_event_id: String,
    pub attachments: Vec<IncomingAttachment>,
}

impl AttachmentBatch {
    pub fn bytes(&self) -> usize {
        self.attachments.iter().map(|a| a.body.len()).sum()
    }
}

/// What happened to the attachments offered by one envelope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OfferOutcome {
    pub queued: u64,
    pub dropped: u64,
}

/// Counters for the queue and the uploader, read by the usage endpoint.
#[derive(Debug, Default)]
pub struct AttachmentStats {
    pub queued: AtomicU64,
    pub dropped_queue_full: AtomicU64,
    pub stored: AtomicU64,
    pub duplicates: AtomicU64,
    pub over_quota: AtomicU64,
    pub over_event_limit: AtomicU64,
    pub without_owner: AtomicU64,
    pub failed: AtomicU64,
    pub swept: AtomicU64,
}

/// A snapshot of [`AttachmentStats`] since the process started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttachmentCounts {
    pub queued: u64,
    pub dropped_queue_full: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub over_quota: u64,
    pub over_event_limit: u64,
    pub without_owner: u64,
    pub failed: u64,
    pub swept: u64,
}

impl AttachmentStats {
    pub fn snapshot(&self) -> AttachmentCounts {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        AttachmentCounts {
            queued: read(&self.queued),
            dropped_queue_full: read(&self.dropped_queue_full),
            stored: read(&self.stored),
            duplicates: read(&self.duplicates),
            over_quota: read(&self.over_quota),
            over_event_limit: read(&self.over_event_limit),
            without_owner: read(&self.without_owner),
            failed: read(&self.failed),
            swept: read(&self.swept),
        }
    }
}

/// The sending half, held by ingest.
#[derive(Clone)]
pub struct AttachmentQueue {
    sender: mpsc::Sender<AttachmentBatch>,
    queued_bytes: Arc<AtomicUsize>,
    byte_budget: usize,
    stats: Arc<AttachmentStats>,
}

/// The receiving half, held by the uploader. Dropping a batch it took
/// returns its bytes to the budget through [`QueuedBatch`].
pub struct AttachmentReceiver {
    receiver: mpsc::Receiver<AttachmentBatch>,
    queued_bytes: Arc<AtomicUsize>,
}

/// A batch taken off the queue; its bytes leave the budget when it drops.
pub struct QueuedBatch {
    pub batch: AttachmentBatch,
    bytes: usize,
    queued_bytes: Arc<AtomicUsize>,
}

impl Drop for QueuedBatch {
    fn drop(&mut self) {
        self.queued_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl AttachmentQueue {
    pub fn new(
        batches: usize,
        byte_budget: usize,
        stats: Arc<AttachmentStats>,
    ) -> (Self, AttachmentReceiver) {
        let (sender, receiver) = mpsc::channel(batches.max(1));
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        (
            Self {
                sender,
                queued_bytes: queued_bytes.clone(),
                byte_budget,
                stats,
            },
            AttachmentReceiver {
                receiver,
                queued_bytes,
            },
        )
    }

    /// Hand `batch` to the uploader without waiting. When the queue holds
    /// as many batches or bytes as it may, the batch is dropped and counted.
    pub fn offer(&self, batch: AttachmentBatch) -> OfferOutcome {
        let files = batch.attachments.len() as u64;
        if files == 0 {
            return OfferOutcome::default();
        }
        let bytes = batch.bytes();
        let before = self.queued_bytes.fetch_add(bytes, Ordering::AcqRel);
        if before.saturating_add(bytes) > self.byte_budget {
            self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.stats
                .dropped_queue_full
                .fetch_add(files, Ordering::Relaxed);
            return OfferOutcome {
                queued: 0,
                dropped: files,
            };
        }
        match self.sender.try_send(batch) {
            Ok(()) => {
                self.stats.queued.fetch_add(files, Ordering::Relaxed);
                OfferOutcome {
                    queued: files,
                    dropped: 0,
                }
            }
            Err(_) => {
                self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                self.stats
                    .dropped_queue_full
                    .fetch_add(files, Ordering::Relaxed);
                OfferOutcome {
                    queued: 0,
                    dropped: files,
                }
            }
        }
    }

    /// Bytes waiting for the uploader.
    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes.load(Ordering::Acquire)
    }
}

impl AttachmentReceiver {
    /// The next batch, or `None` once every sender is gone.
    pub async fn next(&mut self) -> Option<QueuedBatch> {
        let batch = self.receiver.recv().await?;
        Some(QueuedBatch {
            bytes: batch.bytes(),
            batch,
            queued_bytes: self.queued_bytes.clone(),
        })
    }
}
