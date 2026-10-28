// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sends the worker's Shield events to the control plane (ADR 066, I8).
//!
//! The ingress hands events to [`EventUploader::submit`], which never
//! waits: a full queue drops them. A task gathers them and posts them
//! gzip-compressed to `POST /api/internal/nodes/{id}/shield/events` when a
//! batch is full or the interval passes. A failed post is retried with
//! backoff while the events wait in a bounded buffer; when that fills, the
//! oldest go. A refusal that a retry cannot fix drops the batch.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::StatusCode;
use temps_core::worker_shield::MAX_EVENTS_PER_UPLOAD;
use temps_shield::{EventSink, ShieldEvent};
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;
use tracing::{debug, warn};

/// How the uploader batches and retries.
#[derive(Debug, Clone)]
pub struct UploaderSettings {
    /// Post as soon as this many events wait.
    pub batch: usize,
    /// Post whatever waits this often.
    pub interval: Duration,
    /// Events between the ingress and the uploader task.
    pub queue: usize,
    /// Events kept while posts fail.
    pub buffer: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub timeout: Duration,
}

impl Default for UploaderSettings {
    fn default() -> Self {
        Self {
            batch: 500,
            interval: Duration::from_secs(2),
            queue: 4096,
            buffer: 10_000,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            timeout: Duration::from_secs(30),
        }
    }
}

/// The worker's [`EventSink`].
pub struct EventUploader {
    queue: mpsc::Sender<ShieldEvent>,
    dropped: Arc<AtomicU64>,
}

impl EventUploader {
    pub fn spawn(
        http: reqwest::Client,
        url: String,
        node_token: String,
        settings: UploaderSettings,
        shutdown: Arc<Notify>,
    ) -> Arc<Self> {
        let (queue, events) = mpsc::channel(settings.queue.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let task = UploadTask {
            http,
            url,
            node_token,
            settings,
            dropped: Arc::clone(&dropped),
            pending: VecDeque::new(),
        };
        tokio::spawn(task.run(events, shutdown));
        Arc::new(Self { queue, dropped })
    }

    /// Events dropped so far, at the queue or the buffer.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl EventSink for EventUploader {
    fn submit(&self, events: Vec<ShieldEvent>) {
        for event in events {
            if self.queue.try_send(event).is_err() {
                note_dropped(&self.dropped, 1, "the Shield event queue is full");
            }
        }
    }
}

/// Count drops, and say so on the first and every 1024th.
fn note_dropped(dropped: &AtomicU64, count: u64, why: &str) {
    let before = dropped.fetch_add(count, Ordering::Relaxed);
    if before == 0 || (before / 1024) != ((before + count) / 1024) {
        warn!(dropped = before + count, "dropping Shield events: {why}");
    }
}

enum Posted {
    Accepted,
    /// The control plane will not take this batch, now or later.
    Refused(StatusCode),
    /// Worth another try.
    Failed(String),
}

struct UploadTask {
    http: reqwest::Client,
    url: String,
    node_token: String,
    settings: UploaderSettings,
    dropped: Arc<AtomicU64>,
    pending: VecDeque<ShieldEvent>,
}

impl UploadTask {
    async fn run(mut self, mut events: mpsc::Receiver<ShieldEvent>, shutdown: Arc<Notify>) {
        // The first tick a whole interval from now, not at once.
        let mut ticker = tokio::time::interval_at(
            Instant::now() + self.settings.interval,
            self.settings.interval,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut backoff = self.settings.initial_backoff;
        let mut retry_at: Option<Instant> = None;
        loop {
            let flush = tokio::select! {
                _ = shutdown.notified() => {
                    // One last try, without waiting out a backoff.
                    while let Ok(event) = events.try_recv() {
                        self.push(event);
                    }
                    if !self.pending.is_empty() {
                        self.flush().await;
                    }
                    return;
                }
                received = events.recv() => match received {
                    Some(event) => {
                        self.push(event);
                        self.pending.len() >= self.settings.batch
                    }
                    None => {
                        if !self.pending.is_empty() {
                            self.flush().await;
                        }
                        return;
                    }
                },
                _ = ticker.tick() => !self.pending.is_empty(),
            };
            if !flush || retry_at.is_some_and(|at| Instant::now() < at) {
                continue;
            }
            if self.flush().await {
                backoff = self.settings.initial_backoff;
                retry_at = None;
            } else {
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(self.settings.max_backoff);
            }
        }
    }

    fn push(&mut self, event: ShieldEvent) {
        if self.pending.len() >= self.settings.buffer.max(1) {
            self.pending.pop_front();
            note_dropped(&self.dropped, 1, "the control plane is not taking them");
        }
        self.pending.push_back(event);
    }

    /// Post everything pending, a request at a time. `false` when a post
    /// failed and what is left waits for a retry.
    async fn flush(&mut self) -> bool {
        while !self.pending.is_empty() {
            let count = self.pending.len().min(MAX_EVENTS_PER_UPLOAD);
            let posted = match gzip_json(&self.pending.make_contiguous()[..count]) {
                Ok(body) => self.post(body).await,
                Err(error) => Posted::Failed(format!("encoding events: {error}")),
            };
            match posted {
                Posted::Accepted => {
                    debug!(count, "sent Shield events");
                    self.pending.drain(..count);
                }
                Posted::Refused(status) => {
                    warn!(count, %status, "the control plane refused Shield events; dropping them");
                    self.pending.drain(..count);
                    self.dropped.fetch_add(count as u64, Ordering::Relaxed);
                }
                Posted::Failed(error) => {
                    warn!(%error, waiting = self.pending.len(), "could not send Shield events");
                    return false;
                }
            }
        }
        true
    }

    async fn post(&self, body: Vec<u8>) -> Posted {
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.node_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::CONTENT_ENCODING, "gzip")
            .timeout(self.settings.timeout)
            .body(body)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => Posted::Accepted,
            Ok(response) => {
                let status = response.status();
                match status {
                    StatusCode::BAD_REQUEST
                    | StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::PAYLOAD_TOO_LARGE => Posted::Refused(status),
                    _ => Posted::Failed(format!("POST {} answered {status}", self.url)),
                }
            }
            Err(error) => Posted::Failed(format!("POST {}: {error}", self.url)),
        }
    }
}

