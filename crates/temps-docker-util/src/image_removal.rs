// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing images only when nothing on the daemon still uses them.
//!
//! Callers that clean up images they built (a deleted project's deployment
//! images, an orphan sweep) all need the same guard: an image a container
//! still runs from, or was created from and could be restarted from, is left
//! alone whatever its tag says. A container records the image *ID* it was
//! created from, so a check by tag alone would miss a container started from
//! an image that was retagged since.
//!
//! Each request is checked on its own, so the work and memory of a call are
//! bounded by the batch, not by how many images or containers the daemon has:
//!
//! 1. the reference is inspected, which gives the image ID it names now and
//!    every tag on that image;
//! 2. it is kept if that ID is not the one the caller recorded, or the image
//!    carries any tag the caller did not ask to remove;
//! 3. it is kept if any container, running or stopped, was created from that
//!    image ID (the daemon's `ancestor` filter, which also covers containers
//!    of images built on top of it);
//! 4. otherwise the image is removed **by that ID**, never forced.
//!
//! Removing by the checked ID, not by the tag, means a tag moved to another
//! image after the check can never take that image down with it. The daemon's
//! own refusal (`409`, non-forced removal of an image a container uses or
//! that another tag references) stays behind the checks for anything that
//! changes in between.

use std::collections::HashMap;

use bollard::errors::Error as DockerError;
use bollard::query_parameters::{ListContainersOptions, RemoveImageOptions};
use bollard::Docker;
use serde::{Deserialize, Serialize};

use crate::error::DockerUtilError;

/// Most references one call accepts. Bounds a remote request body and how
/// long a single call holds the daemon; callers with more chunk them.
pub const MAX_IMAGE_REMOVAL_BATCH: usize = 500;

/// Most other tags an outcome lists; the rest are only counted.
const MAX_REPORTED_TAGS: usize = 8;

/// One image the caller wants gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRemovalRequest {
    /// A `repository:tag` (a missing tag means `latest`) or an image ID
    /// (`sha256:<hex>`). A tag is the only tag the image may carry; an ID
    /// means the image must carry no tag at all.
    pub reference: String,
    /// The image ID the caller recorded for this reference. When set and the
    /// reference now names a different image, that image is left alone: it
    /// is not the one the caller meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_image_id: Option<String>,
    /// Keep the image if it was labelled as built by someone other than the
    /// caller (see [`OwnerGuard`]). Optional on the wire: an agent that
    /// predates it removes without this check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_guard: Option<OwnerGuard>,
}

/// Who may remove a labelled image: the image is kept when it carries
/// `label` with a value other than `owner`; `owner: None` means the caller
/// has no owner ID, so every image carrying the label is someone else's.
///
/// An image without the label predates ownership labels and proves nothing
/// either way. With `keep_unlabelled_when_shared`, it is kept when the
/// daemon shows another owner is present (any image carrying `label` with
/// another value): on a host shared with another Temps install, an unlabelled
/// image may well be that install's. On a host nobody else labels, it is
/// judged by the caller's records, as before the labels existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerGuard {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Optional on the wire: an agent that predates it removes unlabelled
    /// images on the caller's records.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_unlabelled_when_shared: bool,
}

impl ImageRemovalRequest {
    pub fn new(reference: impl Into<String>, expected_image_id: Option<String>) -> Self {
        Self {
            reference: reference.into(),
            expected_image_id,
            owner_guard: None,
        }
    }

    /// The same request, keeping images another owner labelled.
    pub fn with_owner_guard(mut self, guard: OwnerGuard) -> Self {
        self.owner_guard = Some(guard);
        self
    }
}

/// What happened to one [`ImageRemovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImageRemovalStatus {
    /// The image is gone.
    Removed { image_id: String },
    /// The daemon has no image under this reference (already removed, or
    /// never pulled to this host).
    Absent,
    /// A container (running or stopped) was created from this image.
    InUse {
        image_id: String,
        container_id: String,
    },
    /// The reference now names a different image than the one recorded.
    Replaced {
        expected_image_id: String,
        found_image_id: String,
    },
    /// The image is labelled as built by another owner (another Temps
    /// install sharing this Docker host).
    ForeignOwner { image_id: String, owner: String },
    /// The image carries no owner label, and the daemon shows another owner
    /// is present, so it may be that owner's image.
    UnlabelledOnSharedHost { image_id: String },
    /// The image also carries tags the caller did not ask to remove, so
    /// someone else still names it.
    OtherTags {
        image_id: String,
        /// At most a few of them.
        tags: Vec<String>,
        count: usize,
    },
    /// The daemon refused the non-forced removal (`409`), most often because
    /// a container or a tag appeared after the check.
    Refused { image_id: String, reason: String },
    /// The check or the removal call failed for another reason.
    Failed { reason: String },
}

impl ImageRemovalStatus {
    pub fn is_removed(&self) -> bool {
        matches!(self, Self::Removed { .. })
    }

