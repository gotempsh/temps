// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chunked attachment uploads.
//!
//! A file arrives as a start request, a run of small multipart chunks, and a
//! completion. Each request stays far below the body size a reverse proxy in
//! front of the console may allow (a Fleet preview rejects anything over 40 KiB),
//! and the file is only stored, through the same path as a single-request
//! upload, once every byte has arrived.
//!
//! Memory is bounded: a session reserves its declared size up front against a
//! process-wide budget ([`MAX_RESERVED_BYTES`]) and against its user's share of
//! it ([`MAX_RESERVED_BYTES_PER_USER`]), so one user cannot take the whole
//! budget; a user may hold only a few sessions open; and a session is dropped
//! when nobody advances it for [`SESSION_TTL`] or, whatever happens, once it is
//! [`MAX_SESSION_LIFETIME`] old. Only a chunk that adds bytes keeps a session
//! alive: an empty chunk is refused, and a retried chunk is accepted without
//! extending the session's life, so a client cannot hold a reservation open
//! without uploading anything.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use thiserror::Error;

/// How much of a file one chunk request carries. Leaves room for the multipart
/// framing under a 40 KiB proxy limit.
pub const UPLOAD_CHUNK_BYTES: usize = 32 * 1024;
/// Uploads one user may have in flight at once (a message takes four files).
pub const MAX_ACTIVE_UPLOADS_PER_USER: usize = 8;
/// Everything all in-flight uploads may reserve together.
pub const MAX_RESERVED_BYTES: usize = 64 * 1024 * 1024;
/// What one user's in-flight uploads may reserve together. Large enough for a
/// single largest-allowed file (20 MiB) plus a few small ones, and small
/// enough that the global budget always leaves room for at least two other
/// users uploading at the same time (64 MiB = 24 + 24 + 16).
pub const MAX_RESERVED_BYTES_PER_USER: usize = 24 * 1024 * 1024;
/// An upload that no chunk has advanced for this long is discarded.
pub const SESSION_TTL: Duration = Duration::from_secs(10 * 60);
/// An upload still unfinished this long after it started is discarded even if
/// it is still advancing. At one 32 KiB chunk per request a 20 MiB file is 640
/// requests, which finishes in minutes on any working connection.
pub const MAX_SESSION_LIFETIME: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UploadError {
    #[error("Attachment upload '{upload_id}' not found for conversation '{conversation_id}' (it may have expired after {ttl_secs}s)")]
    NotFound {
        upload_id: String,
        conversation_id: String,
        ttl_secs: u64,
    },
    #[error("Attachment '{name}' is empty; there is nothing to upload")]
    Empty { name: String },
    #[error("Attachment '{name}' is {declared} bytes, over the {limit}-byte limit")]
    TooLarge {
        name: String,
        declared: usize,
        limit: usize,
    },
    #[error("User {user_id} already has {active} uploads in progress (limit {limit}); finish or cancel one first")]
    TooManyActive {
        user_id: i32,
        active: usize,
        limit: usize,
    },
    #[error("Uploads in progress already hold {reserved} bytes; the server allows {limit} at once, try again shortly")]
    OutOfCapacity { reserved: usize, limit: usize },
    #[error("User {user_id}'s uploads in progress already reserve {reserved} bytes; a new {requested}-byte upload would pass the {limit}-byte per-user limit. Finish or cancel one first")]
    UserOutOfCapacity {
        user_id: i32,
        reserved: usize,
        requested: usize,
        limit: usize,
    },
    #[error("Upload '{upload_id}' received an empty chunk at offset {offset}; every chunk must carry at least one byte")]
    EmptyChunk { upload_id: String, offset: usize },
    #[error(
        "Upload '{upload_id}' expected the next chunk at offset {expected} but got offset {got}"
    )]
    OutOfOrder {
        upload_id: String,
        expected: usize,
        got: usize,
    },
    #[error("Upload '{upload_id}' chunk of {chunk} bytes exceeds the {limit}-byte chunk size")]
    ChunkTooLarge {
        upload_id: String,
        chunk: usize,
        limit: usize,
    },
    #[error("Upload '{upload_id}' chunk would take it to {would_be} bytes, past the {declared} bytes declared at the start")]
    PastDeclaredSize {
        upload_id: String,
        would_be: usize,
        declared: usize,
    },
    #[error("Upload '{upload_id}' received {received} of {declared} bytes; send the rest before completing")]
    Incomplete {
        upload_id: String,
        received: usize,
        declared: usize,
    },
}

