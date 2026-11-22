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

/// Most labelled images one listing keeps, oldest first; the rest are
/// counted in [`LabelledImages::unlisted`]. The listing is read as a stream,
/// so this bounds what is held while it is read, not only what is returned.
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
    images: Vec<LabelledImage>,
    after: Option<&ListCursor>,
    max: usize,
) -> LabelledImages {
    let mut page = listing::PageCollector::new(after, max);
    for image in images {
        page.offer(image);
    }
    page.finish()
}

/// Images carrying every `key=value` of `labels` after `after`, oldest
/// first and bounded by [`MAX_LABELLED_IMAGES`] (see
/// [`page_labelled_images`]), keeping only their labels under `keep_prefix`.
/// With `check_usage`, each kept image is also checked for a container
/// created from it (one query per image).
///
/// Docker cannot page or limit an image listing, so its answer is read as
/// a stream and only the page is held: at most [`MAX_LABELLED_IMAGES`]
/// images and one entry being read, however many images match on the
/// daemon (see the `listing` module).
pub async fn list_labelled_images(
    docker: &Docker,
    labels: &[(&str, &str)],
    keep_prefix: &str,
    check_usage: bool,
    after: Option<&ListCursor>,
) -> Result<LabelledImages, DockerUtilError> {
    let mut page =
        listing::stream_labelled_images(docker, labels, keep_prefix, after, MAX_LABELLED_IMAGES)
            .await?;
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
    // Whether another owner's images share this daemon, asked at most once
    // per guard in the batch (in practice one), and only when an image
    // without an owner label turns up: listing every owner's images is the
    // one daemon-wide read here, and labelled images never need it.
    let mut shared_hosts = SharedHosts::new();
    let mut outcomes = Vec::with_capacity(requests.len());
    for request in requests {
        let status = remove_one(docker, request, &mut shared_hosts).await;
        outcomes.push(ImageRemovalOutcome {
            reference: request.reference.clone(),
            status,
        });
    }
    Ok(outcomes)
}

/// Answers of [`another_owner_present`] within one batch, by guard label and
/// owner.
type SharedHosts = HashMap<(String, Option<String>), bool>;

/// The guard whose shared-host answer [`decide_on_host`] needs for `image`:
/// only an image without the guard's owner label, under a guard that keeps
/// such images on a shared host. Any other image is decided without it.
fn guard_needing_shared_host<'a>(
    request: &'a ImageRemovalRequest,
    image: &InspectedImage,
) -> Option<&'a OwnerGuard> {
    request.owner_guard.as_ref().filter(|guard| {
        guard.keep_unlabelled_when_shared && !image.labels.contains_key(&guard.label)
    })
}

/// Containers one page of [`another_owner_present`] reads.
const OWNER_PROBE_PAGE: usize = 64;
/// Pages [`another_owner_present`] reads before giving up and answering yes.
const MAX_OWNER_PROBE_PAGES: usize = 64;

/// Whether another owner uses this daemon: a container, running or stopped,
/// created from an image carrying `label` with a value other than `owner`
/// (a container inherits its image's labels).
///
/// Docker can limit and page a container listing, never an image listing,
/// so this reads the labelled containers a page of [`OWNER_PROBE_PAGE`] at a
/// time, newest first, and stops at the first one of another owner: what it
/// holds never grows with the daemon. An owner that built images here but
/// runs no container from any of them is not seen; see the known limits of
/// unlabelled images. A listing that fails, or more of this owner's own
/// containers than [`MAX_OWNER_PROBE_PAGES`] pages, counts as yes:
/// unlabelled images are then kept, the safe direction.
pub async fn another_owner_present(docker: &Docker, label: &str, owner: Option<&str>) -> bool {
    probe_other_owner(
        docker,
        label,
        owner,
        OWNER_PROBE_PAGE,
        MAX_OWNER_PROBE_PAGES,
    )
    .await
}

async fn probe_other_owner(
    docker: &Docker,
    label: &str,
    owner: Option<&str>,
    page_size: usize,
    max_pages: usize,
) -> bool {
    let mut before: Option<String> = None;
    for _ in 0..max_pages {
        let mut filters = HashMap::from([("label".to_string(), vec![label.to_string()])]);
        if let Some(before) = &before {
            filters.insert("before".to_string(), vec![before.clone()]);
        }
        let Ok(page) = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                limit: Some(i32::try_from(page_size).unwrap_or(i32::MAX)),
                filters: Some(filters),
                ..Default::default()
            }))
            .await
        else {
            return true;
        };
        let foreign = page.iter().any(|container| {
            container
                .labels
                .as_ref()
                .and_then(|labels| labels.get(label))
                .map(String::as_str)
                != owner
        });
        if foreign {
            return true;
        }
        if page.len() < page_size {
            return false;
        }
        match page.last().and_then(|container| container.id.clone()) {
            Some(id) => before = Some(id),
            None => return true,
        }
    }
    true
}

async fn remove_one(
    docker: &Docker,
    request: &ImageRemovalRequest,
    shared_hosts: &mut SharedHosts,
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
    let shared_host = match guard_needing_shared_host(request, &image) {
        None => false,
        Some(guard) => {
            let key = (guard.label.clone(), guard.owner.clone());
            match shared_hosts.get(&key) {
                Some(&known) => known,
                None => {
                    let found =
                        another_owner_present(docker, &guard.label, guard.owner.as_deref()).await;
                    shared_hosts.insert(key, found);
                    found
                }
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

mod listing;

#[cfg(test)]
mod tests;