    /// Whether the image was deliberately kept for safety (in use, replaced,
    /// tagged by someone else, refused), as opposed to removed, absent or
    /// failed.
    pub fn is_kept(&self) -> bool {
        matches!(
            self,
            Self::InUse { .. }
                | Self::Replaced { .. }
                | Self::ForeignOwner { .. }
                | Self::UnlabelledOnSharedHost { .. }
                | Self::OtherTags { .. }
                | Self::Refused { .. }
        )
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// A one-line reason for logs: why the image is still there, or `None`
    /// when it was removed or was already gone.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Removed { .. } | Self::Absent => None,
            Self::InUse {
                image_id,
                container_id,
            } => Some(format!(
                "image {image_id} is used by container {container_id}"
            )),
            Self::Replaced {
                expected_image_id,
                found_image_id,
            } => Some(format!(
                "reference now names image {found_image_id}, not the recorded {expected_image_id}"
            )),
            Self::ForeignOwner { image_id, owner } => Some(format!(
                "image {image_id} was built by another owner ({owner})"
            )),
            Self::UnlabelledOnSharedHost { image_id } => Some(format!(
                "image {image_id} carries no owner label and another owner's images share this \
                 host, so it may be theirs"
            )),
            Self::OtherTags {
                image_id,
                tags,
                count,
            } => Some(format!(
                "image {image_id} also carries {count} other tag(s): {}",
                tags.join(", ")
            )),
            Self::Refused { image_id, reason } => Some(format!(
                "the daemon refused to remove image {image_id}: {reason}"
            )),
            Self::Failed { reason } => Some(reason.clone()),
        }
    }
}

/// The outcome for one request, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRemovalOutcome {
    pub reference: String,
    #[serde(flatten)]
    pub status: ImageRemovalStatus,
}

/// What the daemon says a reference names now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedImage {
    pub image_id: String,
    pub tags: Vec<String>,
    pub labels: HashMap<String, String>,
}

/// What [`decide`] concluded for one request, before the container check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalDecision {
    /// Nothing else names the image: remove it by this ID if no container
    /// was created from it.
    Remove { image_id: String },
    /// Leave it, for this reason.
    Keep(ImageRemovalStatus),
}

/// Whether `request` may be removed, given what its reference names now.
pub fn decide(request: &ImageRemovalRequest, image: &InspectedImage) -> RemovalDecision {
    decide_on_host(request, image, false)
}

/// [`decide`], knowing whether the daemon shows another owner of images
/// labelled like the request's guard (`shared_host`).
pub fn decide_on_host(
    request: &ImageRemovalRequest,
    image: &InspectedImage,
    shared_host: bool,
) -> RemovalDecision {
    let image_id = normalize_image_id(&image.image_id);
    if let Some(expected) = request.expected_image_id.as_deref() {
        let expected = normalize_image_id(expected);
        if !expected.is_empty() && expected != image_id {
            return RemovalDecision::Keep(ImageRemovalStatus::Replaced {
                expected_image_id: expected,
                found_image_id: image_id,
            });
        }
    }
    if let Some(guard) = &request.owner_guard {
        match image.labels.get(&guard.label) {
            Some(labelled) if guard.owner.as_deref() != Some(labelled.as_str()) => {
                return RemovalDecision::Keep(ImageRemovalStatus::ForeignOwner {
                    image_id,
                    owner: labelled.clone(),
                });
            }
            None if guard.keep_unlabelled_when_shared && shared_host => {
                return RemovalDecision::Keep(ImageRemovalStatus::UnlabelledOnSharedHost {
                    image_id,
                });
            }
            _ => {}
        }
    }
    let requested = normalize_reference(&request.reference);
    let other_tags: Vec<String> = image
        .tags
        .iter()
        .filter(|tag| tag.as_str() != "<none>:<none>")
        .map(|tag| normalize_tag(tag))
        .filter(|tag| *tag != requested)
        .collect();
    if !other_tags.is_empty() {
        return RemovalDecision::Keep(ImageRemovalStatus::OtherTags {
            image_id,
            count: other_tags.len(),
            tags: other_tags.into_iter().take(MAX_REPORTED_TAGS).collect(),
        });
    }
    RemovalDecision::Remove { image_id }
}

/// `sha256:<lowercase hex>` for any spelling of an image ID; empty for blank.
pub fn normalize_image_id(id: &str) -> String {
    let lower = id.trim().to_ascii_lowercase();
    let hex = lower.trim_start_matches("sha256:");
    if hex.is_empty() {
        String::new()
    } else {
        format!("sha256:{hex}")
    }
}