struct Session {
    user_id: i32,
    conversation_id: String,
    name: String,
    declared_mime: Option<String>,
    declared_size: usize,
    buffer: Vec<u8>,
    /// When the upload started; bounds its whole life.
    started: Instant,
    /// When a chunk last added bytes; bounds how long it may sit idle.
    touched: Instant,
}

impl Session {
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.touched) >= SESSION_TTL
            || now.duration_since(self.started) >= MAX_SESSION_LIFETIME
    }
}

/// What a finished upload hands to storage.
#[derive(Debug)]
pub struct FinishedUpload {
    pub name: String,
    pub declared_mime: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub struct UploadSessions {
    sessions: Mutex<HashMap<String, Session>>,
}

impl UploadSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open an upload for a file of `size` bytes. `limit` is the per-file cap.
    pub fn start(
        &self,
        user_id: i32,
        conversation_id: &str,
        name: &str,
        declared_mime: Option<String>,
        size: usize,
        limit: usize,
    ) -> Result<String, UploadError> {
        self.start_at(
            Instant::now(),
            user_id,
            conversation_id,
            name,
            declared_mime,
            size,
            limit,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_at(
        &self,
        now: Instant,
        user_id: i32,
        conversation_id: &str,
        name: &str,
        declared_mime: Option<String>,
        size: usize,
        limit: usize,
    ) -> Result<String, UploadError> {
        if size == 0 {
            return Err(UploadError::Empty {
                name: name.to_string(),
            });
        }
        if size > limit {
            return Err(UploadError::TooLarge {
                name: name.to_string(),
                declared: size,
                limit,
            });
        }
        let mut sessions = self.lock();
        sessions.retain(|_, session| !session.expired(now));
        let (active, user_reserved) = sessions
            .values()
            .filter(|session| session.user_id == user_id)
            .fold((0usize, 0usize), |(count, bytes), session| {
                (count + 1, bytes.saturating_add(session.declared_size))
            });
        if active >= MAX_ACTIVE_UPLOADS_PER_USER {
            return Err(UploadError::TooManyActive {
                user_id,
                active,
                limit: MAX_ACTIVE_UPLOADS_PER_USER,
            });
        }
        if user_reserved.saturating_add(size) > MAX_RESERVED_BYTES_PER_USER {
            return Err(UploadError::UserOutOfCapacity {
                user_id,
                reserved: user_reserved,
                requested: size,
                limit: MAX_RESERVED_BYTES_PER_USER,
            });
        }
        let reserved: usize = sessions.values().map(|session| session.declared_size).sum();
        if reserved.saturating_add(size) > MAX_RESERVED_BYTES {
            return Err(UploadError::OutOfCapacity {
                reserved,
                limit: MAX_RESERVED_BYTES,
            });
        }
        let upload_id = format!("upl_{}", uuid::Uuid::new_v4().simple());
        sessions.insert(
            upload_id.clone(),
            Session {
                user_id,
                conversation_id: conversation_id.to_string(),
                name: name.to_string(),
                declared_mime,
                declared_size: size,
                buffer: Vec::new(),
                started: now,
                touched: now,
            },
        );
        Ok(upload_id)
    }

    /// Add the chunk that starts at `offset`; returns the bytes held so far.
    /// Sending the chunk just applied again (a retry after a lost response) is
    /// accepted and changes nothing -- not even the session's idle deadline,
    /// which only a chunk that adds bytes pushes out. An empty chunk is refused.
    pub fn append(
        &self,
        user_id: i32,
        conversation_id: &str,
        upload_id: &str,
        offset: usize,
        chunk: &[u8],
    ) -> Result<usize, UploadError> {
        self.append_at(
            Instant::now(),
            user_id,
            conversation_id,
            upload_id,
            offset,
            chunk,
        )
    }

    fn append_at(
        &self,
        now: Instant,
        user_id: i32,
        conversation_id: &str,
        upload_id: &str,
        offset: usize,
        chunk: &[u8],
    ) -> Result<usize, UploadError> {
        let mut sessions = self.lock();
        let session = Self::session_mut(&mut sessions, now, user_id, conversation_id, upload_id)?;
        if chunk.len() > UPLOAD_CHUNK_BYTES {
            return Err(UploadError::ChunkTooLarge {
                upload_id: upload_id.to_string(),
                chunk: chunk.len(),
                limit: UPLOAD_CHUNK_BYTES,
            });
        }
        if chunk.is_empty() {
            return Err(UploadError::EmptyChunk {
                upload_id: upload_id.to_string(),
                offset,
            });
        }
        let received = session.buffer.len();
        let end = offset.saturating_add(chunk.len());
        if end == received && offset < received && session.buffer[offset..] == *chunk {
            // A retry: accepted, but it does not extend the session's life.
            return Ok(received);
        }
        if offset != received {
            return Err(UploadError::OutOfOrder {
                upload_id: upload_id.to_string(),
                expected: received,
                got: offset,
            });
        }
        if end > session.declared_size {
            return Err(UploadError::PastDeclaredSize {
                upload_id: upload_id.to_string(),
                would_be: end,
                declared: session.declared_size,
            });
        }
        session.buffer.extend_from_slice(chunk);
        session.touched = now;
        Ok(session.buffer.len())
    }

    /// Take the finished file out. The upload is gone afterwards either way a
    /// caller handles the result: a completed file is stored once, not twice.
    pub fn finish(
        &self,
        user_id: i32,
        conversation_id: &str,
        upload_id: &str,
    ) -> Result<FinishedUpload, UploadError> {
        self.finish_at(Instant::now(), user_id, conversation_id, upload_id)
    }

    fn finish_at(
        &self,
        now: Instant,
        user_id: i32,
        conversation_id: &str,
        upload_id: &str,
    ) -> Result<FinishedUpload, UploadError> {
        let mut sessions = self.lock();
        let session = Self::session_mut(&mut sessions, now, user_id, conversation_id, upload_id)?;
        if session.buffer.len() != session.declared_size {
            return Err(UploadError::Incomplete {
                upload_id: upload_id.to_string(),
                received: session.buffer.len(),
                declared: session.declared_size,
            });
        }
        let Some(session) = sessions.remove(upload_id) else {
            return Err(Self::not_found(upload_id, conversation_id));
        };
        Ok(FinishedUpload {
            name: session.name,
            declared_mime: session.declared_mime,
            bytes: session.buffer,
        })
    }

    /// Drop an upload and free what it reserved. Unknown ids are not an error:
    /// the goal (no upload left behind) already holds.
    pub fn cancel(&self, user_id: i32, conversation_id: &str, upload_id: &str) {
        let mut sessions = self.lock();
        let owned = sessions
            .get(upload_id)
            .is_some_and(|s| s.user_id == user_id && s.conversation_id == conversation_id);
        if owned {
            sessions.remove(upload_id);
        }
    }

    fn session_mut<'a>(
        sessions: &'a mut HashMap<String, Session>,
        now: Instant,
        user_id: i32,
        conversation_id: &str,
        upload_id: &str,
    ) -> Result<&'a mut Session, UploadError> {
        let expired = sessions.get(upload_id).is_some_and(|s| s.expired(now));
        if expired {
            sessions.remove(upload_id);
        }
        match sessions.get_mut(upload_id) {
            Some(session)
                if session.user_id == user_id && session.conversation_id == conversation_id =>
            {
                Ok(session)
            }
            _ => Err(Self::not_found(upload_id, conversation_id)),
        }
    }