pub(crate) fn gzip_json(events: &[ShieldEvent]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    serde_json::to_writer(&mut encoder, events)?;
    encoder.flush()?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use temps_shield::{EventKind, ShieldEvent, ShieldMode};
    use tokio::sync::Notify;

    use super::*;
    use crate::shield::tests::*;

    // --- Event uploader -------------------------------------------------------

    fn event(n: usize) -> ShieldEvent {
        ShieldEvent {
            timestamp_ms: 1_700_000_000_000 + n as i64,
            project_id: 1,
            environment_id: 2,
            node_id: None,
            request_id: format!("req-{n}"),
            kind: EventKind::Verdict,
            ip: CLEAN.to_string(),
            ja4: None,
            verdict: "block".to_string(),
            mode: ShieldMode::Selective.as_str().to_string(),
            reasons: vec!["test_block".to_string()],
            signature: None,
            host: HOST.to_string(),
            path: "/".to_string(),
            user_agent: BROWSER.to_string(),
            difficulty: None,
            solve_ms: None,
        }
    }

    fn uploader(url: &str, settings: UploaderSettings) -> (Arc<EventUploader>, Arc<Notify>) {
        let shutdown = Arc::new(Notify::new());
        let uploader = EventUploader::spawn(
            reqwest::Client::new(),
            format!("{url}/api/internal/nodes/7/shield/events"),
            "node-token".to_string(),
            settings,
            shutdown.clone(),
        );
        (uploader, shutdown)
    }

    async fn until(condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("condition not met in time");
    }

    fn quick(batch: usize, interval: Duration, backoff: Duration) -> UploaderSettings {
        UploaderSettings {
            batch,
            interval,
            initial_backoff: backoff,
            max_backoff: backoff,
            ..UploaderSettings::default()
        }
    }

    #[tokio::test]
    async fn a_full_batch_is_sent_at_once_and_the_rest_on_the_interval() {
        let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
        let settings = quick(3, Duration::from_millis(300), Duration::from_millis(20));
        let (uploader, _shutdown) = uploader(&url, settings);
        uploader.submit((0..4).map(event).collect());
        until(|| plane.received.lock().unwrap().len() == 4).await;
        assert_eq!(
            *plane.posts.lock().unwrap(),
            vec![(3, StatusCode::ACCEPTED), (1, StatusCode::ACCEPTED)],
            "the full batch at once, the rest on the interval"
        );
        assert_eq!(
            plane.received_ids(),
            vec!["req-0", "req-1", "req-2", "req-3"]
        );
        assert_eq!(uploader.dropped(), 0);
    }

    #[tokio::test]
    async fn a_failed_post_is_retried_and_a_refused_one_dropped() {
        let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
        *plane.statuses.lock().unwrap() = vec![StatusCode::SERVICE_UNAVAILABLE];
        let settings = quick(2, Duration::from_millis(30), Duration::from_millis(20));
        let (uploader, _shutdown) = uploader(&url, settings);
        uploader.submit((0..2).map(event).collect());
        until(|| plane.received.lock().unwrap().len() == 2).await;
        assert_eq!(
            plane.posts.lock().unwrap()[..2],
            [
                (2, StatusCode::SERVICE_UNAVAILABLE),
                (2, StatusCode::ACCEPTED)
            ]
        );

        *plane.statuses.lock().unwrap() = vec![StatusCode::BAD_REQUEST];
        uploader.submit((2..4).map(event).collect());
        until(|| uploader.dropped() == 2).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            plane.posts.lock().unwrap().len(),
            3,
            "a refusal is not retried"
        );
        assert_eq!(plane.received_ids(), vec!["req-0", "req-1"]);
    }

    #[tokio::test]
    async fn a_full_queue_drops_events_without_waiting() {
        let (_plane, url) = control_plane(stored_response(1, &keypair().1)).await;
        let settings = UploaderSettings {
            queue: 2,
            ..quick(500, Duration::from_secs(60), Duration::from_millis(20))
        };
        let (uploader, _shutdown) = uploader(&url, settings);
        // The test runtime has one thread: the uploader task does not run
        // while `submit` does.
        uploader.submit((0..5).map(event).collect());
        assert_eq!(uploader.dropped(), 3);
    }

    #[tokio::test]
    async fn the_buffer_keeps_the_newest_while_the_control_plane_is_down() {
        let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
        *plane.statuses.lock().unwrap() = vec![StatusCode::BAD_GATEWAY; 3];
        let settings = UploaderSettings {
            buffer: 3,
            ..quick(1, Duration::from_millis(20), Duration::from_millis(300))
        };
        let (uploader, _shutdown) = uploader(&url, settings);
        uploader.submit(vec![event(0)]);
        until(|| !plane.posts.lock().unwrap().is_empty()).await;
        // Within the backoff: four more into a buffer of three.
        uploader.submit((1..5).map(event).collect());
        until(|| plane.received.lock().unwrap().len() == 3).await;
        assert_eq!(plane.received_ids(), vec!["req-2", "req-3", "req-4"]);
        assert_eq!(uploader.dropped(), 2);
    }

    #[tokio::test]
    async fn shutdown_sends_what_waits() {
        let (plane, url) = control_plane(stored_response(1, &keypair().1)).await;
        let settings = quick(500, Duration::from_secs(60), Duration::from_millis(20));
        let (uploader, shutdown) = uploader(&url, settings);
        // Let the task reach its wait, past the interval's first tick.
        tokio::time::sleep(Duration::from_millis(20)).await;
        uploader.submit((0..2).map(event).collect());
        tokio::time::sleep(Duration::from_millis(20)).await;
        shutdown.notify_waiters();
        until(|| plane.received.lock().unwrap().len() == 2).await;
    }
}