fn is_image_id(reference: &str) -> bool {
    let reference = reference.trim().to_ascii_lowercase();
    reference.starts_with("sha256:")
        || (reference.len() == 64 && reference.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `repo:tag` with the implicit `latest` made explicit.
fn normalize_tag(tag: &str) -> String {
    let tag = tag.trim();
    let name_part = tag.rsplit('/').next().unwrap_or(tag);
    if name_part.contains(':') || tag.contains('@') {
        tag.to_string()
    } else {
        format!("{tag}:latest")
    }
}

fn normalize_reference(reference: &str) -> String {
    if is_image_id(reference) {
        normalize_image_id(reference)
    } else {
        normalize_tag(reference)
    }
}

/// What `reference` names on the daemon now, or `None` if nothing.
pub async fn inspect_reference(
    docker: &Docker,
    reference: &str,
) -> Result<Option<InspectedImage>, DockerError> {
    match docker.inspect_image(&normalize_reference(reference)).await {
        Ok(image) => Ok(Some(InspectedImage {
            image_id: image.id.unwrap_or_default(),
            tags: image.repo_tags.unwrap_or_default(),
            labels: image
                .config
                .and_then(|config| config.labels)
                .unwrap_or_default(),
        })),
        Err(DockerError::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// One container, running or stopped, created from `image_id` or from an
/// image built on top of it; `None` if there is none.
pub async fn container_using_image(
    docker: &Docker,
    image_id: &str,
) -> Result<Option<String>, DockerError> {
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            limit: Some(1),
            filters: Some(HashMap::from([(
                "ancestor".to_string(),
                vec![image_id.to_string()],
            )])),
            ..Default::default()
        }))
        .await?;
    Ok(containers
        .into_iter()
        .next()
        .map(|container| container.id.unwrap_or_else(|| "<unknown>".to_string())))
}

/// Most labelled images one listing keeps, oldest first. Docker cannot page
/// an image listing, so this bounds what a caller holds, not what the daemon
/// sends; the rest are counted in [`LabelledImages::unlisted`].
pub const MAX_LABELLED_IMAGES: usize = 1000;

/// An image carrying the label a listing asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelledImage {
    pub image_id: String,
    pub tags: Vec<String>,
    /// Only the labels under the prefix the listing kept.
    pub labels: HashMap<String, String>,
    /// Seconds since the epoch, as the daemon reports it.
    pub created: i64,
    /// A container created from the image, when the listing checked usage.
    /// A check that failed is reported here too, so the image is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use_by: Option<String>,
}

/// Where a listing left off: the last image it returned, in its
/// `(created, image_id)` order. The next listing starts after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCursor {
    pub created: i64,
    pub image_id: String,
}

/// What [`list_labelled_images`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelledImages {
    /// At most [`MAX_LABELLED_IMAGES`], oldest first, all after the cursor
    /// the listing was given.
    pub images: Vec<LabelledImage>,
    /// Matching images after the last one returned, left for a later listing.
    #[serde(default)]
    pub unlisted: usize,
    /// Where to start the next listing so it reaches them, when there are
    /// any. `None` once a listing reached the newest image: start over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ListCursor>,
}

/// Keep, oldest first, at most `max` of `images` after `after`, and say where
/// the next page starts. A listing that always kept the same oldest window
/// would never reach newer images when the oldest ones all stay; paging
/// through with the cursor reaches every image in turn.
pub fn page_labelled_images(
    mut images: Vec<LabelledImage>,
    after: Option<&ListCursor>,
    max: usize,
) -> LabelledImages {
    let position = |image: &LabelledImage| (image.created, image.image_id.clone());
    if let Some(after) = after {
        let start = (after.created, after.image_id.clone());
        images.retain(|image| position(image) > start);
    }
    images.sort_by_key(position);
    let unlisted = images.len().saturating_sub(max);
    images.truncate(max);
    let next = (unlisted > 0)
        .then(|| images.last())
        .flatten()
        .map(|last| ListCursor {
            created: last.created,
            image_id: last.image_id.clone(),
        });
    LabelledImages {
        images,
        unlisted,
        next,
    }
}

/// Images labelled `key=value` after `after`, oldest first and bounded by
/// [`MAX_LABELLED_IMAGES`] (see [`page_labelled_images`]), keeping only their
/// labels under `keep_prefix`. With `check_usage`, each kept image is also
/// checked for a container created from it (one query per image).
pub async fn list_labelled_images(
    docker: &Docker,
    key: &str,
    value: &str,
    keep_prefix: &str,
    check_usage: bool,
    after: Option<&ListCursor>,
) -> Result<LabelledImages, DockerUtilError> {
    let images = docker
        .list_images(Some(bollard::query_parameters::ListImagesOptions {
            all: false,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![format!("{key}={value}")],
            )])),
            ..Default::default()
        }))
        .await
        .map_err(|source| DockerUtilError::ImageInventory {
            operation: "list labelled images",
            source,
        })?;
    let images: Vec<LabelledImage> = images
        .into_iter()
        .map(|image| LabelledImage {
            image_id: image.id,
            tags: image
                .repo_tags
                .into_iter()
                .filter(|tag| tag != "<none>:<none>")
                .collect(),
            labels: image
                .labels
                .into_iter()
                .filter(|(label, _)| label.starts_with(keep_prefix))
                .collect(),
            created: image.created,
            in_use_by: None,
        })
        .collect();
    let mut page = page_labelled_images(images, after, MAX_LABELLED_IMAGES);
    if check_usage {
        for image in &mut page.images {
            image.in_use_by = match container_using_image(docker, &image.image_id).await {
                Ok(holder) => holder,
                Err(error) => Some(format!("<usage could not be checked: {error}>")),
            };
        }
    }
    Ok(page)
}

