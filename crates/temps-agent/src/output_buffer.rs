// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded tail buffers for Docker command output.
//!
//! Docker exec, backup, and restore streams are controlled by workloads, so
//! collecting them into an unbounded `String` lets a noisy container grow the
//! worker agent until it is OOM-killed. This buffer retains only the newest
//! bytes and records that earlier output was discarded.

use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures::Stream;
use serde::Serialize;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::OwnedSemaphorePermit;

/// Maximum captured bytes per stdout/stderr stream.
///
/// Two active streams therefore retain at most 8 MiB of payload. Response
/// serialization can briefly create another bounded copy.
pub(crate) const MAX_CAPTURED_STREAM_BYTES: usize = 4 * 1024 * 1024;

/// Keep response frames small enough for HTTP backpressure to prevent a slow
/// peer from moving an entire multi-megabyte capture outside the semaphore's
/// accounting in one poll.
const CAPTURE_RESPONSE_CHUNK_BYTES: usize = 64 * 1024;

const TRUNCATION_NOTICE: &str = "[… earlier output truncated by worker …]\n";

struct CaptureResponseStream {
    payload: Bytes,
    offset: usize,
    permit: Option<OwnedSemaphorePermit>,
}

impl Stream for CaptureResponseStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.offset >= self.payload.len() {
            self.payload = Bytes::new();
            self.permit.take();
            return Poll::Ready(None);
        }

        let end = self
            .offset
            .saturating_add(CAPTURE_RESPONSE_CHUNK_BYTES)
            .min(self.payload.len());
        // Use an independent allocation rather than `Bytes::slice`: a slice
        // would keep the complete multi-megabyte JSON allocation alive after
        // the permit is released merely because Hyper still owns one frame.
        let chunk = Bytes::copy_from_slice(&self.payload[self.offset..end]);
        self.offset = end;
        Poll::Ready(Some(Ok(chunk)))
    }
}

/// Serialize a captured result while its permit is held, then retain that
/// permit until the response body reaches EOF or is dropped by the server.
/// Chunking allows Hyper's normal transport backpressure to bound data queued
/// beyond the body stream.
pub(crate) fn json_response_with_capture_permit<T: Serialize>(
    status: StatusCode,
    value: T,
    permit: OwnedSemaphorePermit,
) -> Response {
    let payload = match serde_json::to_vec(&value) {
        Ok(payload) => Bytes::from(payload),
        Err(error) => {
            tracing::error!(reason = %error, "Failed to serialize captured agent response");
            drop(permit);
            let mut response = Response::new(Body::from(
                r#"{"success":false,"data":null,"error":"Failed to serialize agent response"}"#,
            ));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return response;
        }
    };

    let body = Body::from_stream(CaptureResponseStream {
        payload,
        offset: 0,
        permit: Some(permit),
    });
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn append_printable_utf8(output: &mut String, input: &str) {
    for character in input.chars() {
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            output.push('?');
        } else {
            output.push(character);
        }
    }
}