    fn not_found(upload_id: &str, conversation_id: &str) -> UploadError {
        UploadError::NotFound {
            upload_id: upload_id.to_string(),
            conversation_id: conversation_id.to_string(),
            ttl_secs: SESSION_TTL.as_secs(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        // A panic while holding the lock cannot leave the map half-edited in a
        // way later requests cannot cope with, so keep serving.
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1024 * 1024;

    fn start(sessions: &UploadSessions, user: i32, size: usize) -> String {
        sessions
            .start(
                user,
                "conv",
                "shot.png",
                Some("image/png".into()),
                size,
                LIMIT,
            )
            .expect("start")
    }

    #[test]
    fn a_file_sent_in_chunks_comes_back_whole() {
        let sessions = UploadSessions::new();
        let data: Vec<u8> = (0..100_000u32).map(|n| (n % 251) as u8).collect();
        let id = start(&sessions, 1, data.len());
        let mut offset = 0;
        for chunk in data.chunks(UPLOAD_CHUNK_BYTES) {
            offset = sessions.append(1, "conv", &id, offset, chunk).unwrap();
        }
        assert_eq!(offset, data.len());
        let done = sessions.finish(1, "conv", &id).unwrap();
        assert_eq!(done.bytes, data);
        assert_eq!(done.name, "shot.png");
        assert_eq!(done.declared_mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn finishing_twice_stores_once() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 3);
        sessions.append(1, "conv", &id, 0, b"abc").unwrap();
        sessions.finish(1, "conv", &id).unwrap();
        assert!(matches!(
            sessions.finish(1, "conv", &id),
            Err(UploadError::NotFound { .. })
        ));
    }

    #[test]
    fn an_incomplete_file_cannot_be_finished_but_can_be_continued() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 6);
        sessions.append(1, "conv", &id, 0, b"abc").unwrap();
        assert_eq!(
            sessions.finish(1, "conv", &id).unwrap_err(),
            UploadError::Incomplete {
                upload_id: id.clone(),
                received: 3,
                declared: 6
            }
        );
        sessions.append(1, "conv", &id, 3, b"def").unwrap();
        assert_eq!(sessions.finish(1, "conv", &id).unwrap().bytes, b"abcdef");
    }

    #[test]
    fn a_gap_or_a_rewind_with_different_bytes_is_refused() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 9);
        sessions.append(1, "conv", &id, 0, b"abc").unwrap();
        assert!(matches!(
            sessions.append(1, "conv", &id, 5, b"xyz"),
            Err(UploadError::OutOfOrder {
                expected: 3,
                got: 5,
                ..
            })
        ));
        assert!(matches!(
            sessions.append(1, "conv", &id, 0, b"zzz"),
            Err(UploadError::OutOfOrder {
                expected: 3,
                got: 0,
                ..
            })
        ));
    }

    #[test]
    fn resending_the_last_chunk_after_a_lost_response_is_harmless() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 6);
        sessions.append(1, "conv", &id, 0, b"abc").unwrap();
        assert_eq!(sessions.append(1, "conv", &id, 0, b"abc").unwrap(), 3);
        sessions.append(1, "conv", &id, 3, b"def").unwrap();
        assert_eq!(sessions.finish(1, "conv", &id).unwrap().bytes, b"abcdef");
    }

    #[test]
    fn more_bytes_than_declared_are_refused() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 4);
        assert!(matches!(
            sessions.append(1, "conv", &id, 0, b"abcde"),
            Err(UploadError::PastDeclaredSize {
                would_be: 5,
                declared: 4,
                ..
            })
        ));
    }

    #[test]
    fn a_chunk_over_the_chunk_size_is_refused() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, UPLOAD_CHUNK_BYTES * 2);
        let big = vec![0u8; UPLOAD_CHUNK_BYTES + 1];
        assert!(matches!(
            sessions.append(1, "conv", &id, 0, &big),
            Err(UploadError::ChunkTooLarge { .. })
        ));
    }

    #[test]
    fn empty_and_oversized_files_are_refused_at_the_start() {
        let sessions = UploadSessions::new();
        assert!(matches!(
            sessions.start(1, "conv", "a", None, 0, LIMIT),
            Err(UploadError::Empty { .. })
        ));
        assert!(matches!(
            sessions.start(1, "conv", "a", None, LIMIT + 1, LIMIT),
            Err(UploadError::TooLarge { .. })
        ));
    }

    #[test]
    fn another_user_or_chat_cannot_touch_an_upload() {
        let sessions = UploadSessions::new();
        let id = start(&sessions, 1, 3);
        assert!(matches!(
            sessions.append(2, "conv", &id, 0, b"abc"),
            Err(UploadError::NotFound { .. })
        ));
        assert!(matches!(
            sessions.append(1, "other", &id, 0, b"abc"),
            Err(UploadError::NotFound { .. })
        ));
        sessions.cancel(2, "conv", &id);
        sessions.append(1, "conv", &id, 0, b"abc").unwrap();
    }

    #[test]
    fn a_user_may_hold_only_a_few_uploads_open() {
        let sessions = UploadSessions::new();
        for _ in 0..MAX_ACTIVE_UPLOADS_PER_USER {
            start(&sessions, 1, 10);
        }
        assert!(matches!(
            sessions.start(1, "conv", "a", None, 10, LIMIT),
            Err(UploadError::TooManyActive { user_id: 1, .. })
        ));
        // Someone else is not affected.
        start(&sessions, 2, 10);
    }

    #[test]
    fn reservations_are_capped_and_returned_on_cancel() {
        let sessions = UploadSessions::new();
        let per_file = 16 * 1024 * 1024;
        let mut ids = Vec::new();
        for user in 0..4 {
            let id = sessions
                .start(user, "conv", "a", None, per_file, per_file)
                .unwrap();
            ids.push((user, id));
        }
        assert!(matches!(
            sessions.start(9, "conv", "a", None, 1, per_file),
            Err(UploadError::OutOfCapacity { .. })
        ));
        let (user, id) = &ids[0];
        sessions.cancel(*user, "conv", id);
        sessions.start(9, "conv", "a", None, 1, per_file).unwrap();
    }

    #[test]
    fn one_user_cannot_reserve_the_whole_budget() {
        let sessions = UploadSessions::new();
        let max_file = 20 * 1024 * 1024;
        // A single largest-allowed file always fits.
        sessions
            .start(1, "conv", "big", None, max_file, max_file)
            .unwrap();
        // A second one would pass the user's share, though the global budget
        // still has room.
        assert_eq!(
            sessions
                .start(1, "conv", "big2", None, max_file, max_file)
                .unwrap_err(),
            UploadError::UserOutOfCapacity {
                user_id: 1,
                reserved: max_file,
                requested: max_file,
                limit: MAX_RESERVED_BYTES_PER_USER,
            }
        );
        // Small files up to the user's share still go through.
        let rest = MAX_RESERVED_BYTES_PER_USER - max_file;
        sessions
            .start(1, "conv", "rest", None, rest, max_file)
            .unwrap();
        assert!(matches!(
            sessions.start(1, "conv", "one-more", None, 1, max_file),
            Err(UploadError::UserOutOfCapacity { user_id: 1, .. })
        ));
        // Others still have room for their own largest file.
        sessions
            .start(2, "conv", "big", None, max_file, max_file)
            .unwrap();
        sessions
            .start(3, "conv", "big", None, max_file, max_file)
            .unwrap();
    }

    // The per-user share leaves room for other users: checked when the crate
    // compiles, since both sides are constants. At least two other users can
    // upload while one holds a full share.
    const _: () = {
        assert!(MAX_RESERVED_BYTES_PER_USER < MAX_RESERVED_BYTES);
        assert!(MAX_RESERVED_BYTES - MAX_RESERVED_BYTES_PER_USER >= 2 * (20 * 1024 * 1024));
    };

    #[test]
    fn an_empty_chunk_is_refused_and_does_not_keep_the_upload_alive() {
        let sessions = UploadSessions::new();
        let t0 = Instant::now();
        let id = sessions
            .start_at(t0, 1, "conv", "a", None, 3, LIMIT)
            .unwrap();
        let almost = t0 + SESSION_TTL - Duration::from_secs(1);
        assert_eq!(
            sessions
                .append_at(almost, 1, "conv", &id, 0, b"")
                .unwrap_err(),
            UploadError::EmptyChunk {
                upload_id: id.clone(),
                offset: 0
            }
        );
        let later = t0 + SESSION_TTL + Duration::from_secs(1);
        assert!(matches!(
            sessions.append_at(later, 1, "conv", &id, 0, b"abc"),
            Err(UploadError::NotFound { .. })
        ));
    }

    #[test]
    fn a_retried_chunk_does_not_extend_the_upload_life() {
        let sessions = UploadSessions::new();
        let t0 = Instant::now();
        let id = sessions
            .start_at(t0, 1, "conv", "a", None, 6, LIMIT)
            .unwrap();
        sessions.append_at(t0, 1, "conv", &id, 0, b"abc").unwrap();
        // The retry is accepted...
        let almost = t0 + SESSION_TTL - Duration::from_secs(1);
        assert_eq!(
            sessions
                .append_at(almost, 1, "conv", &id, 0, b"abc")
                .unwrap(),
            3
        );
        // ...but the idle deadline still counts from the last real progress.
        let later = t0 + SESSION_TTL + Duration::from_secs(1);
        assert!(matches!(
            sessions.append_at(later, 1, "conv", &id, 3, b"def"),
            Err(UploadError::NotFound { .. })
        ));
    }

    #[test]
    fn real_progress_extends_the_idle_deadline() {
        let sessions = UploadSessions::new();
        let t0 = Instant::now();
        let id = sessions
            .start_at(t0, 1, "conv", "a", None, 6, LIMIT)
            .unwrap();
        let t1 = t0 + SESSION_TTL - Duration::from_secs(1);
        sessions.append_at(t1, 1, "conv", &id, 0, b"abc").unwrap();
        let t2 = t1 + SESSION_TTL - Duration::from_secs(1);
        sessions.append_at(t2, 1, "conv", &id, 3, b"def").unwrap();
        assert_eq!(
            sessions.finish_at(t2, 1, "conv", &id).unwrap().bytes,
            b"abcdef"
        );
    }

    #[test]
    fn an_upload_that_keeps_advancing_still_ends_at_its_lifetime() {
        let sessions = UploadSessions::new();
        let t0 = Instant::now();
        let size = 1024;
        let id = sessions
            .start_at(t0, 1, "conv", "a", None, size, LIMIT)
            .unwrap();
        let step = SESSION_TTL / 2;
        let mut now = t0;
        let mut offset = 0;
        while now.duration_since(t0) + step < MAX_SESSION_LIFETIME {
            now += step;
            offset = sessions
                .append_at(now, 1, "conv", &id, offset, b"x")
                .unwrap();
        }
        let past_lifetime = t0 + MAX_SESSION_LIFETIME;
        assert!(matches!(
            sessions.append_at(past_lifetime, 1, "conv", &id, offset, b"x"),
            Err(UploadError::NotFound { .. })
        ));
        // And its reservation is swept on the next start.
        sessions
            .start_at(
                past_lifetime,
                1,
                "conv",
                "b",
                None,
                MAX_RESERVED_BYTES_PER_USER,
                MAX_RESERVED_BYTES_PER_USER,
            )
            .unwrap();
    }

    #[test]
    fn an_abandoned_upload_expires_and_frees_its_reservation() {
        let sessions = UploadSessions::new();
        let t0 = Instant::now();
        let id = sessions
            .start_at(t0, 1, "conv", "a", None, 3, LIMIT)
            .unwrap();
        let later = t0 + SESSION_TTL + Duration::from_secs(1);
        assert!(matches!(
            sessions.append_at(later, 1, "conv", &id, 0, b"abc"),
            Err(UploadError::NotFound { .. })
        ));
        // And a new start sweeps expired sessions out of the user's count.
        for _ in 0..MAX_ACTIVE_UPLOADS_PER_USER {
            sessions
                .start_at(t0, 1, "conv", "a", None, 3, LIMIT)
                .unwrap();
        }
        sessions
            .start_at(later, 1, "conv", "a", None, 3, LIMIT)
            .unwrap();
    }
}
