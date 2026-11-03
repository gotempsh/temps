// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pull an image by draining the daemon's progress stream.
//!
//! A pull is only finished when its stream ends, and the first error ends it.
//! That much is the same for every caller. What differs is explicit in
//! [`PullSpec`]: a separate tag, registry credentials, and whether the wait is
//! bounded at all. Retries and "use the local copy if the registry is down"
//! are the caller's policy and stay with the caller.

use std::time::Duration;

use bollard::auth::DockerCredentials;
use bollard::errors::Error as DockerError;
use bollard::models::CreateImageInfo;
use bollard::query_parameters::CreateImageOptions;
use bollard::Docker;
use futures::{Stream, StreamExt};
use tokio::time::Instant;

use crate::error::DockerUtilError;

/// Bounds on the wait for one pull. Without them a registry that accepted the
/// connection and went quiet holds the caller forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullTimeouts {
    /// Longest the daemon may report no progress.
    pub idle: Duration,
    /// Ceiling on the whole pull, however much progress it reports.
    pub total: Duration,
}

/// What to pull, and how.
#[derive(Debug, Clone)]
pub struct PullSpec<'a> {
    reference: &'a str,
    tag: Option<&'a str>,
    credentials: Option<DockerCredentials>,
    timeouts: Option<PullTimeouts>,
}

impl<'a> PullSpec<'a> {
    /// Pull `reference` as given (`mongo:8`, `repo@sha256:…`), anonymously and
    /// with no bound on the wait.
    pub fn new(reference: &'a str) -> Self {
        Self {
            reference,
            tag: None,
            credentials: None,
            timeouts: None,
        }
    }

    /// Send the tag separately from the image name.
    #[must_use]
    pub fn tag(mut self, tag: Option<&'a str>) -> Self {
        self.tag = tag;
        self
    }