/// Remove each requested image that no container uses and nothing else
/// names, returning one outcome per request in request order.
///
/// Every failure is reported in that request's outcome and the rest are
/// still processed: a request whose image or containers cannot be read is
/// `Failed`, and nothing is removed for it. Accepts at most
/// [`MAX_IMAGE_REMOVAL_BATCH`] requests.
pub async fn remove_unused_images(
    docker: &Docker,
    requests: &[ImageRemovalRequest],
) -> Result<Vec<ImageRemovalOutcome>, DockerUtilError> {
    if requests.len() > MAX_IMAGE_REMOVAL_BATCH {
        return Err(DockerUtilError::ImageBatchTooLarge {
            requested: requests.len(),
            max: MAX_IMAGE_REMOVAL_BATCH,
        });
    }
    // Whether another owner's images share this daemon, once per guard in
    // the batch (in practice one).
    let mut shared_hosts: HashMap<(String, Option<String>), bool> = HashMap::new();
    for guard in requests
        .iter()
        .filter_map(|request| request.owner_guard.as_ref())
        .filter(|guard| guard.keep_unlabelled_when_shared)
    {
        let key = (guard.label.clone(), guard.owner.clone());
        if let std::collections::hash_map::Entry::Vacant(slot) = shared_hosts.entry(key) {
            slot.insert(another_owner_present(docker, &guard.label, guard.owner.as_deref()).await);
        }
    }
    let mut outcomes = Vec::with_capacity(requests.len());
    for request in requests {
        let shared_host = request
            .owner_guard
            .as_ref()
            .and_then(|guard| shared_hosts.get(&(guard.label.clone(), guard.owner.clone())))
            .copied()
            .unwrap_or(false);
        let status = remove_one(docker, request, shared_host).await;
        outcomes.push(ImageRemovalOutcome {
            reference: request.reference.clone(),
            status,
        });
    }
    Ok(outcomes)
}

/// Whether the daemon holds any image carrying `label` with a value other
/// than `owner`. A listing that fails counts as yes: unlabelled images are
/// then kept, the safe direction.
pub async fn another_owner_present(docker: &Docker, label: &str, owner: Option<&str>) -> bool {
    let listed = docker
        .list_images(Some(bollard::query_parameters::ListImagesOptions {
            all: false,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![label.to_string()],
            )])),
            ..Default::default()
        }))
        .await;
    match listed {
        Ok(images) => images
            .iter()
            .any(|image| image.labels.get(label).map(String::as_str) != owner),
        Err(_) => true,
    }
}

async fn remove_one(
    docker: &Docker,
    request: &ImageRemovalRequest,
    shared_host: bool,
) -> ImageRemovalStatus {
    let image = match inspect_reference(docker, &request.reference).await {
        Ok(Some(image)) => image,
        Ok(None) => return ImageRemovalStatus::Absent,
        Err(error) => {
            return ImageRemovalStatus::Failed {
                reason: format!("Failed to inspect image '{}': {error}", request.reference),
            }
        }
    };
    let image_id = match decide_on_host(request, &image, shared_host) {
        RemovalDecision::Keep(status) => return status,
        RemovalDecision::Remove { image_id } => image_id,
    };
    match container_using_image(docker, &image_id).await {
        Ok(None) => {}
        Ok(Some(container_id)) => {
            return ImageRemovalStatus::InUse {
                image_id,
                container_id,
            }
        }
        Err(error) => {
            return ImageRemovalStatus::Failed {
                reason: format!(
                    "Failed to list the containers created from image {image_id} ('{}'): {error}",
                    request.reference
                ),
            }
        }
    }
    remove_by_id(docker, &request.reference, image_id).await
}

