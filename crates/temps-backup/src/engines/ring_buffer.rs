// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded ring buffer for capturing the tail of a byte stream.
//!
//! Used by backup engines to retain at most N bytes of stdout/stderr from a
//! long-running docker exec without growing unboundedly. A docker exec that
//! generates gigabytes of output (e.g., a verbose `wal-g backup-push`) will
//! only keep the last `capacity` bytes, which is sufficient to diagnose
//! failures.
//!
//! The buffer keeps the **tail** of the stream: when the total accumulated
//! bytes exceed `capacity`, bytes are dropped from the front.

pub use temps_docker_util::RingBuffer;

/// Streaming, case-insensitive substring detector with bounded memory.
///
/// Unlike [`RingBuffer`], which keeps the tail of a stream for display and
/// can therefore evict an earlier diagnostic line once enough later output
/// arrives, this only needs to answer "did this substring ever appear
/// anywhere in the stream" -- so it never has to retain the stream itself.
/// It carries forward at most `needle.len() - 1` bytes between chunks (to
/// catch a match split across a chunk boundary), which bounds its memory
/// to the needle's length regardless of how much total output streams
/// through, including a verbose command that emits gigabytes.
pub struct SubstringWatcher {
    needle: String,
    carry: Vec<u8>,
    matched: bool,
}

impl SubstringWatcher {
    /// `needle` is matched case-insensitively.
    pub fn new(needle: &str) -> Self {
        Self {
            needle: needle.to_ascii_lowercase(),
            carry: Vec::new(),
            matched: false,
        }
    }

    /// Feed the next chunk of the stream. No-op once a match has been found.
    pub fn feed(&mut self, chunk: &[u8]) {
        if self.matched || self.needle.is_empty() {
            return;
        }
        let mut combined = std::mem::take(&mut self.carry);
        combined.extend_from_slice(chunk);
        let lowered = String::from_utf8_lossy(&combined).to_ascii_lowercase();
        if lowered.contains(&self.needle) {
            self.matched = true;
            return;
        }
        let keep = self.needle.len().saturating_sub(1);
        self.carry = if combined.len() > keep {
            combined[combined.len() - keep..].to_vec()
        } else {
            combined
        };
    }

    /// `true` if `needle` has appeared in any chunk fed so far.
    pub fn matched(&self) -> bool {
        self.matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watcher matches when the needle is fed in a single chunk.
    #[test]
    fn watcher_matches_within_a_single_chunk() {
        let mut watcher = SubstringWatcher::new("access denied");
        watcher.feed(b"mc: <ERROR> Unable to list comparison retrying.. Access Denied");
        assert!(watcher.matched());
    }

    /// A watcher matches case-insensitively.
    #[test]
    fn watcher_matches_case_insensitively() {
        let mut watcher = SubstringWatcher::new("access denied");
        watcher.feed(b"ACCESS DENIED");
        assert!(watcher.matched());
    }

    /// The whole point of the watcher: it must still catch a match that
    /// later output would have evicted from a bounded tail buffer, because
    /// it never discards its verdict once found.
    #[test]
    fn watcher_survives_gigabytes_of_output_after_the_match() {
        let mut watcher = SubstringWatcher::new("access denied");
        watcher.feed(b"mc: <ERROR> Unable to list comparison retrying.. Access Denied");
        // Simulate far more stderr than a 4 KiB tail buffer could retain.
        let filler = vec![b'x'; 4 * 1024];
        for _ in 0..16 {
            watcher.feed(&filler);
        }
        assert!(watcher.matched());
    }

    /// The needle can be split exactly across two chunk boundaries -- the
    /// carried-forward suffix from the previous chunk must be considered
    /// together with the next one.
    #[test]
    fn watcher_matches_needle_split_across_chunk_boundary() {
        let mut watcher = SubstringWatcher::new("access denied");
        watcher.feed(b"...access den");
        assert!(!watcher.matched());
        watcher.feed(b"ied...");
        assert!(watcher.matched());
    }

    /// No match, no false positive.
    #[test]
    fn watcher_does_not_match_unrelated_output() {
        let mut watcher = SubstringWatcher::new("access denied");
        watcher.feed(b"mc: `source/foo.txt`: object is a delete marker, skipping");
        assert!(!watcher.matched());
    }
}
