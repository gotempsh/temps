// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A bounded ring of log lines per instance.

use std::collections::VecDeque;

use bytes::Bytes;
use temps_workload::LogLine;

/// Longest line kept; longer lines are cut.
const MAX_LINE_BYTES: usize = 4096;

/// The last `max_bytes` of an instance's log lines. Memory is bounded by
/// `max_bytes` plus one line, whatever the guest writes.
#[derive(Debug)]
pub struct LogRing {
    lines: VecDeque<LogLine>,
    bytes: usize,
    max_bytes: usize,
}

impl LogRing {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            max_bytes,
        }
    }

    pub fn push(&mut self, stderr: bool, mut line: Bytes) {
        if line.len() > MAX_LINE_BYTES {
            line.truncate(MAX_LINE_BYTES);
        }
        self.bytes += line.len();
        self.lines.push_back(LogLine { stderr, line });
        while self.bytes > self.max_bytes {
            match self.lines.pop_front() {
                Some(dropped) => self.bytes -= dropped.line.len(),
                None => {
                    self.bytes = 0;
                    break;
                }
            }
        }
    }

    /// The last `tail` lines (all when `None`), oldest first.
    pub fn tail(&self, tail: Option<u32>) -> Vec<LogLine> {
        let count = tail.map_or(self.lines.len(), |tail| self.lines.len().min(tail as usize));
        self.lines
            .iter()
            .skip(self.lines.len() - count)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_only_the_newest_bytes() {
        let mut ring = LogRing::new(10);
        ring.push(false, Bytes::from_static(b"aaaa"));
        ring.push(false, Bytes::from_static(b"bbbb"));
        ring.push(true, Bytes::from_static(b"cccc"));
        assert_eq!(ring.len(), 2, "the oldest line went over the bound");
        let lines = ring.tail(None);
        assert_eq!(lines[0].line, Bytes::from_static(b"bbbb"));
        assert!(lines[1].stderr);
        assert_eq!(ring.tail(Some(1))[0].line, Bytes::from_static(b"cccc"));
    }

    #[test]
    fn a_long_line_is_cut() {
        let mut ring = LogRing::new(1 << 20);
        ring.push(false, Bytes::from(vec![b'x'; MAX_LINE_BYTES * 2]));
        assert_eq!(ring.tail(None)[0].line.len(), MAX_LINE_BYTES);
    }
}