fn append_sanitized_utf8(output: &mut String, mut input: &[u8]) {
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                append_printable_utf8(output, valid);
                break;
            }
            Err(error) => {
                let valid_bytes = error.valid_up_to();
                if let Ok(valid) = std::str::from_utf8(&input[..valid_bytes]) {
                    append_printable_utf8(output, valid);
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

pub(crate) struct BoundedTailBuffer {
    bytes: Vec<u8>,
    start: usize,
    limit: usize,
    truncated: bool,
}

impl BoundedTailBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            start: 0,
            limit,
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: Bytes) {
        if chunk.is_empty() {
            return;
        }

        if self.limit == 0 {
            self.truncated = true;
            return;
        }

        if chunk.len() >= self.limit {
            self.truncated |= self.len() > 0 || chunk.len() > self.limit;
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

        // Once full, overwrite the oldest bytes in place. Each push is
        // O(chunk size), even when Docker emits millions of tiny frames.
        let overwrite = &chunk[append_bytes..];
        let first = overwrite.len().min(self.limit - self.start);
        self.bytes[self.start..self.start + first].copy_from_slice(&overwrite[..first]);
        if first < overwrite.len() {
            self.bytes[..overwrite.len() - first].copy_from_slice(&overwrite[first..]);
        }
        self.start = (self.start + overwrite.len()) % self.limit;
        self.truncated = true;
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn into_string(mut self) -> String {
        let notice_len = if self.truncated {
            TRUNCATION_NOTICE.len()
        } else {
            0
        };
        let mut output = String::with_capacity(self.bytes.len().saturating_add(notice_len));
        if self.truncated {
            output.push_str(TRUNCATION_NOTICE);
        }
        if self.bytes.len() == self.limit && self.start > 0 {
            self.bytes.rotate_left(self.start);
        }
        append_sanitized_utf8(&mut output, &self.bytes);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::Arc;

    #[test]
    fn retains_complete_output_below_limit() {
        let mut output = BoundedTailBuffer::new(16);
        output.push(Bytes::from_static(b"hello "));
        output.push(Bytes::from_static(b"world"));

        assert_eq!(output.len(), 11);
        assert_eq!(output.into_string(), "hello world");
    }

    #[test]
    fn retains_only_tail_across_chunk_boundaries() {
        let mut output = BoundedTailBuffer::new(8);
        output.push(Bytes::from_static(b"abcd"));
        output.push(Bytes::from_static(b"efgh"));
        output.push(Bytes::from_static(b"ijkl"));

        assert_eq!(output.len(), 8);
        assert_eq!(output.into_string(), format!("{TRUNCATION_NOTICE}efghijkl"));
    }

    #[test]
    fn large_single_chunk_is_bounded() {
        let mut output = BoundedTailBuffer::new(4);
        output.push(Bytes::from_static(b"0123456789"));

        assert_eq!(output.len(), 4);
        assert_eq!(output.into_string(), format!("{TRUNCATION_NOTICE}6789"));
    }

    #[test]
    fn zero_limit_discards_output_without_panicking() {
        let mut output = BoundedTailBuffer::new(0);
        output.push(Bytes::from_static(b"discard me"));

        assert_eq!(output.len(), 0);
        assert_eq!(output.into_string(), TRUNCATION_NOTICE);
    }

    #[test]
    fn many_tiny_frames_use_a_fixed_capacity_ring() {
        const LIMIT: usize = 64 * 1024;
        let mut output = BoundedTailBuffer::new(LIMIT);
        for _ in 0..200_000 {
            output.push(Bytes::from_static(b"x"));
        }

        assert_eq!(output.len(), LIMIT);
        assert_eq!(output.bytes.len(), LIMIT);
        assert_eq!(output.into_string().len(), TRUNCATION_NOTICE.len() + LIMIT);
    }

    #[test]
    fn invalid_utf8_does_not_expand_the_output() {
        let mut output = BoundedTailBuffer::new(128);
        output.push(Bytes::from(vec![0xff; 128]));

        let output = output.into_string();
        assert_eq!(output.len(), 128);
        assert!(output.bytes().all(|byte| byte == b'?'));
    }

    #[test]
    fn control_bytes_do_not_expand_during_json_serialization() {
        let mut output = BoundedTailBuffer::new(128);
        output.push(Bytes::from(vec![0; 128]));

        let output = output.into_string();
        assert_eq!(output.len(), 128);
        assert!(output.bytes().all(|byte| byte == b'?'));
    }

    #[test]
    fn preserves_utf8_character_split_across_ring_wrap() {
        let mut output = BoundedTailBuffer::new(5);
        output.push(Bytes::from_static(b"abcde"));
        output.push(Bytes::from_static(b"wxyz"));
        output.push(Bytes::from_static("😀".as_bytes()));

        assert_eq!(output.into_string(), format!("{TRUNCATION_NOTICE}z😀"));
    }

    #[tokio::test]
    async fn capture_permit_is_held_until_response_body_finishes_or_drops() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots
            .clone()
            .acquire_owned()
            .await
            .expect("test semaphore remains open");
        let payload = "x".repeat(CAPTURE_RESPONSE_CHUNK_BYTES * 2);
        let response = json_response_with_capture_permit(StatusCode::OK, &payload, permit);
        let mut body = response.into_body();

        let first = body
            .frame()
            .await
            .expect("response has a first frame")
            .expect("response frame is infallible")
            .into_data()
            .expect("response frame contains data");
        assert!(first.len() <= CAPTURE_RESPONSE_CHUNK_BYTES);
        assert!(slots.clone().try_acquire_owned().is_err());

        drop(body);
        assert_eq!(slots.available_permits(), 1);

        let permit = slots
            .clone()
            .acquire_owned()
            .await
            .expect("test semaphore remains open");
        let response = json_response_with_capture_permit(StatusCode::OK, &payload, permit);
        let collected = response
            .into_body()
            .collect()
            .await
            .expect("response body is infallible");
        assert!(!collected.to_bytes().is_empty());
        assert_eq!(slots.available_permits(), 1);
    }
}
