// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One pull per image per host, shared by everyone who needs that image.
//!
//! A sandbox image is gigabytes. Before this, a create pulled it inline and
//! two creates (or a create and an operator's "prepare") pulled it twice in
//! parallel. [`ImagePulls`] keys pulls by the exact image reference: the
//! first caller starts a detached task, every later caller joins it through
//! a `watch` channel, and the task runs to its end even when every caller
//! has stopped waiting (an HTTP client that gave up must not abort a
//! half-downloaded image).
//!
//! Memory: one entry per distinct image reference asked for, each a few
//! hundred bytes plus a map of the image's layers while it pulls; finished
//! entries beyond [`MAX_FINISHED_ENTRIES`] are dropped oldest first. A
//! running pull is never dropped, and the number of running pulls is bounded
//! by the number of distinct images callers ask for at once.

use std::collections::HashMap;
use std::future::Future;

use bollard::models::CreateImageInfo;
use chrono::Utc;
use tokio::sync::watch;

use crate::image_prepare::{ImagePrepareState, ImagePrepareStatus};

/// Finished pulls whose result is kept so a status read can still say why
/// the last attempt failed.
pub(crate) const MAX_FINISHED_ENTRIES: usize = 16;

/// How a pull job ended successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullOutcome {
    /// The image was downloaded (or built locally).
    Pulled,
    /// The image was already on the host.
    AlreadyPresent,
}

/// Every pull this host is running or ran recently, by image reference.
#[derive(Default)]
pub(crate) struct ImagePulls {
    entries: std::sync::Mutex<HashMap<String, watch::Receiver<ImagePrepareStatus>>>,
}

/// Handed to a pull job to publish its progress.
pub(crate) struct PullReporter {
    tx: watch::Sender<ImagePrepareStatus>,
    /// Per layer: (bytes downloaded, layer size). Only layers the registry
    /// reported a size for count towards the totals.
    layers: HashMap<String, (u64, u64)>,
}

impl PullReporter {
    /// Say what the pull is doing now.
    pub(crate) fn detail(&mut self, detail: &str) {
        self.tx
            .send_modify(|status| status.detail = Some(detail.to_string()));
    }

