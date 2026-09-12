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

/// Bounded ring buffer that keeps the tail of an appended byte stream.
///
/// The buffer capacity is set at construction and never changes. When
/// `append` would cause the total size to exceed `capacity`, bytes are
/// discarded from the front (oldest data) to make room for the new chunk.
///
/// # Examples
///
/// ```rust
/// use temps_backup::engines::ring_buffer::RingBuffer;
///
/// let mut buf = RingBuffer::with_capacity(16);
/// buf.append(b"hello, ");
/// buf.append(b"world");
/// assert_eq!(buf.into_string_lossy(), "hello, world");
/// ```
pub struct RingBuffer {
    capacity: usize,
    buf: Vec<u8>,
}

impl RingBuffer {
    /// Create a new `RingBuffer` that keeps at most `capacity` bytes.
    ///
    /// If `capacity` is 0, every `append` is a no-op and `into_string_lossy`
    /// always returns an empty string.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            buf: Vec::with_capacity(capacity.min(64 * 1024)),
        }
    }

    /// Append `chunk` to the buffer.
    ///
    /// If appending `chunk` would push the total length over `capacity`,
    /// bytes are dropped from the front (oldest) until the buffer fits
    /// within `capacity`. If `chunk` itself is larger than `capacity`,
    /// only the trailing `capacity` bytes of `chunk` are kept.
    pub fn append(&mut self, chunk: &[u8]) {
        if self.capacity == 0 {
            return;
        }

        // If the incoming chunk alone exceeds capacity, keep only its tail.
        let chunk = if chunk.len() >= self.capacity {
            &chunk[chunk.len() - self.capacity..]
        } else {
            chunk
        };

        // How many existing bytes need to be evicted to fit `chunk`?
        let combined = self.buf.len() + chunk.len();
        if combined > self.capacity {
            let drop_count = combined - self.capacity;
            self.buf.drain(..drop_count);
        }

        self.buf.extend_from_slice(chunk);
    }

    /// Return the buffered data as a `String`, replacing invalid UTF-8 sequences
    /// with the Unicode replacement character (`U+FFFD`).
    ///
    /// Consumes the `RingBuffer`.
    pub fn into_string_lossy(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    /// Return the current number of bytes in the buffer.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Return `true` if the buffer contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

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

    /// Empty buffer returns empty string.
    #[test]
    fn test_empty_buffer_returns_empty_string() {
        let buf = RingBuffer::with_capacity(64);
        assert_eq!(buf.into_string_lossy(), "");
    }

    /// A single small chunk is preserved exactly.
    #[test]
    fn test_small_chunk_preserved() {
        let mut buf = RingBuffer::with_capacity(64);
        buf.append(b"hello");
        assert_eq!(buf.into_string_lossy(), "hello");
    }

    /// A chunk larger than capacity keeps only the tail.
    #[test]
    fn test_chunk_larger_than_capacity_keeps_tail() {
        let mut buf = RingBuffer::with_capacity(8);
        buf.append(b"0123456789"); // 10 bytes > capacity 8
        assert_eq!(buf.into_string_lossy(), "23456789"); // last 8 bytes
    }

    /// Multiple appends staying within capacity are all preserved.
    #[test]
    fn test_multiple_appends_within_capacity() {
        let mut buf = RingBuffer::with_capacity(16);
        buf.append(b"hello, ");
        buf.append(b"world");
        assert_eq!(buf.into_string_lossy(), "hello, world");
    }

    /// Multiple appends overflowing capacity keep only the tail.
    #[test]
    fn test_multiple_appends_overflowing_capacity_keeps_tail() {
        let mut buf = RingBuffer::with_capacity(8);
        buf.append(b"abcd"); // 4 bytes — fits
        buf.append(b"efgh"); // 4 bytes — exactly fills capacity
        buf.append(b"ijkl"); // 4 bytes — overflows; oldest 4 must be dropped
                             // After overflow: buf had "abcdefgh" (8), then "ijkl" appended → drop 4
                             // → keeps "efghijkl" (last 8 bytes of all appended data)
        assert_eq!(buf.into_string_lossy(), "efghijkl");
    }

    /// Zero-capacity buffer is always empty.
    #[test]
    fn test_zero_capacity_always_empty() {
        let mut buf = RingBuffer::with_capacity(0);
        buf.append(b"any data");
        // Check is_empty() before consuming the buffer.
        assert!(buf.is_empty());
        assert_eq!(buf.into_string_lossy(), "");
    }

    /// `len()` reports current buffer length correctly.
    #[test]
    fn test_len_tracks_content() {
        let mut buf = RingBuffer::with_capacity(16);
        assert_eq!(buf.len(), 0);
        buf.append(b"abc");
        assert_eq!(buf.len(), 3);
        buf.append(b"def");
        assert_eq!(buf.len(), 6);
    }

    /// `is_empty()` returns true initially and false after data.
    #[test]
    fn test_is_empty() {
        let mut buf = RingBuffer::with_capacity(8);
        assert!(buf.is_empty());
        buf.append(b"x");
        assert!(!buf.is_empty());
    }

    /// Invalid UTF-8 bytes are replaced with the replacement character.
    #[test]
    fn test_invalid_utf8_is_replaced() {
        let mut buf = RingBuffer::with_capacity(16);
        buf.append(&[0xFF, 0xFE]); // invalid UTF-8
        let s = buf.into_string_lossy();
        assert!(s.contains('\u{FFFD}'));
    }

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