async fn remove_by_id(docker: &Docker, reference: &str, image_id: String) -> ImageRemovalStatus {
    match docker
        .remove_image(
            &image_id,
            Some(RemoveImageOptions {
                force: false,
                noprune: false,
                ..Default::default()
            }),
            None,
        )
        .await
    {
        Ok(_) => ImageRemovalStatus::Removed { image_id },
        Err(DockerError::DockerResponseServerError {
            status_code: 404, ..
        }) => ImageRemovalStatus::Absent,
        Err(DockerError::DockerResponseServerError {
            status_code: 409,
            message,
        }) => ImageRemovalStatus::Refused {
            image_id,
            reason: message,
        },
        Err(error) => ImageRemovalStatus::Failed {
            reason: format!("Failed to remove image {image_id} ('{reference}'): {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn inspected(id: &str, tags: &[&str]) -> InspectedImage {
        InspectedImage {
            image_id: id.to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            labels: HashMap::new(),
        }
    }

    const OWNER_LABEL: &str = "sh.temps.image.instance";

    fn guard(owner: Option<&str>) -> OwnerGuard {
        OwnerGuard {
            label: OWNER_LABEL.to_string(),
            owner: owner.map(str::to_string),
            keep_unlabelled_when_shared: false,
        }
    }

    /// On a host another install also labels images on, an unlabelled image
    /// may be that install's (built before the labels): kept. On a host
    /// nobody else labels, it is judged by the caller's records.
    #[test]
    fn an_unlabelled_image_is_kept_only_when_another_owner_shares_the_host() {
        let request = ImageRemovalRequest::new("app-1:latest", Some(APP.to_string()))
            .with_owner_guard(OwnerGuard {
                keep_unlabelled_when_shared: true,
                ..guard(Some("staging.marker"))
            });
        let unlabelled = inspected(APP, &["app-1:latest"]);
        assert_eq!(
            decide_on_host(&request, &unlabelled, true),
            RemovalDecision::Keep(ImageRemovalStatus::UnlabelledOnSharedHost {
                image_id: APP.to_string()
            })
        );
        assert!(matches!(
            decide_on_host(&request, &unlabelled, false),
            RemovalDecision::Remove { .. }
        ));
        // This owner's own labelled image is removed even on a shared host.
        let mut ours = inspected(APP, &["app-1:latest"]);
        ours.labels
            .insert(OWNER_LABEL.to_string(), "staging.marker".to_string());
        assert!(matches!(
            decide_on_host(&request, &ours, true),
            RemovalDecision::Remove { .. }
        ));
        // Without the flag (an older caller), the host does not matter.
        let plain = ImageRemovalRequest::new("app-1:latest", Some(APP.to_string()))
            .with_owner_guard(guard(Some("staging.marker")));
        assert!(matches!(
            decide_on_host(&plain, &unlabelled, true),
            RemovalDecision::Remove { .. }
        ));
    }

    /// Another install sharing the host built the image: it is theirs,
    /// whatever this caller's records say.
    #[test]
    fn an_image_another_owner_labelled_is_kept() {
        let mut image = inspected(APP, &["app-1:latest"]);
        image
            .labels
            .insert(OWNER_LABEL.to_string(), "production.marker".to_string());
        let request = ImageRemovalRequest::new("app-1:latest", Some(APP.to_string()))
            .with_owner_guard(guard(Some("staging.marker")));
        assert_eq!(
            decide(&request, &image),
            RemovalDecision::Keep(ImageRemovalStatus::ForeignOwner {
                image_id: APP.to_string(),
                owner: "production.marker".to_string(),
            })
        );
        // A caller with no owner ID owns no labelled image at all.
        let request = ImageRemovalRequest::new("app-1:latest", None).with_owner_guard(guard(None));
        assert!(matches!(
            decide(&request, &image),
            RemovalDecision::Keep(ImageRemovalStatus::ForeignOwner { .. })
        ));
    }

    #[test]
    fn an_image_this_owner_labelled_or_never_labelled_is_judged_as_before() {
        let mut ours = inspected(APP, &["app-1:latest"]);
        ours.labels
            .insert(OWNER_LABEL.to_string(), "staging.marker".to_string());
        let request = ImageRemovalRequest::new("app-1:latest", Some(APP.to_string()))
            .with_owner_guard(guard(Some("staging.marker")));
        assert!(matches!(
            decide(&request, &ours),
            RemovalDecision::Remove { .. }
        ));
        // Built before ownership labels: the caller's records decide.
        assert!(matches!(
            decide(&request, &inspected(APP, &["app-1:latest"])),
            RemovalDecision::Remove { .. }
        ));
    }

    fn listed(id: &str, created: i64) -> LabelledImage {
        LabelledImage {
            image_id: id.to_string(),
            tags: Vec::new(),
            labels: HashMap::new(),
            created,
            in_use_by: None,
        }
    }

    /// Paging reaches every image: a window that always started at the
    /// oldest would never get past old images that all stay.
    #[test]
    fn paging_reaches_every_image_and_then_starts_over() {
        let images = vec![
            listed("c", 3),
            listed("a", 1),
            listed("b", 1),
            listed("d", 4),
            listed("e", 5),
        ];
        let ids = |page: &LabelledImages| -> Vec<String> {
            page.images.iter().map(|i| i.image_id.clone()).collect()
        };
        let first = page_labelled_images(images.clone(), None, 2);
        assert_eq!(ids(&first), ["a", "b"]);
        assert_eq!(first.unlisted, 3);
        let second = page_labelled_images(images.clone(), first.next.as_ref(), 2);
        assert_eq!(ids(&second), ["c", "d"]);
        let third = page_labelled_images(images.clone(), second.next.as_ref(), 2);
        assert_eq!(ids(&third), ["e"]);
        assert_eq!(third.unlisted, 0);
        assert_eq!(third.next, None, "the end of the list starts over");
        // Images sharing a timestamp are not skipped at a page boundary.
        let after_a = ListCursor {
            created: 1,
            image_id: "a".into(),
        };
        let tied = page_labelled_images(images, Some(&after_a), 1);
        assert_eq!(ids(&tied), ["b"]);
    }

    #[test]
    fn a_request_without_a_guard_round_trips_for_older_agents() {
        let json = serde_json::to_value(ImageRemovalRequest::new("app-1:latest", None))
            .expect("serialize");
        assert!(json.get("owner_guard").is_none(), "{json}");
        let guarded: ImageRemovalRequest = serde_json::from_value(serde_json::json!({
            "reference": "app-1:latest",
            "owner_guard": { "label": OWNER_LABEL, "owner": "a.b" }
        }))
        .expect("deserialize");
        assert_eq!(guarded.owner_guard, Some(guard(Some("a.b"))));
    }

    #[test]
    fn an_image_only_this_reference_names_is_removed_by_its_id() {
        assert_eq!(
            decide(
                &ImageRemovalRequest::new("app-1:latest", Some(APP.to_string())),
                &inspected(APP, &["app-1:latest"])
            ),
            RemovalDecision::Remove {
                image_id: APP.to_string()
            }
        );
    }

    /// Another tag on the same image means someone else still names it
    /// (another project's identical build, an operator's retag).
    #[test]
    fn an_image_with_another_tag_is_kept() {
        assert_eq!(
            decide(
                &ImageRemovalRequest::new("app-1:latest", None),
                &inspected(APP, &["app-1:latest", "other-project-7:latest"])
            ),
            RemovalDecision::Keep(ImageRemovalStatus::OtherTags {
                image_id: APP.to_string(),
                tags: vec!["other-project-7:latest".to_string()],
                count: 1,
            })
        );
        // Asked for by ID, any tag at all is someone else's.
        assert!(matches!(
            decide(
                &ImageRemovalRequest::new(APP, Some(APP.to_string())),
                &inspected(APP, &["app-1:latest"])
            ),
            RemovalDecision::Keep(ImageRemovalStatus::OtherTags { .. })
        ));
        assert!(matches!(
            decide(
                &ImageRemovalRequest::new(APP, Some(APP.to_string())),
                &inspected(APP, &["<none>:<none>"])
            ),
            RemovalDecision::Remove { .. }
        ));
    }

    #[test]
    fn a_tag_now_naming_another_image_is_left_alone() {
        assert_eq!(
            decide(
                &ImageRemovalRequest::new("app-1:latest", Some(APP.to_string())),
                &inspected(OTHER, &["app-1:latest"])
            ),
            RemovalDecision::Keep(ImageRemovalStatus::Replaced {
                expected_image_id: APP.to_string(),
                found_image_id: OTHER.to_string(),
            })
        );
    }

    #[test]
    fn ids_and_implicit_tags_compare_in_any_spelling() {
        let bare_hex = APP.trim_start_matches("sha256:");
        assert!(matches!(
            decide(
                &ImageRemovalRequest::new("app-1", Some(bare_hex.to_uppercase())),
                &inspected(bare_hex, &["app-1:latest"])
            ),
            RemovalDecision::Remove { image_id } if image_id == APP
        ));
        // A registry host with a port is not a tag.
        assert_eq!(
            normalize_tag("registry:5000/app"),
            "registry:5000/app:latest"
        );
        assert_eq!(normalize_reference(bare_hex), APP);
        assert_eq!(normalize_image_id(&APP.to_uppercase()), APP);
        assert_eq!(normalize_reference(&APP.to_uppercase()), APP);
    }

    #[test]
    fn outcomes_round_trip_as_tagged_json() {
        let outcome = ImageRemovalOutcome {
            reference: "app-1:latest".to_string(),
            status: ImageRemovalStatus::InUse {
                image_id: APP.to_string(),
                container_id: "c0ffee".to_string(),
            },
        };
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(json["status"], "in_use");
        assert_eq!(json["reference"], "app-1:latest");
        let back: ImageRemovalOutcome = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, outcome);
        assert!(back.status.is_kept());
        assert!(back.status.reason().is_some_and(|r| r.contains("c0ffee")));
    }

    #[tokio::test]
    async fn an_oversized_batch_is_refused_before_touching_the_daemon() {
        // No daemon is needed: the size check comes first.
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            return;
        };
        let requests =
            vec![ImageRemovalRequest::new("app:latest", None); MAX_IMAGE_REMOVAL_BATCH + 1];
        let result = remove_unused_images(&docker, &requests).await;
        assert!(matches!(
            result,
            Err(DockerUtilError::ImageBatchTooLarge { requested, .. }) if requested == MAX_IMAGE_REMOVAL_BATCH + 1
        ));
    }

    /// Build a single-tag image with its own ID, as a per-deployment build is.
    async fn build_unique_image(
        docker: &Docker,
        tag: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        build_labelled_unique_image(docker, tag, "sh.temps.test.unused", "x").await
    }

    /// [`build_unique_image`] with one extra label baked into the image.
    async fn build_labelled_unique_image(
        docker: &Docker,
        tag: &str,
        label: &str,
        value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use bollard::query_parameters::BuildImageOptionsBuilder;
        use futures::StreamExt as _;

        let dockerfile = format!(
            "FROM {}\nLABEL sh.temps.test.nonce={}\nLABEL {label}={value}\n",
            crate::test_support::TEST_IMAGE,
            uuid::Uuid::new_v4()
        );
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut archive = tar::Builder::new(Vec::new());
        archive.append_data(&mut header, "Dockerfile", dockerfile.as_bytes())?;
        let context = archive.into_inner()?;
        let mut build = docker.build_image(
            BuildImageOptionsBuilder::default().t(tag).rm(true).build(),
            None,
            Some(bollard::body_full(bytes::Bytes::from(context))),
        );
        while let Some(step) = build.next().await {
            step?;
        }
        Ok(())
    }

    async fn force_remove_image(docker: &Docker, tag: &str) {
        let _ = docker
            .remove_image(
                tag,
                Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            )
            .await;
    }

    /// Against a real daemon: an unused image goes, one a stopped container
    /// was created from stays, an unrelated image is untouched, and one that
    /// does not exist is reported absent. Skips when Docker is unavailable.
    #[tokio::test]
    async fn removes_only_unused_images_on_a_real_daemon() {
        use crate::pull::{pull_image, PullSpec};
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::CreateContainerOptionsBuilder;

        let Some(docker) =
            crate::test_support::docker_or_skip("removes_only_unused_images_on_a_real_daemon")
                .await
        else {
            return;
        };
        if let Err(error) =
            pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await
        {
            println!("Could not pull the test image, skipping: {error}");
            return;
        }
        let unused = format!("{}:latest", crate::test_support::unique_name("img-unused"));
        let in_use = format!("{}:latest", crate::test_support::unique_name("img-inuse"));
        let unrelated = format!(
            "{}:latest",
            crate::test_support::unique_name("img-unrelated")
        );
        let missing = format!("{}:latest", crate::test_support::unique_name("img-missing"));
        let retagged = format!(
            "{}:latest",
            crate::test_support::unique_name("img-retagged")
        );
        let foreign = format!("{}:latest", crate::test_support::unique_name("img-foreign"));
        let retag = format!("{}:keep", crate::test_support::unique_name("img-retag"));
        let holder = crate::test_support::unique_name("img-holder");

        let mut built = true;
        if let Err(error) =
            build_labelled_unique_image(&docker, &foreign, OWNER_LABEL, "another-install").await
        {
            println!("Could not build test image {foreign}, skipping: {error}");
            built = false;
        }
        for tag in [&unused, &in_use, &unrelated, &retagged] {
            if let Err(error) = build_unique_image(&docker, tag).await {
                println!("Could not build test image {tag}, skipping: {error}");
                built = false;
                break;
            }
        }
        if built {
            let (repo, tag) = retag.rsplit_once(':').expect("repo:tag");
            if let Err(error) = docker
                .tag_image(
                    &retagged,
                    Some(bollard::query_parameters::TagImageOptions {
                        repo: Some(repo.to_string()),
                        tag: Some(tag.to_string()),
                    }),
                )
                .await
            {
                println!("Could not retag the test image, skipping: {error}");
                built = false;
            }
        }
        let created = if built {
            docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(&holder).build()),
                    ContainerCreateBody {
                        image: Some(in_use.clone()),
                        cmd: Some(vec!["true".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
        } else {
            Ok(())
        };

        let outcomes = if built && created.is_ok() {
            Some(
                remove_unused_images(
                    &docker,
                    &[
                        ImageRemovalRequest::new(unused.clone(), None),
                        ImageRemovalRequest::new(in_use.clone(), None),
                        ImageRemovalRequest::new(missing.clone(), None),
                        ImageRemovalRequest::new(retagged.clone(), None),
                        ImageRemovalRequest::new(foreign.clone(), None)
                            .with_owner_guard(guard(Some("this-install"))),
                    ],
                )
                .await,
            )
        } else {
            None
        };
        let unused_left = docker.inspect_image(&unused).await.is_ok();
        let in_use_left = docker.inspect_image(&in_use).await.is_ok();
        let unrelated_left = docker.inspect_image(&unrelated).await.is_ok();
        let retagged_left = docker.inspect_image(&retagged).await.is_ok();
        let foreign_left = docker.inspect_image(&foreign).await.is_ok();

        // Clean up only what this test created, before asserting.
        crate::test_support::remove_container(&docker, &holder).await;
        for tag in [&unused, &in_use, &unrelated, &retagged, &retag, &foreign] {
            force_remove_image(&docker, tag).await;
        }

        let Some(outcomes) = outcomes else {
            println!("Test images could not be prepared ({created:?}), skipping");
            return;
        };
        let outcomes = outcomes.expect("the batch is within the limit");
        assert!(outcomes[0].status.is_removed(), "{:?}", outcomes[0]);
        assert!(
            matches!(outcomes[1].status, ImageRemovalStatus::InUse { .. }),
            "{:?}",
            outcomes[1]
        );
        assert_eq!(outcomes[2].status, ImageRemovalStatus::Absent);
        assert!(
            matches!(
                outcomes[3].status,
                ImageRemovalStatus::OtherTags { count: 1, .. }
            ),
            "{:?}",
            outcomes[3]
        );
        assert!(retagged_left, "an image someone else also tagged must stay");
        assert!(
            matches!(outcomes[4].status, ImageRemovalStatus::ForeignOwner { .. }),
            "{:?}",
            outcomes[4]
        );
        assert!(foreign_left, "an image another install labelled must stay");
        assert!(!unused_left, "the unused image must be gone");
        assert!(
            in_use_left,
            "an image a container was created from must stay"
        );
        assert!(unrelated_left, "an image nobody asked about must stay");
    }

    /// Against a real daemon: only images with the label are listed, with
    /// their labels under the kept prefix only, oldest first, and usage is
    /// reported when asked. Skips when Docker is unavailable.
    #[tokio::test]
    async fn lists_only_labelled_images_with_their_usage_on_a_real_daemon() {
        use crate::pull::{pull_image, PullSpec};
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{BuildImageOptionsBuilder, CreateContainerOptionsBuilder};
        use futures::StreamExt as _;

        let Some(docker) = crate::test_support::docker_or_skip(
            "lists_only_labelled_images_with_their_usage_on_a_real_daemon",
        )
        .await
        else {
            return;
        };
        if let Err(error) =
            pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await
        {
            println!("Could not pull the test image, skipping: {error}");
            return;
        }
        let owner = uuid::Uuid::new_v4().to_string();
        let free = format!("{}:latest", crate::test_support::unique_name("lbl-free"));
        let held = format!("{}:latest", crate::test_support::unique_name("lbl-held"));
        let other = format!("{}:latest", crate::test_support::unique_name("lbl-other"));
        let holder = crate::test_support::unique_name("lbl-holder");
        let build = |tag: String, owner: String| {
            let docker = docker.clone();
            async move {
                let dockerfile = format!(
                    "FROM {}\nLABEL sh.temps.test.nonce={}\n",
                    crate::test_support::TEST_IMAGE,
                    uuid::Uuid::new_v4()
                );
                let mut header = tar::Header::new_gnu();
                header.set_size(dockerfile.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                let mut archive = tar::Builder::new(Vec::new());
                archive.append_data(&mut header, "Dockerfile", dockerfile.as_bytes())?;
                let context = archive.into_inner()?;
                let labels = HashMap::from([
                    ("sh.temps.test.owner".to_string(), owner),
                    ("unrelated.label".to_string(), "x".to_string()),
                ]);
                let mut stream = docker.build_image(
                    BuildImageOptionsBuilder::default()
                        .t(&tag)
                        .labels(&labels)
                        .rm(true)
                        .build(),
                    None,
                    Some(bollard::body_full(bytes::Bytes::from(context))),
                );
                while let Some(step) = stream.next().await {
                    step?;
                }
                Ok::<(), Box<dyn std::error::Error>>(())
            }
        };
        let mut prepared: Result<(), String> = Ok(());
        for (tag, image_owner) in [
            (free.clone(), owner.clone()),
            (held.clone(), owner.clone()),
            (other.clone(), "someone-else".to_string()),
        ] {
            if let Err(error) = build(tag.clone(), image_owner).await {
                prepared = Err(format!("build {tag}: {error}"));
                break;
            }
        }
        if prepared.is_ok() {
            prepared = docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(&holder).build()),
                    ContainerCreateBody {
                        image: Some(held.clone()),
                        cmd: Some(vec!["true".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
                .map_err(|error| format!("create holder: {error}"));
        }
        let listed = if prepared.is_ok() {
            Some(
                list_labelled_images(
                    &docker,
                    "sh.temps.test.owner",
                    &owner,
                    "sh.temps.",
                    true,
                    None,
                )
                .await,
            )
        } else {
            None
        };

        crate::test_support::remove_container(&docker, &holder).await;
        for tag in [&free, &held, &other] {
            force_remove_image(&docker, tag).await;
        }

        if let Err(reason) = prepared {
            panic!("preparing the listing test failed: {reason}");
        }
        let listed = listed.expect("listed").expect("the listing succeeds");
        assert_eq!(listed.unlisted, 0);
        let tags: Vec<&str> = listed
            .images
            .iter()
            .flat_map(|image| image.tags.iter().map(String::as_str))
            .collect();
        assert_eq!(tags.len(), 2, "{listed:?}");
        assert!(tags.contains(&free.as_str()) && tags.contains(&held.as_str()));
        for image in &listed.images {
            assert!(
                image.labels.keys().all(|key| key.starts_with("sh.temps.")),
                "only kept labels: {image:?}"
            );
            let is_held = image.tags.contains(&held);
            assert_eq!(image.in_use_by.is_some(), is_held, "{image:?}");
        }
    }

    /// Against a real daemon: an unlabelled image is kept while another
    /// owner's labelled image shares the daemon, and removed once it no
    /// longer does. Uses its own label key, so nothing else on the host
    /// affects it. Skips when Docker is unavailable.
    #[tokio::test]
    async fn unlabelled_images_are_kept_while_another_owner_shares_the_daemon() {
        use crate::pull::{pull_image, PullSpec};

        let Some(docker) = crate::test_support::docker_or_skip(
            "unlabelled_images_are_kept_while_another_owner_shares_the_daemon",
        )
        .await
        else {
            return;
        };
        if let Err(error) =
            pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await
        {
            println!("Could not pull the test image, skipping: {error}");
            return;
        }
        let label = format!("sh.temps.test.owner.{}", uuid::Uuid::new_v4().simple());
        let theirs = format!(
            "{}:latest",
            crate::test_support::unique_name("shared-theirs")
        );
        let unlabelled = format!(
            "{}:latest",
            crate::test_support::unique_name("shared-unlabelled")
        );
        let request =
            ImageRemovalRequest::new(unlabelled.clone(), None).with_owner_guard(OwnerGuard {
                label: label.clone(),
                owner: Some("this-install".to_string()),
                keep_unlabelled_when_shared: true,
            });

        let prepared = async {
            build_labelled_unique_image(&docker, &theirs, &label, "another-install").await?;
            build_unique_image(&docker, &unlabelled).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
        .await;
        let while_shared = if prepared.is_ok() {
            Some(remove_unused_images(&docker, std::slice::from_ref(&request)).await)
        } else {
            None
        };
        let kept = docker.inspect_image(&unlabelled).await.is_ok();
        force_remove_image(&docker, &theirs).await;
        let once_alone = if prepared.is_ok() {
            Some(remove_unused_images(&docker, std::slice::from_ref(&request)).await)
        } else {
            None
        };
        let removed = docker.inspect_image(&unlabelled).await.is_err();
        force_remove_image(&docker, &unlabelled).await;

        if let Err(error) = prepared {
            panic!("preparing the shared-host test failed: {error}");
        }
        let while_shared = while_shared.expect("ran").expect("within the batch limit");
        assert!(
            matches!(
                while_shared[0].status,
                ImageRemovalStatus::UnlabelledOnSharedHost { .. }
            ),
            "{:?}",
            while_shared[0]
        );
        assert!(
            kept,
            "an unlabelled image stays while another owner shares the daemon"
        );
        let once_alone = once_alone.expect("ran").expect("within the batch limit");
        assert!(once_alone[0].status.is_removed(), "{:?}", once_alone[0]);
        assert!(
            removed,
            "with no other owner present it is judged on the records"
        );
    }
}
