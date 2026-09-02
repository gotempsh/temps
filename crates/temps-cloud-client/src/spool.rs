// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded in-memory buffer for telemetry the backend could not accept yet.
//!
//! # Why bounded, and why it drops the *oldest*
//!
//! An unbounded buffer is a memory leak with a delay: a backend outage would
//! eventually OOM the instance, turning our problem into the operator's outage.
//! So the spool has a hard cap.
//!
//! When full it discards the **oldest** spans, because during an incident the
//! newest telemetry is the useful telemetry. Dropping is always counted and
//! always reported — a gap the operator cannot see is worse than no data at all.

use temps_cloud_protocol::SpanRecord;

/// Default cap. Roughly a few MB of spans — enough to ride out a short outage,
/// small enough to be irrelevant on a 4 GB box.
pub const DEFAULT_CAPACITY: usize = 10_000;
pub const DEFAULT_CAPACITY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
struct BufferedSpan {
    span: SpanRecord,
    bytes: usize,
}

#[derive(Debug)]
pub struct Spool {
    buffer: std::collections::VecDeque<BufferedSpan>,
    capacity: usize,
    capacity_bytes: usize,
    buffered_bytes: usize,
    dropped: u64,
}

impl Spool {
    pub fn new(capacity: usize) -> Self {
        Self::with_limits(capacity, usize::MAX)
    }

    pub fn with_limits(capacity: usize, capacity_bytes: usize) -> Self {
        Self {
            buffer: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
            capacity_bytes: capacity_bytes.max(1),
            buffered_bytes: 0,
            dropped: 0,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::with_limits(DEFAULT_CAPACITY, DEFAULT_CAPACITY_BYTES)
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Remove every buffered span after the operator revokes telemetry export.
    /// This is intentionally distinct from capacity drops: consent revocation
    /// must not leave exportable customer data resident for a later re-enable.
    pub fn clear(&mut self) -> usize {
        let removed = self.buffer.len();
        self.buffer.clear();
        self.buffered_bytes = 0;
        removed
    }

    /// Spans discarded because the spool was full. Never resets — it is a
    /// lifetime counter the operator can watch.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Add spans, discarding the oldest if that would exceed the cap.
    pub fn push(&mut self, spans: impl IntoIterator<Item = SpanRecord>) {
        for span in spans {
            let bytes = estimated_bytes(&span);
            if bytes > self.capacity_bytes {
                self.dropped += 1;
                continue;
            }
            while self.buffer.len() == self.capacity
                || self.buffered_bytes.saturating_add(bytes) > self.capacity_bytes
            {
                if let Some(removed) = self.buffer.pop_front() {
                    self.buffered_bytes = self.buffered_bytes.saturating_sub(removed.bytes);
                    self.dropped += 1;
                } else {
                    break;
                }
            }
            self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
            self.buffer.push_back(BufferedSpan { span, bytes });
        }
    }

    /// Take up to `max` spans for a shipping attempt.
    ///
    /// They are removed from the spool, so a caller that fails to ship MUST
    /// return them with [`Spool::requeue`]. That is deliberate: it makes losing
    /// data require an explicit mistake rather than a forgotten branch.
    pub fn take(&mut self, max: usize) -> Vec<SpanRecord> {
        let n = max.min(self.buffer.len());
        self.buffer
            .drain(..n)
            .map(|buffered| {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
                buffered.span
            })
            .collect()
    }

    /// Put spans back at the front after a failed attempt, preserving order.
    pub fn requeue(&mut self, spans: Vec<SpanRecord>) {
        for s in spans.into_iter().rev() {
            let bytes = estimated_bytes(&s);
            if bytes > self.capacity_bytes {
                self.dropped += 1;
                continue;
            }
            while self.buffer.len() == self.capacity
                || self.buffered_bytes.saturating_add(bytes) > self.capacity_bytes
            {
                // Still full: the newest already-buffered span wins over a
                // returned older one.
                if let Some(removed) = self.buffer.pop_back() {
                    self.buffered_bytes = self.buffered_bytes.saturating_sub(removed.bytes);
                    self.dropped += 1;
                } else {
                    break;
                }
            }
            self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
            self.buffer.push_front(BufferedSpan { span: s, bytes });
        }
    }
}

/// Cheap upper-bound input sizing without serializing on the telemetry path.
fn estimated_bytes(span: &SpanRecord) -> usize {
    let attributes = span.attributes.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(key.len()).saturating_add(value.len())
    });
    span.trace_id
        .len()
        .saturating_add(span.span_id.len())
        .saturating_add(span.name.len())
        .saturating_add(attributes)
        .saturating_add(std::mem::size_of::<i64>() + std::mem::size_of::<f64>() + 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(i: i64) -> SpanRecord {
        SpanRecord {
            trace_id: format!("t{i}"),
            span_id: format!("s{i}"),
            name: "GET /".into(),
            ts_millis: i,
            duration_ms: 1.0,
            attributes: Default::default(),
            ..Default::default()
        }
    }

    fn spans(range: std::ops::Range<i64>) -> Vec<SpanRecord> {
        range.map(span).collect()
    }

    #[test]
    fn spans_come_back_in_the_order_they_went_in() {
        let mut s = Spool::new(10);
        s.push(spans(0..3));
        let taken = s.take(10);
        assert_eq!(
            taken.iter().map(|x| x.ts_millis).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(s.is_empty());
    }

    #[test]
    fn a_full_spool_discards_the_oldest_and_counts_it() {
        // During an incident the newest telemetry is the useful telemetry.
        let mut s = Spool::new(3);
        s.push(spans(0..5));

        assert_eq!(s.len(), 3);
        assert_eq!(s.dropped(), 2);
        assert_eq!(
            s.take(3).iter().map(|x| x.ts_millis).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "the newest spans should survive"
        );
    }

    #[test]
    fn taking_a_partial_batch_leaves_the_rest() {
        let mut s = Spool::new(10);
        s.push(spans(0..5));
        assert_eq!(s.take(2).len(), 2);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn requeued_spans_keep_their_place_at_the_front() {
        // A failed shipment must not reorder telemetry.
        let mut s = Spool::new(10);
        s.push(spans(0..4));
        let attempt = s.take(2);
        s.requeue(attempt);

        assert_eq!(
            s.take(4).iter().map(|x| x.ts_millis).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn the_spool_never_grows_past_its_cap_however_it_is_used() {
        let mut s = Spool::new(4);
        for _ in 0..10 {
            s.push(spans(0..3));
            let t = s.take(2);
            s.requeue(t);
            assert!(s.len() <= 4, "spool exceeded its cap: {}", s.len());
        }
    }

    #[test]
    fn a_zero_capacity_spool_is_clamped_rather_than_dividing_by_zero() {
        let mut s = Spool::new(0);
        s.push(spans(0..3));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn taking_from_an_empty_spool_is_not_an_error() {
        assert!(Spool::new(10).take(5).is_empty());
    }

    #[test]
    fn clearing_removes_payloads_without_counting_capacity_drops() {
        let mut spool = Spool::new(10);
        spool.push(spans(0..3));

        assert_eq!(spool.clear(), 3);
        assert!(spool.is_empty());
        assert_eq!(spool.dropped(), 0);
    }

    #[test]
    fn serialized_content_cannot_bypass_the_memory_bound() {
        let mut s = Spool::with_limits(10_000, 256);
        let mut oversized = span(1);
        oversized.name = "x".repeat(10_000);
        s.push([oversized]);

        assert!(s.is_empty());
        assert_eq!(s.dropped(), 1);
    }
}
