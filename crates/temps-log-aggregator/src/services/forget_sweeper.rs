// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drains the durable line-index forget backlog (ADR-047 §8a).
//!
//! Compaction, purge and retention each attempt an immediate
//! `LineIndexSink::forget_chunks` when they retire a chunk, but that call
//! crosses the network to ClickHouse or Temps Cloud and can fail: a
//! transient connection error, or (on Cloud) a rejected insert during an
//! outage. Before this sweeper existed, a failed forget was logged and
//! dropped — the chunk's rows stayed in the index, silently double-counted
//! or (for a hard-deleted/compacted chunk) pointing at data the reader can
//! no longer resolve, for as long as the index's own retention window.
//!
//! Every caller now enqueues into `log_line_forget_backlog` *before*
//! attempting the immediate forget (see
//! [`crate::store::manifest::ManifestRepo::enqueue_forget`]), so the record
//! survives a crash between the two. This sweeper is the retry loop: it
//! drains the backlog on an interval, independent of which call site (or
//! which past server run) created the entry, until every one resolves.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::index::LineIndexSink;
use crate::store::manifest::ManifestRepo;

/// Backlog rows examined per [`ForgetSweeper::run_once`] call. Forgets are
/// cheap (a `DELETE`/tombstone insert, not a read), so this can be generous
/// without risking a slow pass.
pub const DEFAULT_FORGET_BATCH: u32 = 256;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ForgetSweepReport {
    /// Chunks confirmed forgotten and removed from the backlog this pass.
    pub resolved: u64,
    /// Chunks still pending after this pass (forget failed again).
    pub failed: u64,
    /// True when the batch was full, i.e. more work is likely waiting.
    pub more: bool,
}

pub struct ForgetSweeper {
    manifests: Arc<ManifestRepo>,
    line_index: Arc<dyn LineIndexSink>,
}

impl ForgetSweeper {
    pub fn new(manifests: Arc<ManifestRepo>, line_index: Arc<dyn LineIndexSink>) -> Self {
        Self {
            manifests,
            line_index,
        }
    }

    /// Retry up to `batch` pending forgets, oldest first. Returns early
    /// (`more = false`, nothing attempted) when the index reports itself
    /// unavailable — retrying against a store that is not there would just
    /// grow `attempts` on every entry for no reason; the backlog is exactly
    /// as durable while it waits for the index to come back.
    pub async fn run_once(&self, batch: u32) -> ForgetSweepReport {
        let mut report = ForgetSweepReport::default();
        if self.line_index.unavailable_reason().is_some() {
            return report;
        }

        let seqs = match self.manifests.pending_forgets(batch).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "forget sweep: could not list the backlog");
                return report;
            }
        };
        if seqs.is_empty() {
            return report;
        }
        report.more = seqs.len() as u32 >= batch;

        match self.line_index.forget_chunks(&seqs).await {
            Ok(()) => {
                report.resolved = seqs.len() as u64;
                if let Err(e) = self.manifests.resolve_forgets(&seqs).await {
                    // The index confirmed the forget but the backlog row
                    // could not be cleared — the next sweep will just ask
                    // the index to forget an already-forgotten chunk again,
                    // which every backend treats as a no-op, so this is a
                    // wasted retry rather than a correctness problem.
                    warn!(chunks = seqs.len(), error = %e, "forget sweep: resolved but could not clear the backlog");
                }
            }
            Err(e) => {
                report.failed = seqs.len() as u64;
                if let Err(record_err) = self
                    .manifests
                    .record_forget_failure(&seqs, &e.to_string())
                    .await
                {
                    warn!(error = %record_err, "forget sweep: could not record the failed attempt");
                }
                warn!(chunks = seqs.len(), error = %e, "forget sweep: index still refuses these chunks");
            }
        }

        if report.resolved > 0 {
            info!(
                resolved = report.resolved,
                failed = report.failed,
                more = report.more,
                "forget sweep pass"
            );
        } else if report.failed > 0 {
            debug!(
                failed = report.failed,
                "forget sweep pass: nothing resolved"
            );
        }
        report
    }
}

/// Interval between sweeps once the backlog is empty (or the index is
/// unavailable): frequent enough that a Cloud outage's worth of retired
/// chunks does not linger for long once the outage ends, cheap enough to
/// run forever in the background.
pub const FORGET_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Short pause between passes while there is more backlog than one batch —
/// mirrors the reindexer's burst/idle shape.
pub const FORGET_SWEEP_BURST_PAUSE: Duration = Duration::from_millis(500);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkLabels;
    use crate::error::LogAggregatorError;
    use crate::index::IndexOutcome;
    use crate::types::LogLine;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// A sink whose `forget_chunks` fails a configurable number of times
    /// before succeeding, so the sweeper's retry-until-resolved behaviour is
    /// exercised without a live database or ClickHouse.
    struct FlakySink {
        remaining_failures: AtomicUsize,
        forgotten: Mutex<Vec<i64>>,
    }

    #[async_trait]
    impl LineIndexSink for FlakySink {
        async fn index_chunk(
            &self,
            _seq: i64,
            _labels: &ChunkLabels,
            _segments: &[Arc<Vec<LogLine>>],
        ) -> Result<IndexOutcome, LogAggregatorError> {
            Ok(IndexOutcome::Indexed)
        }

        async fn forget_chunks(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
            if self.remaining_failures.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(LogAggregatorError::LineIndex {
                    reason: "flaky sink: simulated failure".into(),
                });
            }
            self.forgotten.lock().unwrap().extend(seqs.iter().copied());
            Ok(())
        }
    }

    #[test]
    fn report_defaults_to_nothing_done() {
        let report = ForgetSweepReport::default();
        assert_eq!(report.resolved, 0);
        assert_eq!(report.failed, 0);
        assert!(!report.more);
    }

    /// The behaviour this sweeper exists for: an entry that fails the first
    /// pass is neither lost nor resolved early, and a later pass — once the
    /// index recovers — clears it. Uses out-of-range negative sequence
    /// numbers so this never collides with another test's chunks in the
    /// shared schema (the backlog table has no foreign key to `log_chunks`).
    #[tokio::test]
    #[serial_test::serial]
    async fn a_failed_forget_stays_queued_and_a_later_pass_resolves_it() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let manifests = Arc::new(ManifestRepo::new(db.connection_arc()));
        let seq = -900_101_i64;
        manifests.enqueue_forget(&[seq]).await.unwrap();

        let sink = Arc::new(FlakySink {
            // First `run_once` call fails, the second succeeds.
            remaining_failures: AtomicUsize::new(1),
            forgotten: Mutex::new(Vec::new()),
        });
        let sweeper = ForgetSweeper::new(manifests.clone(), sink.clone());

        let first = sweeper.run_once(10).await;
        assert_eq!(first.resolved, 0);
        assert_eq!(first.failed, 1);
        assert!(
            manifests.pending_forgets(10).await.unwrap().contains(&seq),
            "a failed attempt must not drop the backlog entry"
        );

        let second = sweeper.run_once(10).await;
        assert_eq!(second.resolved, 1);
        assert_eq!(second.failed, 0);
        assert!(
            !manifests.pending_forgets(10).await.unwrap().contains(&seq),
            "a resolved attempt must clear the backlog entry"
        );
        assert_eq!(sink.forgotten.lock().unwrap().as_slice(), &[seq]);
    }
}