    /// Authenticate to the registry.
    #[must_use]
    pub fn credentials(mut self, credentials: Option<DockerCredentials>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Give up when the pull goes quiet or runs too long.
    #[must_use]
    pub fn timeouts(mut self, timeouts: PullTimeouts) -> Self {
        self.timeouts = Some(timeouts);
        self
    }
}

/// Pull an image, discarding progress. See [`pull_image_with_progress`].
pub async fn pull_image(docker: &Docker, spec: PullSpec<'_>) -> Result<(), DockerUtilError> {
    pull_image_with_progress(docker, spec, |_| {}).await
}

/// Pull an image, calling `on_progress` for every event the daemon reports.
/// Progress events are not kept, so memory does not depend on the image.
pub async fn pull_image_with_progress<F>(
    docker: &Docker,
    spec: PullSpec<'_>,
    on_progress: F,
) -> Result<(), DockerUtilError>
where
    F: FnMut(&CreateImageInfo),
{
    let PullSpec {
        reference,
        tag,
        credentials,
        timeouts,
    } = spec;
    let stream = docker.create_image(
        Some(CreateImageOptions {
            from_image: Some(reference.to_string()),
            tag: tag.map(str::to_string),
            ..Default::default()
        }),
        None,
        credentials,
    );
    drain_pull_stream(Box::pin(stream), timeouts, on_progress)
        .await
        .map_err(|interrupted| match interrupted {
            DrainInterrupted::Docker(source) => DockerUtilError::PullFailed {
                reference: reference.to_string(),
                source,
            },
            DrainInterrupted::Stalled { idle_secs } => DockerUtilError::PullStalled {
                reference: reference.to_string(),
                idle_secs,
            },
            DrainInterrupted::TimedOut { total_secs } => DockerUtilError::PullTimedOut {
                reference: reference.to_string(),
                total_secs,
            },
        })
}

/// Why draining a pull stream stopped before its end.
#[derive(Debug)]
enum DrainInterrupted {
    Docker(DockerError),
    Stalled { idle_secs: u64 },
    TimedOut { total_secs: u64 },
}

/// Drain a pull stream to its end. With `timeouts`, give up when it goes quiet
/// for `idle` or runs for longer than `total`.
async fn drain_pull_stream<S, T, F>(
    mut stream: S,
    timeouts: Option<PullTimeouts>,
    mut on_item: F,
) -> Result<(), DrainInterrupted>
where
    S: Stream<Item = Result<T, DockerError>> + Unpin,
    F: FnMut(&T),
{
    let Some(PullTimeouts { idle, total }) = timeouts else {
        while let Some(item) = stream.next().await {
            on_item(&item.map_err(DrainInterrupted::Docker)?);
        }
        return Ok(());
    };

    let deadline = Instant::now() + total;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DrainInterrupted::TimedOut {
                total_secs: total.as_secs(),
            });
        }
        let wait = idle.min(remaining);
        match tokio::time::timeout(wait, stream.next()).await {
            Ok(None) => return Ok(()),
            Ok(Some(Ok(item))) => on_item(&item),
            Ok(Some(Err(e))) => return Err(DrainInterrupted::Docker(e)),
            // The overall ceiling and the idle limit use one clock; report
            // whichever of the two actually ran out.
            Err(_) if remaining <= idle => {
                return Err(DrainInterrupted::TimedOut {
                    total_secs: total.as_secs(),
                })
            }
            Err(_) => {
                return Err(DrainInterrupted::Stalled {
                    idle_secs: idle.as_secs(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{docker_or_skip, unique_name, TEST_IMAGE};

    fn docker_404() -> DockerError {
        DockerError::DockerResponseServerError {
            status_code: 404,
            message: "manifest unknown".to_string(),
        }
    }

    fn bounded(idle: Duration, total: Duration) -> Option<PullTimeouts> {
        Some(PullTimeouts { idle, total })
    }

    #[tokio::test]
    async fn drain_succeeds_when_the_stream_ends() {
        let stream = futures::stream::iter(vec![Ok::<(), DockerError>(())]);
        let bounds = bounded(Duration::from_secs(5), Duration::from_secs(5));
        let result = drain_pull_stream(stream, bounds, |_| {}).await;
        assert!(result.is_ok(), "a finished pull must succeed: {result:?}");
    }

    #[tokio::test]
    async fn drain_reports_a_stalled_pull_instead_of_waiting_forever() {
        // One progress event, then the daemon goes silent without closing.
        let stream = futures::stream::iter(vec![Ok(())]).chain(futures::stream::pending());
        let started = std::time::Instant::now();

        let result = drain_pull_stream(
            Box::pin(stream),
            bounded(Duration::from_millis(100), Duration::from_secs(30)),
            |_| {},
        )
        .await;

        assert!(
            matches!(result, Err(DrainInterrupted::Stalled { .. })),
            "a silent stream must be reported as stalled: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the stall must be detected at the idle limit, not the overall one"
        );
    }

    #[tokio::test]
    async fn drain_caps_a_pull_that_keeps_reporting_progress() {
        // Events arrive faster than the idle limit, so only the overall
        // ceiling can stop this pull.
        let stream = futures::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((Ok::<(), DockerError>(()), ()))
        });

        let result = drain_pull_stream(
            Box::pin(stream),
            bounded(Duration::from_secs(5), Duration::from_millis(200)),
            |_| {},
        )
        .await;

        assert!(
            matches!(result, Err(DrainInterrupted::TimedOut { .. })),
            "an endless trickle of progress must hit the overall ceiling: {result:?}"
        );
    }

    #[tokio::test]
    async fn drain_stops_at_the_daemons_first_error() {
        let stream = futures::stream::iter(vec![Ok(()), Err(docker_404()), Ok(())]);
        let mut events = 0;
        let error = drain_pull_stream(stream, None, |_| events += 1)
            .await
            .expect_err("a pull error must be returned");
        assert!(matches!(error, DrainInterrupted::Docker(_)), "{error:?}");
        assert_eq!(events, 1, "nothing is read past the first error");
    }

    #[tokio::test]
    async fn drain_without_timeouts_waits_for_a_quiet_stream() {
        // Quiet for longer than any bound the other tests use, then finished.
        let stream = futures::stream::once(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok::<(), DockerError>(())
        });
        let mut events = 0;
        drain_pull_stream(Box::pin(stream), None, |_| events += 1)
            .await
            .expect("an unbounded pull waits");
        assert_eq!(events, 1);
    }

    #[tokio::test]
    async fn pulling_a_real_image_reports_progress() {
        let Some(docker) = docker_or_skip("pulling_a_real_image_reports_progress").await else {
            return;
        };
        let mut events = 0;
        pull_image_with_progress(&docker, PullSpec::new(TEST_IMAGE), |_| events += 1)
            .await
            .expect("pull the test image");
        assert!(events > 0, "the daemon reports at least a status line");
    }

    #[tokio::test]
    async fn a_missing_image_is_a_typed_error_naming_the_image() {
        let Some(docker) = docker_or_skip("a_missing_image_is_a_typed_error").await else {
            return;
        };
        // A local-looking registry name that resolves nowhere fails fast and
        // never touches a real registry.
        let reference = format!("127.0.0.1:1/{}:nope", unique_name("missing"));
        let error = pull_image(&docker, PullSpec::new(&reference))
            .await
            .expect_err("an image that does not exist cannot be pulled");
        match &error {
            DockerUtilError::PullFailed { reference: got, .. } => assert_eq!(got, &reference),
            other => panic!("expected PullFailed, got {other:?}"),
        }
        assert!(error.to_string().contains(&reference), "{error}");
        assert!(error.into_docker_source().is_ok());
    }
}
