// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preparing a host for sandboxes ahead of the first create, and reporting
//! what a create is doing while it runs.
//!
//! **Image preparation.** The first sandbox on a host used to pull its image
//! (gigabytes) inside the create request. A provider that can pull ahead of
//! time answers [`SandboxProvider::prepare_image`](crate::SandboxProvider::prepare_image)
//! and [`SandboxProvider::image_prepare_status`](crate::SandboxProvider::image_prepare_status)
//! with an [`ImagePrepareStatus`]. The image is resolved exactly as a create
//! resolves it (the provider's pinned default when none is named), and a
//! create that starts while that pull runs waits on it instead of starting a
//! second one. These types travel between the control plane and worker
//! nodes, so they are serialisable and documented in the OpenAPI schema.
//!
//! **Create phases.** A provider reports the steps of a create through
//! [`report_create_phase`], read from a task-local sink the caller scopes
//! around the create with [`with_create_progress`]. A task-local rather than
//! a field on `SandboxCreateConfig` keeps every existing create call
//! unchanged; a provider that runs a step on another task simply does not
//! report it, and the caller falls back to marking the steps done when the
//! create returns.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where one host stands with one sandbox image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImagePrepareState {
    /// The image is not on the host and nothing is pulling it.
    NotPulled,
    /// A pull (or, for a preset image whose pull failed, a local build) is
    /// running. Creates of this image wait for it.
    Pulling,
    /// The image is on the host: a create starts without downloading it.
    Ready,
    /// The last attempt failed; `error` says why. A new prepare retries.
    Failed,
    /// This host's sandbox backend has no image to pull (a microVM backend,
    /// or a worker whose agent predates image preparation); `error` says why.
    Unsupported,
}

impl ImagePrepareState {
    /// Whether the state can still change without a new request.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pulling)
    }
}

/// One host's status for one sandbox image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImagePrepareStatus {
    /// The exact reference a create on this host would use (tag or
    /// `@sha256:` digest), so the pre-pull and the create cannot disagree.
    pub image: String,
    pub state: ImagePrepareState,
    /// What a running pull is doing right now (`downloading`, `extracting`,
    /// `building locally`).
    #[serde(default)]
    pub detail: Option<String>,
    /// Bytes downloaded so far, summed over the layers the registry
    /// reported. `None` before the registry reports any size.
    #[serde(default)]
    pub bytes_done: Option<u64>,
    /// Total bytes to download, summed over the layers reported so far (it
    /// grows while the registry announces layers).
    #[serde(default)]
    pub bytes_total: Option<u64>,
    /// Why the last attempt failed, or why the host cannot prepare images.
    #[serde(default)]
    pub error: Option<String>,
    /// When the running or last attempt started.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DateTime<Utc>>,
    /// When the last attempt finished.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
    /// The image was already on the host when the attempt started: nothing
    /// was downloaded.
    #[serde(default)]
    pub already_present: bool,
}

impl ImagePrepareStatus {
    /// A status with nothing but the image and its state.
    pub fn new(image: impl Into<String>, state: ImagePrepareState) -> Self {
        Self {
            image: image.into(),
            state,
            detail: None,
            bytes_done: None,
            bytes_total: None,
            error: None,
            started_at: None,
            finished_at: None,
            already_present: false,
        }
    }

    /// The host cannot prepare this image, and why.
    pub fn unsupported(image: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            error: Some(reason.into()),
            ..Self::new(image, ImagePrepareState::Unsupported)
        }
    }
}

/// What removing a previously prepared image did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageRemoval {
    Removed,
    /// The image was already gone.
    NotPresent,
    /// A container still uses it (or it is being pulled); it was left alone.
    InUse,
    /// This host's backend has no images to remove.
    Unsupported,
}

/// One step a provider performs inside `create`, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CreatePhase {
    /// Making sure the image is on the host.
    Image,
    /// Creating the sandbox's isolated network and attaching the preview
    /// gateway to it, then creating the container.
    CreateContainer,
    /// Starting the container.
    StartContainer,
    /// Fixing workspace ownership and waiting for the in-sandbox runtime to
    /// answer its health check.
    WaitReady,
}

/// A phase starting or finishing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreatePhaseEvent {
    Started(CreatePhase),
    Finished(CreatePhase),
    /// The image step had nothing to download.
    ImageAlreadyPresent,
    /// Download progress of the image step.
    ImageProgress(ImagePrepareStatus),
}

/// Receives the phases of the create running on the current task.
pub type CreateProgressSink = Arc<dyn Fn(CreatePhaseEvent) + Send + Sync>;

tokio::task_local! {
    static CREATE_PROGRESS: CreateProgressSink;
}

/// Run `future` with `sink` receiving every [`CreatePhaseEvent`] a provider
/// reports on this task.
pub async fn with_create_progress<F: std::future::Future>(
    sink: CreateProgressSink,
    future: F,
) -> F::Output {
    CREATE_PROGRESS.scope(sink, future).await
}

/// Report a create phase to the sink scoped around the current create, if
/// any. A no-op outside [`with_create_progress`].
pub fn report_create_phase(event: CreatePhaseEvent) {
    let _ = CREATE_PROGRESS.try_with(|sink| sink(event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn phases_reach_the_scoped_sink_and_nothing_else() {
        // Outside a scope: silently dropped.
        report_create_phase(CreatePhaseEvent::Started(CreatePhase::Image));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: CreateProgressSink = Arc::new(move |event| {
            sink_seen.lock().unwrap().push(event);
        });
        with_create_progress(sink, async {
            report_create_phase(CreatePhaseEvent::Started(CreatePhase::CreateContainer));
            tokio::task::yield_now().await;
            report_create_phase(CreatePhaseEvent::Finished(CreatePhase::CreateContainer));
        })
        .await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                CreatePhaseEvent::Started(CreatePhase::CreateContainer),
                CreatePhaseEvent::Finished(CreatePhase::CreateContainer),
            ]
        );
    }

    #[test]
    fn only_pulling_is_not_terminal() {
        for state in [
            ImagePrepareState::NotPulled,
            ImagePrepareState::Ready,
            ImagePrepareState::Failed,
            ImagePrepareState::Unsupported,
        ] {
            assert!(state.is_terminal(), "{state:?}");
        }
        assert!(!ImagePrepareState::Pulling.is_terminal());
    }

    #[test]
    fn status_round_trips_and_tolerates_missing_optional_fields() {
        let status: ImagePrepareStatus =
            serde_json::from_str(r#"{"image":"img:1","state":"ready"}"#).unwrap();
        assert_eq!(
            status,
            ImagePrepareStatus::new("img:1", ImagePrepareState::Ready)
        );
        let json = serde_json::to_value(ImagePrepareStatus::unsupported("img:1", "why")).unwrap();
        assert_eq!(json["state"], "unsupported");
        assert_eq!(json["error"], "why");
    }
}
