// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A HyperLogLog sketch for distinct counts that must merge across hours
//! and flushes: users, errored sessions and crashed users of a release.
//!
//! 256 one-byte registers (precision 8), a standard error of about 6.5%.
//! The register layout is also what `temps_hll_merge` in the database
//! merges, so the byte format is fixed: changing it would need a migration.
//! Identifiers are hashed with SHA-256 so the sketch is stable across
//! processes and releases of Temps; no identifier is ever stored.

use sha2::{Digest, Sha256};

/// Bits of the hash that select a register.
const PRECISION: u32 = 8;
/// Registers, and the byte length of a stored sketch.
pub const REGISTERS: usize = 1 << PRECISION;

#[derive(Clone, PartialEq, Eq)]
pub struct Hll {
    registers: Box<[u8; REGISTERS]>,
}

impl std::fmt::Debug for Hll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hll")
            .field("estimate", &self.estimate())
            .finish()
    }
}

impl Default for Hll {
    fn default() -> Self {
        Self {
            registers: Box::new([0; REGISTERS]),
        }
    }
}

/// The 64-bit hash an identifier is counted by.
pub fn hash_id(id: &str) -> u64 {
    let digest = Sha256::digest(id.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

impl Hll {
    /// A sketch from its stored bytes; `None` for a length that is not this
    /// precision's, which the database never writes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let registers: [u8; REGISTERS] = bytes.try_into().ok()?;
        Some(Self {
            registers: Box::new(registers),
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.registers.as_slice()
    }

    pub fn insert_hash(&mut self, hash: u64) {
        let index = (hash >> (64 - PRECISION)) as usize;
        let rest = hash << PRECISION;
        // The rank of the first set bit in the remaining 56 bits, 1-based;
        // all zeros ranks one past the last bit.
        let rank = (rest.leading_zeros().min(64 - PRECISION) + 1) as u8;
        if rank > self.registers[index] {
            self.registers[index] = rank;
        }
    }

    pub fn insert(&mut self, id: &str) {
        self.insert_hash(hash_id(id));
    }

    pub fn merge(&mut self, other: &Hll) {
        for (mine, theirs) in self.registers.iter_mut().zip(other.registers.iter()) {
            if *theirs > *mine {
                *mine = *theirs;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.registers.iter().all(|r| *r == 0)
    }

    /// The estimated number of distinct identifiers inserted.
    pub fn estimate(&self) -> f64 {
        let m = REGISTERS as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0;
        let mut zeros = 0u32;
        for register in self.registers.iter() {
            sum += 2f64.powi(-i32::from(*register));
            if *register == 0 {
                zeros += 1;
            }
        }
        let raw = alpha * m * m / sum;
        // Small cardinalities: linear counting over the empty registers is
        // far more accurate than the raw estimate.
        if raw <= 2.5 * m && zeros > 0 {
            m * (m / f64::from(zeros)).ln()
        } else {
            raw
        }
    }

    /// The estimate rounded to a count, never more than `ceiling` (a
    /// distinct count cannot exceed the number of things counted).
    pub fn count(&self, ceiling: Option<i64>) -> i64 {
        let estimate = self.estimate().round() as i64;
        match ceiling {
            Some(ceiling) => estimate.min(ceiling.max(0)),
            None => estimate,
        }
    }
}

/// Merge an optional stored sketch into `target`.
pub fn merge_bytes(target: &mut Option<Hll>, bytes: Option<&[u8]>) {
    let Some(other) = bytes.and_then(Hll::from_bytes) else {
        return;
    };
    match target {
        Some(sketch) => sketch.merge(&other),
        None => *target = Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sketch_of(range: std::ops::Range<u32>, prefix: &str) -> Hll {
        let mut hll = Hll::default();
        for n in range {
            hll.insert(&format!("{prefix}-{n}"));
        }
        hll
    }

    #[test]
    fn empty_sketch_estimates_zero() {
        let hll = Hll::default();
        assert!(hll.is_empty());
        assert_eq!(hll.count(None), 0);
    }

    #[test]
    fn small_counts_are_nearly_exact() {
        for n in [1u32, 5, 20, 60] {
            let estimate = sketch_of(0..n, "user").estimate();
            assert!(
                (estimate - f64::from(n)).abs() <= f64::from(n) * 0.1 + 1.0,
                "{n} users estimated as {estimate}"
            );
        }
    }

    #[test]
    fn large_counts_stay_within_the_documented_error() {
        for n in [1_000u32, 10_000, 100_000] {
            let estimate = sketch_of(0..n, "user").estimate();
            let error = (estimate - f64::from(n)).abs() / f64::from(n);
            // Three standard errors of a 256-register sketch.
            assert!(error < 0.2, "{n} users estimated as {estimate}");
        }
    }

    #[test]
    fn duplicates_do_not_count_twice() {
        let mut hll = sketch_of(0..500, "user");
        let before = hll.clone();
        for n in 0..500 {
            hll.insert(&format!("user-{n}"));
        }
        assert_eq!(hll, before);
    }

    #[test]
    fn merge_is_the_union() {
        let mut left = sketch_of(0..3_000, "user");
        let right = sketch_of(2_000..5_000, "user");
        left.merge(&right);
        assert_eq!(left, sketch_of(0..5_000, "user"));
    }

    #[test]
    fn bytes_round_trip_and_wrong_lengths_are_refused() {
        let hll = sketch_of(0..100, "user");
        assert_eq!(Hll::from_bytes(hll.as_bytes()), Some(hll.clone()));
        assert!(Hll::from_bytes(&[0u8; 16]).is_none());
        let mut target = None;
        merge_bytes(&mut target, Some(&[1u8; 3]));
        assert!(target.is_none());
        merge_bytes(&mut target, Some(hll.as_bytes()));
        assert_eq!(target, Some(hll));
    }

    #[test]
    fn count_never_exceeds_its_ceiling() {
        let hll = sketch_of(0..50, "user");
        assert!(hll.count(Some(10)) <= 10);
        assert_eq!(hll.count(Some(-1)), 0);
    }

    #[test]
    fn hashes_are_stable() {
        // The stored registers depend on this value never changing: the
        // first eight bytes of SHA-256, big-endian.
        assert_eq!(hash_id("synthetic-user"), 0x14b2_e141_bf7c_c394);
        assert_ne!(hash_id("synthetic-user"), hash_id("synthetic-user-2"));
    }
}
