// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-clearance request counts in fixed memory.
//!
//! A count-min sketch: each clearance's `budget_id` maps to one cell in each
//! of `ROWS` rows, and its count is the smallest of those cells. Clearances
//! that share a cell add to each other's count, so a count can only be too
//! high, never too low. Nothing is evicted, which matters: a cache that
//! forgets a counter under load hands that clearance a fresh budget.
//!
//! Each cell packs the hour it counts for into its upper 32 bits. A cell
//! from an earlier hour reads as zero and is overwritten on its next use, so
//! windows roll over without a sweep. Updates are lock-free.

use std::sync::atomic::{AtomicU64, Ordering};

const ROWS: usize = 4;
const WIDTH_BITS: u32 = 18;
const WIDTH: usize = 1 << WIDTH_BITS;
const SECONDS_PER_WINDOW: u64 = 60 * 60;

/// Per-row seeds so the rows place one id in independent cells.
const ROW_SEEDS: [u64; ROWS] = [
    0x9e37_79b9_7f4a_7c15,
    0xc2b2_ae3d_27d4_eb4f,
    0x1656_67b1_9e37_79f9,
    0x85eb_ca77_c2b2_ae63,
];

/// Hourly request counts for every clearance on one ingress, in
/// `ROWS * WIDTH * 8` bytes (8 MiB).
pub(crate) struct BudgetCounter {
    cells: Box<[AtomicU64]>,
}

impl BudgetCounter {
    pub(crate) fn new() -> Self {
        Self {
            cells: (0..ROWS * WIDTH).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Counts one request for `budget_id` and returns how many it had made
    /// in the current hour before this one.
    pub(crate) fn record(&self, budget_id: &[u8; 8], now: u64) -> u32 {
        let window = u32::try_from(now / SECONDS_PER_WINDOW).unwrap_or(u32::MAX);
        let id = u64::from_le_bytes(*budget_id);
        let mut used = u32::MAX;
        for (row, seed) in ROW_SEEDS.iter().enumerate() {
            let cell = &self.cells[row * WIDTH + column(id, *seed)];
            used = used.min(increment(cell, window));
        }
        used
    }
}

/// Adds one to `cell` for `window` and returns its count before. A request
/// stamped just before the hour turned can land after one stamped just
/// after; it counts toward the cell's newer hour rather than moving the
/// cell back and wiping that hour's count.
fn increment(cell: &AtomicU64, window: u32) -> u32 {
    let mut packed = cell.load(Ordering::Relaxed);
    loop {
        let window = window.max((packed >> 32) as u32);
        let before = count_in(packed, window);
        let next = pack(window, before.saturating_add(1));
        match cell.compare_exchange_weak(packed, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return before,
            Err(current) => packed = current,
        }
    }
}

fn pack(window: u32, count: u32) -> u64 {
    (u64::from(window) << 32) | u64::from(count)
}

/// The cell's count if it is for `window`, else zero.
fn count_in(packed: u64, window: u32) -> u32 {
    if (packed >> 32) as u32 == window {
        packed as u32
    } else {
        0
    }
}

/// splitmix64's finalizer. `budget_id` is random and sealed in the
/// clearance, so visitors cannot pick ids that collide with someone else's.
fn column(id: u64, seed: u64) -> usize {
    let mut x = id ^ seed;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (x as usize) & (WIDTH - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn counts_each_id_separately() {
        let counter = BudgetCounter::new();
        assert_eq!(counter.record(&[1; 8], NOW), 0);
        assert_eq!(counter.record(&[1; 8], NOW), 1);
        assert_eq!(counter.record(&[1; 8], NOW), 2);
        assert_eq!(counter.record(&[2; 8], NOW), 0);
    }

    #[test]
    fn counts_start_over_each_hour() {
        let counter = BudgetCounter::new();
        let hour = NOW - NOW % SECONDS_PER_WINDOW;
        counter.record(&[1; 8], hour);
        assert_eq!(counter.record(&[1; 8], hour + SECONDS_PER_WINDOW - 1), 1);
        assert_eq!(counter.record(&[1; 8], hour + SECONDS_PER_WINDOW), 0);
    }

    #[test]
    fn a_late_request_from_the_previous_hour_keeps_the_new_count() {
        let counter = BudgetCounter::new();
        let hour = NOW - NOW % SECONDS_PER_WINDOW;
        let next = hour + SECONDS_PER_WINDOW;
        counter.record(&[1; 8], next);
        counter.record(&[1; 8], next);
        // Stamped in the old hour, counted after the rollover.
        assert_eq!(counter.record(&[1; 8], next - 1), 2);
        assert_eq!(counter.record(&[1; 8], next), 3);
    }

    /// The defect a size-bounded cache has: enough other clearances push a
    /// spent counter out, and it comes back empty.
    #[test]
    fn a_flood_of_other_ids_does_not_reset_a_count() {
        let counter = BudgetCounter::new();
        for _ in 0..100 {
            counter.record(&[7; 8], NOW);
        }
        for i in 0..300_000u64 {
            counter.record(&(i + 1_000).to_le_bytes(), NOW);
        }
        assert!(counter.record(&[7; 8], NOW) >= 100);
    }
}