    /// Fold one event of the daemon's pull stream into the byte totals.
    ///
    /// The daemon reports per layer: `Downloading` with `current`/`total`,
    /// then `Download complete`, `Extracting`, `Pull complete`; a layer the
    /// host already had says `Already exists` and downloads nothing.
    pub(crate) fn on_pull_event(&mut self, info: &CreateImageInfo) {
        let (Some(layer), Some(status)) = (info.id.as_deref(), info.status.as_deref()) else {
            return;
        };
        let detail = info.progress_detail.as_ref();
        let current = detail.and_then(|d| d.current).and_then(non_negative);
        let total = detail.and_then(|d| d.total).and_then(non_negative);
        let changed = match status {
            "Downloading" => match (current, total) {
                (Some(current), Some(total)) if total > 0 => {
                    self.layers
                        .insert(layer.to_string(), (current.min(total), total));
                    true
                }
                _ => false,
            },
            "Download complete" | "Verifying Checksum" | "Pull complete" => {
                match self.layers.get_mut(layer) {
                    Some((done, total)) if *done != *total => {
                        *done = *total;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        let extracting = status == "Extracting";
        if !changed && !extracting && status != "Downloading" {
            return;
        }
        let (done, total) = self
            .layers
            .values()
            .fold((0u64, 0u64), |(d, t), (ld, lt)| (d + ld, t + lt));
        // The daemon extracts finished layers while others still download:
        // say "extracting" only once nothing is left to download.
        let phase = if extracting && done >= total {
            Some("extracting")
        } else if total > done || status == "Downloading" {
            Some("downloading")
        } else {
            None
        };
        self.tx.send_modify(|s| {
            if let Some(phase) = phase {
                s.detail = Some(phase.to_string());
            }
            if total > 0 {
                s.bytes_done = Some(done);
                s.bytes_total = Some(total);
            }
        });
    }
}

fn non_negative(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

impl ImagePulls {
    /// Join the running pull of `image`, or start one running `job`.
    /// Returns the pull's status channel and whether this call started it.
    pub(crate) fn start<F, Fut>(
        &self,
        image: &str,
        job: F,
    ) -> (watch::Receiver<ImagePrepareStatus>, bool)
    where
        F: FnOnce(PullReporter) -> Fut,
        Fut: Future<Output = Result<PullOutcome, String>> + Send + 'static,
    {
        let mut entries = self.lock();
        if let Some(rx) = entries.get(image) {
            if rx.borrow().state == ImagePrepareState::Pulling {
                return (rx.clone(), false);
            }
        }
        let initial = ImagePrepareStatus {
            started_at: Some(Utc::now()),
            ..ImagePrepareStatus::new(image, ImagePrepareState::Pulling)
        };
        let (tx, rx) = watch::channel(initial);
        prune_finished(&mut entries);
        entries.insert(image.to_string(), rx.clone());
        drop(entries);

        let job = job(PullReporter {
            tx: tx.clone(),
            layers: HashMap::new(),
        });
        let image = image.to_string();
        tokio::spawn(async move {
            // A second task, so a panicking job still ends the pull with a
            // terminal state instead of leaving waiters stuck on "pulling".
            let result = match tokio::spawn(job).await {
                Ok(result) => result,
                Err(join_error) => Err(format!(
                    "the pull of image {image} stopped unexpectedly: {join_error}"
                )),
            };
            match &result {
                Ok(outcome) => {
                    tracing::info!(image = %image, outcome = ?outcome, "sandbox image ready")
                }
                Err(reason) => {
                    tracing::warn!(image = %image, reason = %reason, "sandbox image pull failed")
                }
            }
            tx.send_modify(|status| {
                status.finished_at = Some(Utc::now());
                status.detail = None;
                match result {
                    Ok(outcome) => {
                        status.state = ImagePrepareState::Ready;
                        status.already_present = outcome == PullOutcome::AlreadyPresent;
                        status.error = None;
                        if let Some(total) = status.bytes_total {
                            status.bytes_done = Some(total);
                        }
                    }
                    Err(reason) => {
                        status.state = ImagePrepareState::Failed;
                        status.error = Some(reason);
                    }
                }
            });
        });
        (rx, true)
    }

    /// The running or last finished pull of `image`, if this process ran
    /// one recently.
    pub(crate) fn current(&self, image: &str) -> Option<ImagePrepareStatus> {
        self.lock().get(image).map(|rx| rx.borrow().clone())
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, watch::Receiver<ImagePrepareStatus>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Drop the oldest finished entries so at most [`MAX_FINISHED_ENTRIES`]
/// remain (plus the one about to be inserted). Running pulls stay.
fn prune_finished(entries: &mut HashMap<String, watch::Receiver<ImagePrepareStatus>>) {
    let mut finished: Vec<(String, Option<chrono::DateTime<Utc>>)> = entries
        .iter()
        .filter_map(|(image, rx)| {
            let status = rx.borrow();
            status
                .state
                .is_terminal()
                .then(|| (image.clone(), status.finished_at))
        })
        .collect();
    if finished.len() < MAX_FINISHED_ENTRIES {
        return;
    }
    finished.sort_by_key(|(_, finished_at)| *finished_at);
    let excess = finished.len() + 1 - MAX_FINISHED_ENTRIES;
    for (image, _) in finished.into_iter().take(excess) {
        entries.remove(&image);
    }
}

/// Wait for a pull to finish, calling `on_update` with every status it
/// publishes on the way. A pull task that vanished without a result is
/// reported as failed.
pub(crate) async fn wait_for_pull(
    mut rx: watch::Receiver<ImagePrepareStatus>,
    mut on_update: impl FnMut(&ImagePrepareStatus),
) -> ImagePrepareStatus {
    loop {
        let status = rx.borrow_and_update().clone();
        on_update(&status);
        if status.state.is_terminal() {
            return status;
        }
        if rx.changed().await.is_err() {
            let mut last = rx.borrow().clone();
            if !last.state.is_terminal() {
                last.state = ImagePrepareState::Failed;
                last.error = Some(format!(
                    "the pull of image {} ended without reporting a result",
                    last.image
                ));
            }
            return last;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ProgressDetail;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::oneshot;

    fn event(
        layer: &str,
        status: &str,
        current: Option<i64>,
        total: Option<i64>,
    ) -> CreateImageInfo {
        CreateImageInfo {
            id: Some(layer.to_string()),
            status: Some(status.to_string()),
            progress_detail: Some(ProgressDetail { current, total }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_pull_goes_from_pulling_to_ready_with_its_byte_totals() {
        let pulls = ImagePulls::default();
        let (release, released) = oneshot::channel::<()>();
        let (rx, started) = pulls.start("img:1", |mut reporter| async move {
            reporter.on_pull_event(&event("a", "Downloading", Some(10), Some(100)));
            reporter.on_pull_event(&event("b", "Downloading", Some(5), Some(50)));
            let _ = released.await;
            reporter.on_pull_event(&event("a", "Download complete", None, None));
            Ok(PullOutcome::Pulled)
        });
        assert!(started);
        let pulling = pulls.current("img:1").unwrap();
        assert_eq!(pulling.state, ImagePrepareState::Pulling);
        assert!(pulling.started_at.is_some() && pulling.finished_at.is_none());

        // Progress shows up while the job is held.
        let mut progress_rx = rx.clone();
        progress_rx
            .wait_for(|s| s.bytes_total == Some(150))
            .await
            .unwrap();
        assert_eq!(progress_rx.borrow().bytes_done, Some(15));
        assert_eq!(progress_rx.borrow().detail.as_deref(), Some("downloading"));

        release.send(()).unwrap();
        let done = wait_for_pull(rx, |_| {}).await;
        assert_eq!(done.state, ImagePrepareState::Ready);
        assert_eq!(
            done.bytes_done,
            Some(150),
            "a finished pull reports every byte"
        );
        assert!(done.finished_at.is_some());
        assert!(!done.already_present);
        assert_eq!(done.detail, None);
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_pull() {
        let pulls = Arc::new(ImagePulls::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let (release, released) = oneshot::channel::<()>();
        let job_runs = Arc::clone(&runs);
        let (first, started_first) = pulls.start("img:shared", move |_| async move {
            job_runs.fetch_add(1, Ordering::SeqCst);
            let _ = released.await;
            Ok(PullOutcome::Pulled)
        });
        // A create arriving while the prepare pulls joins it.
        let second_runs = Arc::clone(&runs);
        let (second, started_second) = pulls.start("img:shared", move |_| async move {
            second_runs.fetch_add(1, Ordering::SeqCst);
            Ok(PullOutcome::Pulled)
        });
        assert!(started_first);
        assert!(!started_second, "the second caller must join, not start");

        release.send(()).unwrap();
        let (a, b) = tokio::join!(wait_for_pull(first, |_| {}), wait_for_pull(second, |_| {}));
        assert_eq!(a.state, ImagePrepareState::Ready);
        assert_eq!(b.state, ImagePrepareState::Ready);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "exactly one pull ran");
    }

    #[tokio::test]
    async fn a_failed_pull_keeps_its_reason_and_a_retry_starts_a_new_pull() {
        let pulls = ImagePulls::default();
        let (rx, _) = pulls.start("img:bad", |_| async {
            Err("Failed to pull image img:bad: manifest unknown".to_string())
        });
        let failed = wait_for_pull(rx, |_| {}).await;
        assert_eq!(failed.state, ImagePrepareState::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some("Failed to pull image img:bad: manifest unknown")
        );
        // The reason survives for a later status read.
        assert_eq!(
            pulls.current("img:bad").unwrap().state,
            ImagePrepareState::Failed
        );

        let (rx, started) = pulls.start("img:bad", |_| async { Ok(PullOutcome::AlreadyPresent) });
        assert!(started, "a finished pull does not block a retry");
        let ready = wait_for_pull(rx, |_| {}).await;
        assert_eq!(ready.state, ImagePrepareState::Ready);
        assert!(ready.already_present);
        assert_eq!(ready.error, None);
    }

    #[tokio::test]
    async fn a_panicking_job_ends_the_pull_as_failed() {
        let pulls = ImagePulls::default();
        let (rx, _) = pulls.start("img:panic", |_| async {
            if true {
                panic!("boom");
            }
            Ok(PullOutcome::Pulled)
        });
        let status = wait_for_pull(rx, |_| {}).await;
        assert_eq!(status.state, ImagePrepareState::Failed);
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("img:panic"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn the_pull_outlives_every_caller() {
        let pulls = ImagePulls::default();
        let (release, released) = oneshot::channel::<()>();
        let (rx, _) = pulls.start("img:detached", |_| async move {
            let _ = released.await;
            Ok(PullOutcome::Pulled)
        });
        drop(rx);
        release.send(()).unwrap();
        for _ in 0..100 {
            if pulls.current("img:detached").unwrap().state == ImagePrepareState::Ready {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the pull did not finish after its caller went away");
    }

    #[tokio::test]
    async fn finished_entries_are_bounded_and_running_ones_kept() {
        let pulls = ImagePulls::default();
        let (_hold, held) = oneshot::channel::<()>();
        let (running, _) = pulls.start("img:running", |_| async move {
            let _ = held.await;
            Ok(PullOutcome::Pulled)
        });
        for i in 0..(MAX_FINISHED_ENTRIES * 2) {
            let (rx, _) = pulls.start(&format!("img:{i}"), |_| async { Ok(PullOutcome::Pulled) });
            wait_for_pull(rx, |_| {}).await;
        }
        let entries = pulls.lock();
        let finished = entries
            .values()
            .filter(|rx| rx.borrow().state.is_terminal())
            .count();
        assert!(
            finished <= MAX_FINISHED_ENTRIES,
            "{finished} finished entries kept"
        );
        assert!(entries.contains_key("img:running"));
        drop(entries);
        drop(running);
    }

    #[test]
    fn layer_events_without_sizes_do_not_invent_totals() {
        let (tx, rx) = watch::channel(ImagePrepareStatus::new("img", ImagePrepareState::Pulling));
        let mut reporter = PullReporter {
            tx,
            layers: HashMap::new(),
        };
        reporter.on_pull_event(&event("a", "Already exists", None, None));
        reporter.on_pull_event(&event("a", "Pulling fs layer", None, None));
        assert_eq!(rx.borrow().bytes_total, None);
        reporter.on_pull_event(&event("b", "Downloading", Some(-1), Some(10)));
        assert_eq!(rx.borrow().bytes_total, None, "a negative size is ignored");
        reporter.on_pull_event(&event("b", "Extracting", Some(3), Some(10)));
        assert_eq!(rx.borrow().detail.as_deref(), Some("extracting"));
    }

    #[test]
    fn extracting_is_reported_only_once_nothing_is_left_to_download() {
        let (tx, rx) = watch::channel(ImagePrepareStatus::new("img", ImagePrepareState::Pulling));
        let mut reporter = PullReporter {
            tx,
            layers: HashMap::new(),
        };
        reporter.on_pull_event(&event("a", "Downloading", Some(5), Some(10)));
        reporter.on_pull_event(&event("b", "Downloading", Some(1), Some(100)));
        reporter.on_pull_event(&event("a", "Download complete", None, None));
        // Layer a extracts while b is still downloading.
        reporter.on_pull_event(&event("a", "Extracting", Some(4), Some(10)));
        assert_eq!(rx.borrow().detail.as_deref(), Some("downloading"));
        assert_eq!(rx.borrow().bytes_done, Some(11));
        reporter.on_pull_event(&event("b", "Download complete", None, None));
        reporter.on_pull_event(&event("b", "Extracting", Some(50), Some(100)));
        assert_eq!(rx.borrow().detail.as_deref(), Some("extracting"));
        assert_eq!(rx.borrow().bytes_done, Some(110));
        assert_eq!(rx.borrow().bytes_total, Some(110));
    }
}

#[cfg(test)]
mod provider_tests {
    //! The Docker provider's prepare/status/remove against a fake Engine API.

    use super::super::{DockerSandboxConfig, DockerSandboxProvider};
    use crate::{ImagePrepareState, ImageRemoval, SandboxBackend, SandboxProvider};
    use serde_json::json;
    use std::sync::Arc;
    use temps_docker_capture::{FakeDocker, Method};

    const IMAGE: &str = "registry.invalid/prepare-test:1";

    async fn provider() -> (FakeDocker, DockerSandboxProvider) {
        let fake = FakeDocker::start()
            .await
            .expect("start the fake Engine API");
        let provider =
            DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default());
        (fake, provider)
    }

    fn pulls(fake: &FakeDocker) -> usize {
        fake.calls()
            .iter()
            .filter(|c| c.method == "POST" && c.path == "/images/create")
            .count()
    }

    #[tokio::test]
    async fn not_pulled_then_pulling_then_ready() {
        let (fake, provider) = provider().await;
        let before = provider
            .image_prepare_status(Some(IMAGE), None)
            .await
            .unwrap();
        assert_eq!(before.state, ImagePrepareState::NotPulled);
        assert_eq!(before.image, IMAGE);

        let started = provider.prepare_image(Some(IMAGE), None).await.unwrap();
        assert_eq!(started.state, ImagePrepareState::Pulling);
        assert!(started.started_at.is_some());

        let (rx, _) = provider.start_image_pull(IMAGE);
        let done = super::wait_for_pull(rx, |_| {}).await;
        assert_eq!(done.state, ImagePrepareState::Ready, "{done:?}");
        assert_eq!(
            pulls(&fake),
            1,
            "the status read and the join pulled nothing more"
        );

        // From now on the daemon has it: the status says so.
        fake.respond(Method::GET, "/images/", 200, json!({ "Id": "sha256:abc" }));
        let after = provider
            .image_prepare_status(Some(IMAGE), None)
            .await
            .unwrap();
        assert_eq!(after.state, ImagePrepareState::Ready);
        assert!(
            after.finished_at.is_some(),
            "the finished pull's times are kept"
        );
    }

    #[tokio::test]
    async fn an_image_already_on_the_host_is_ready_without_a_pull() {
        let (fake, provider) = provider().await;
        fake.respond(Method::GET, "/images/", 200, json!({ "Id": "sha256:abc" }));
        let (rx, _) = provider.start_image_pull(IMAGE);
        let done = super::wait_for_pull(rx, |_| {}).await;
        assert_eq!(done.state, ImagePrepareState::Ready);
        assert!(done.already_present);
        assert_eq!(pulls(&fake), 0);
    }

    #[tokio::test]
    async fn using_an_image_already_present_keeps_the_last_pull_status() {
        let (fake, provider) = provider().await;
        // A real pull: the image is missing, the registry delivers it.
        let (rx, _) = provider.start_image_pull(IMAGE);
        let pulled = super::wait_for_pull(rx, |_| {}).await;
        assert_eq!(pulled.state, ImagePrepareState::Ready, "{pulled:?}");
        assert!(!pulled.already_present);

        // A later create (or the egress proxy) finds the image on the host.
        fake.respond(Method::GET, "/images/", 200, json!({ "Id": "sha256:abc" }));
        let status = provider.ensure_image_present(IMAGE, |_| {}).await;
        assert_eq!(status.state, ImagePrepareState::Ready);
        assert!(status.already_present);

        assert_eq!(pulls(&fake), 1, "nothing was pulled again");
        let tracked = provider.image_pulls.current(IMAGE).expect("tracked pull");
        assert_eq!(tracked, pulled, "the operator still sees the real pull");
    }

    #[tokio::test]
    async fn a_failed_pull_surfaces_the_image_and_the_registry_reason() {
        let (fake, provider) = provider().await;
        fake.respond(
            Method::POST,
            "/images/create",
            500,
            json!({ "message": "manifest unknown" }),
        );
        provider.prepare_image(Some(IMAGE), None).await.unwrap();
        let (rx, _) = provider.start_image_pull(IMAGE);
        let failed = super::wait_for_pull(rx, |_| {}).await;
        // A failed pull is retried by the next start, so this may be the
        // retry's result: both fail the same way.
        assert_eq!(failed.state, ImagePrepareState::Failed);
        let reason = failed.error.unwrap_or_default();
        assert!(
            reason.contains(IMAGE) && reason.contains("manifest unknown"),
            "{reason}"
        );

        let status = provider
            .image_prepare_status(Some(IMAGE), None)
            .await
            .unwrap();
        assert_eq!(status.state, ImagePrepareState::Failed);
        assert!(status
            .error
            .unwrap_or_default()
            .contains("manifest unknown"));
    }

    #[tokio::test]
    async fn the_default_image_is_the_one_a_create_would_use() {
        let (_fake, provider) = provider().await;
        let status = provider.image_prepare_status(None, None).await.unwrap();
        assert_eq!(status.image, provider.config.resolved_image());
    }

    #[tokio::test]
    async fn a_microvm_backend_has_nothing_to_pull() {
        let (fake, provider) = provider().await;
        let status = provider
            .prepare_image(None, Some(SandboxBackend::Firecracker))
            .await
            .unwrap();
        assert_eq!(status.state, ImagePrepareState::Unsupported);
        assert!(status.error.unwrap_or_default().contains("firecracker"));
        assert_eq!(pulls(&fake), 0);
    }

    #[tokio::test]
    async fn removing_a_prepared_image_is_never_forced() {
        let (fake, provider) = provider().await;
        fake.respond(
            Method::DELETE,
            "/images/",
            409,
            json!({ "message": "image is being used by stopped container abc" }),
        );
        let outcome = provider.remove_prepared_image(IMAGE).await.unwrap();
        assert_eq!(outcome, ImageRemoval::InUse);
        let delete = fake
            .calls()
            .into_iter()
            .find(|c| c.method == "DELETE")
            .expect("a delete was sent");
        assert!(!delete.query.contains("force=true"), "{}", delete.query);

        fake.respond(
            Method::DELETE,
            "/images/",
            404,
            json!({ "message": "no such image" }),
        );
        assert_eq!(
            provider.remove_prepared_image(IMAGE).await.unwrap(),
            ImageRemoval::NotPresent
        );
        fake.respond(
            Method::DELETE,
            "/images/",
            200,
            json!([{ "Untagged": IMAGE }]),
        );
        assert_eq!(
            provider.remove_prepared_image(IMAGE).await.unwrap(),
            ImageRemoval::Removed
        );
    }
}
