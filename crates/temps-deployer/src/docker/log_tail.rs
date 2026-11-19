// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded capture of container logs.

/// Maximum payload returned by the one-shot container logs endpoint.
///
/// The control plane persists at most 8 MiB during container teardown, so
/// retaining more on a worker only increases memory and transfer cost.
pub(super) const MAX_CONTAINER_LOG_BYTES: usize = 8 * 1024 * 1024;
const LOG_TRUNCATION_NOTICE: &str = "[… earlier container logs truncated by worker …]\n";

fn append_printable_log_utf8(output: &mut String, input: &str) {
    for character in input.chars() {
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            output.push('?');
        } else {
            output.push(character);
        }
    }
}

fn append_sanitized_log_utf8(output: &mut String, mut input: &[u8]) {
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                append_printable_log_utf8(output, valid);
                break;
            }
            Err(error) => {
                let valid_bytes = error.valid_up_to();
                if let Ok(valid) = std::str::from_utf8(&input[..valid_bytes]) {
                    append_printable_log_utf8(output, valid);
                }
                output.push('?');
                match error.error_len() {
                    Some(invalid_bytes) => input = &input[valid_bytes + invalid_bytes..],
                    None => break,
                }
            }
        }
    }
}

pub(super) struct DockerLogTail {
    bytes: Vec<u8>,
    start: usize,
    limit: usize,
    truncated: bool,
}

impl DockerLogTail {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            start: 0,
            limit,
            truncated: false,
        }
    }

    pub(super) fn push(&mut self, chunk: bytes::Bytes) {
        if chunk.is_empty() {
            return;
        }

        if self.limit == 0 {
            self.truncated = true;
            return;
        }

        if chunk.len() >= self.limit {
            self.truncated |= !self.bytes.is_empty() || chunk.len() > self.limit;
            self.bytes.clear();
            self.start = 0;
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.limit..]);
            return;
        }

        let mut append_bytes = 0;
        if self.bytes.len() < self.limit {
            append_bytes = chunk.len().min(self.limit - self.bytes.len());
            self.bytes.extend_from_slice(&chunk[..append_bytes]);
            if append_bytes == chunk.len() {
                return;
            }
        }

        let overwrite = &chunk[append_bytes..];
        let first = overwrite.len().min(self.limit - self.start);
        self.bytes[self.start..self.start + first].copy_from_slice(&overwrite[..first]);
        if first < overwrite.len() {
            self.bytes[..overwrite.len() - first].copy_from_slice(&overwrite[first..]);
        }
        self.start = (self.start + overwrite.len()) % self.limit;
        self.truncated = true;
    }

    pub(super) fn into_string(mut self) -> String {
        let notice_len = if self.truncated {
            LOG_TRUNCATION_NOTICE.len()
        } else {
            0
        };
        let mut output = String::with_capacity(self.bytes.len().saturating_add(notice_len));
        if self.truncated {
            output.push_str(LOG_TRUNCATION_NOTICE);
        }
        if self.bytes.len() == self.limit && self.start > 0 {
            self.bytes.rotate_left(self.start);
        }
        append_sanitized_log_utf8(&mut output, &self.bytes);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_log_tail_keeps_memory_bounded_and_retains_newest_bytes() {
        let mut logs = DockerLogTail::new(8);
        logs.push(bytes::Bytes::from_static(b"abcd"));
        logs.push(bytes::Bytes::from_static(b"efgh"));
        logs.push(bytes::Bytes::from_static(b"ijkl"));

        assert_eq!(
            logs.into_string(),
            format!("{LOG_TRUNCATION_NOTICE}efghijkl")
        );
    }

    #[test]
    fn docker_log_tail_uses_a_fixed_capacity_ring() {
        const LIMIT: usize = 64 * 1024;
        let mut logs = DockerLogTail::new(LIMIT);
        for _ in 0..200_000 {
            logs.push(bytes::Bytes::from_static(b"x"));
        }

        assert_eq!(logs.bytes.len(), LIMIT);
        assert_eq!(
            logs.into_string().len(),
            LOG_TRUNCATION_NOTICE.len() + LIMIT
        );
    }

    #[test]
    fn docker_log_tail_invalid_utf8_does_not_expand_output() {
        let mut logs = DockerLogTail::new(128);
        logs.push(bytes::Bytes::from(vec![0xff; 128]));

        let logs = logs.into_string();
        assert_eq!(logs.len(), 128);
        assert!(logs.bytes().all(|byte| byte == b'?'));
    }

    #[test]
    fn docker_log_tail_control_bytes_do_not_expand_in_json() {
        let mut logs = DockerLogTail::new(128);
        logs.push(bytes::Bytes::from(vec![0; 128]));

        let logs = logs.into_string();
        assert_eq!(logs.len(), 128);
        assert!(logs.bytes().all(|byte| byte == b'?'));
    }

    #[test]
    fn docker_log_tail_preserves_utf8_split_across_ring_wrap() {
        let mut logs = DockerLogTail::new(5);
        logs.push(bytes::Bytes::from_static(b"abcde"));
        logs.push(bytes::Bytes::from_static(b"wxyz"));
        logs.push(bytes::Bytes::from_static("😀".as_bytes()));

        assert_eq!(logs.into_string(), format!("{LOG_TRUNCATION_NOTICE}z😀"));
    }
}
