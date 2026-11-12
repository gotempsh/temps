// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing images only when nothing on the daemon still uses them.
//!
//! Callers that clean up images they built (a deleted project's deployment
//! images, an orphan sweep) all need the same guard: an image a container
//! still runs from, or was created from and could be restarted from, is left
//! alone whatever its tag says. A container records the image *ID* it was
//! created from, so a check by tag alone would miss a container started from
//! an image that was retagged since. This module reads the daemon's images
//! and containers once per call, decides per reference by ID, and only then
//! removes; the daemon's own refusal (`409`, non-forced removal) stays behind
//! it as the last line of defence for anything created in between.
//!
//! Removal is never forced. Removing a tag that shares its image with other
//! tags only untags it, so another owner's tag keeps the image.

use std::collections::HashMap;

use bollard::errors::Error as DockerError;
use bollard::query_parameters::{ListContainersOptions, ListImagesOptions, RemoveImageOptions};
use bollard::Docker;
use serde::{Deserialize, Serialize};

use crate::error::DockerUtilError;

/// Most references one call accepts. Bounds a remote request body and how
/// long a single call holds the daemon; callers with more chunk them.
pub const MAX_IMAGE_REMOVAL_BATCH: usize = 500;

/// One image the caller wants gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRemovalRequest {
    /// A `repository:tag` (a missing tag means `latest`) or an image ID
    /// (`sha256:<hex>`).
    pub reference: String,
    /// The image ID the caller recorded for this reference. When set and the
    /// reference now names a different image, that image is left alone: it
    /// is not the one the caller meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_image_id: Option<String>,
}

impl ImageRemovalRequest {
    pub fn new(reference: impl Into<String>, expected_image_id: Option<String>) -> Self {
        Self {
            reference: reference.into(),
            expected_image_id,
        }
    }
}

/// What happened to one [`ImageRemovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImageRemovalStatus {
    /// The reference is gone. If other tags shared the image, only this tag
    /// was removed and the image stays for them.
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
    /// The daemon refused the non-forced removal (`409`), most often because
    /// a container appeared after the check.
    Refused { image_id: String, reason: String },
    /// The removal call failed for another reason.
    Failed { reason: String },
}

impl ImageRemovalStatus {
    pub fn is_removed(&self) -> bool {
        matches!(self, Self::Removed { .. })
    }

    /// Whether the image was deliberately kept for safety (in use, replaced,
    /// refused), as opposed to removed, absent or failed.
    pub fn is_kept(&self) -> bool {
        matches!(
            self,
            Self::InUse { .. } | Self::Replaced { .. } | Self::Refused { .. }
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

/// What [`ImageInventory::decide`] concluded for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalDecision {
    /// Nothing uses the image: remove the reference.
    Remove { image_id: String },
    /// Leave it, for this reason.
    Keep(ImageRemovalStatus),
}

/// The daemon's images and the image each container was created from, read
/// once and consulted for every request in a batch.
#[derive(Debug, Default, Clone)]
pub struct ImageInventory {
    /// Normalized tag or ID -> normalized image ID.
    by_reference: HashMap<String, String>,
    /// Normalized image ID -> one container created from it.
    in_use: HashMap<String, String>,
}

impl ImageInventory {
    /// Build from `(image id, tags)` pairs and `(container id, image id)`
    /// pairs, as the daemon lists them.
    pub fn new<I, T, C>(images: I, containers: C) -> Self
    where
        I: IntoIterator<Item = (String, T)>,
        T: IntoIterator<Item = String>,
        C: IntoIterator<Item = (String, String)>,
    {
        let mut by_reference = HashMap::new();
        for (id, tags) in images {
            let id = normalize_image_id(&id);
            if id.is_empty() {
                continue;
            }
            for tag in tags {
                if tag != "<none>:<none>" {
                    by_reference.insert(normalize_tag(&tag), id.clone());
                }
            }
            by_reference.insert(id.clone(), id);
        }
        let mut in_use = HashMap::new();
        for (container_id, image_id) in containers {
            let image_id = normalize_image_id(&image_id);
            if !image_id.is_empty() {
                in_use.entry(image_id).or_insert(container_id);
            }
        }
        Self {
            by_reference,
            in_use,
        }
    }

    /// The image ID `reference` names on this daemon, if any.
    pub fn resolve(&self, reference: &str) -> Option<&str> {
        self.by_reference
            .get(&normalize_reference(reference))
            .map(String::as_str)
    }

    /// Whether `request` may be removed, and if not, why.
    pub fn decide(&self, request: &ImageRemovalRequest) -> RemovalDecision {
        let Some(image_id) = self.resolve(&request.reference) else {
            return RemovalDecision::Keep(ImageRemovalStatus::Absent);
        };
        if let Some(expected) = request.expected_image_id.as_deref() {
            let expected = normalize_image_id(expected);
            if !expected.is_empty() && expected != image_id {
                return RemovalDecision::Keep(ImageRemovalStatus::Replaced {
                    expected_image_id: expected,
                    found_image_id: image_id.to_string(),
                });
            }
        }
        if let Some(container_id) = self.in_use.get(image_id) {
            return RemovalDecision::Keep(ImageRemovalStatus::InUse {
                image_id: image_id.to_string(),
                container_id: container_id.clone(),
            });
        }
        RemovalDecision::Remove {
            image_id: image_id.to_string(),
        }
    }
}

/// `sha256:<lowercase hex>` for any spelling of an image ID; empty for blank.
pub fn normalize_image_id(id: &str) -> String {
    let hex = id.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if hex.is_empty() {
        String::new()
    } else {
        format!("sha256:{hex}")
    }
}

fn is_image_id(reference: &str) -> bool {
    let reference = reference.trim();
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

/// Read the daemon's images and every container's image, running or not.
pub async fn read_image_inventory(docker: &Docker) -> Result<ImageInventory, DockerUtilError> {
    let images = docker
        .list_images(Some(ListImagesOptions {
            all: false,
            ..Default::default()
        }))
        .await
        .map_err(|source| DockerUtilError::ImageInventory {
            operation: "list images",
            source,
        })?;
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
        .map_err(|source| DockerUtilError::ImageInventory {
            operation: "list containers",
            source,
        })?;
    Ok(ImageInventory::new(
        images.into_iter().map(|image| (image.id, image.repo_tags)),
        containers
            .into_iter()
            .filter_map(|container| Some((container.id?, container.image_id?))),
    ))
}

/// Remove each requested image that no container uses, returning one outcome
/// per request in request order.
///
/// Fails as a whole only when the daemon's images or containers cannot be
/// listed, because without them nothing can be proved unused; nothing is
/// removed then. A failure to remove one image is reported in its outcome
/// and the rest are still processed. Accepts at most
/// [`MAX_IMAGE_REMOVAL_BATCH`] requests.
pub async fn remove_unused_images(
    docker: &Docker,
    requests: &[ImageRemovalRequest],
) -> Result<Vec<ImageRemovalOutcome>, DockerUtilError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    if requests.len() > MAX_IMAGE_REMOVAL_BATCH {
        return Err(DockerUtilError::ImageBatchTooLarge {
            requested: requests.len(),
            max: MAX_IMAGE_REMOVAL_BATCH,
        });
    }
    let inventory = read_image_inventory(docker).await?;
    let mut outcomes = Vec::with_capacity(requests.len());
    for request in requests {
        let status = match inventory.decide(request) {
            RemovalDecision::Keep(status) => status,
            RemovalDecision::Remove { image_id } => {
                remove_reference(docker, &request.reference, image_id).await
            }
        };
        outcomes.push(ImageRemovalOutcome {
            reference: request.reference.clone(),
            status,
        });
    }
    Ok(outcomes)
}

async fn remove_reference(
    docker: &Docker,
    reference: &str,
    image_id: String,
) -> ImageRemovalStatus {
    let target = normalize_reference(reference);
    match docker
        .remove_image(
            &target,
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
            reason: format!("Failed to remove image '{target}' ({image_id}): {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn inventory(images: &[(&str, &[&str])], containers: &[(&str, &str)]) -> ImageInventory {
        ImageInventory::new(
            images.iter().map(|(id, tags)| {
                (
                    id.to_string(),
                    tags.iter().map(|tag| tag.to_string()).collect::<Vec<_>>(),
                )
            }),
            containers
                .iter()
                .map(|(container, image)| (container.to_string(), image.to_string())),
        )
    }

    #[test]
    fn an_unused_image_is_removed() {
        let inventory = inventory(&[(APP, &["app-1:latest"])], &[]);
        assert_eq!(
            inventory.decide(&ImageRemovalRequest::new("app-1:latest", None)),
            RemovalDecision::Remove {
                image_id: APP.to_string()
            }
        );
    }

    /// A container records the image ID, not the tag: one created from the
    /// same image under another name still holds it.
    #[test]
    fn an_image_used_by_another_container_is_kept_whatever_its_tag() {
        let inventory = inventory(
            &[(APP, &["app-1:latest", "renamed:v2"])],
            &[("c0ffee", APP)],
        );
        assert_eq!(
            inventory.decide(&ImageRemovalRequest::new("app-1:latest", None)),
            RemovalDecision::Keep(ImageRemovalStatus::InUse {
                image_id: APP.to_string(),
                container_id: "c0ffee".to_string(),
            })
        );
    }

    #[test]
    fn a_container_of_another_image_does_not_hold_this_one() {
        let inventory = inventory(
            &[(APP, &["app-1:latest"]), (OTHER, &["other:latest"])],
            &[("c0ffee", OTHER)],
        );
        assert!(matches!(
            inventory.decide(&ImageRemovalRequest::new("app-1:latest", None)),
            RemovalDecision::Remove { .. }
        ));
    }

    #[test]
    fn an_image_already_gone_is_absent() {
        let inventory = inventory(&[(OTHER, &["other:latest"])], &[]);
        assert_eq!(
            inventory.decide(&ImageRemovalRequest::new("app-1:latest", None)),
            RemovalDecision::Keep(ImageRemovalStatus::Absent)
        );
    }

    #[test]
    fn a_tag_now_naming_another_image_is_left_alone() {
        let inventory = inventory(&[(OTHER, &["app-1:latest"])], &[]);
        assert_eq!(
            inventory.decide(&ImageRemovalRequest::new(
                "app-1:latest",
                Some(APP.to_string())
            )),
            RemovalDecision::Keep(ImageRemovalStatus::Replaced {
                expected_image_id: APP.to_string(),
                found_image_id: OTHER.to_string(),
            })
        );
    }

    #[test]
    fn ids_and_implicit_tags_resolve_in_any_spelling() {
        let inventory = inventory(&[(APP, &["app-1:latest"])], &[]);
        let bare_hex = APP.trim_start_matches("sha256:");
        assert_eq!(inventory.resolve("app-1"), Some(APP));
        assert_eq!(inventory.resolve(bare_hex), Some(APP));
        assert_eq!(
            inventory.resolve(&APP.to_uppercase().replace("SHA256:", "sha256:")),
            Some(APP)
        );
        assert!(matches!(
            inventory.decide(&ImageRemovalRequest::new(
                "app-1:latest",
                Some(bare_hex.to_string())
            )),
            RemovalDecision::Remove { .. }
        ));
        // A registry host with a port is not a tag.
        assert_eq!(
            normalize_tag("registry:5000/app"),
            "registry:5000/app:latest"
        );
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
        use bollard::query_parameters::BuildImageOptionsBuilder;
        use futures::StreamExt as _;

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
        let holder = crate::test_support::unique_name("img-holder");

        let mut built = true;
        for tag in [&unused, &in_use, &unrelated] {
            if let Err(error) = build_unique_image(&docker, tag).await {
                println!("Could not build test image {tag}, skipping: {error}");
                built = false;
                break;
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

        // Clean up only what this test created, before asserting.
        crate::test_support::remove_container(&docker, &holder).await;
        for tag in [&unused, &in_use, &unrelated] {
            force_remove_image(&docker, tag).await;
        }

        let Some(outcomes) = outcomes else {
            println!("Test images could not be prepared ({created:?}), skipping");
            return;
        };
        let outcomes = outcomes.expect("the daemon's images and containers can be listed");
        assert!(outcomes[0].status.is_removed(), "{:?}", outcomes[0]);
        assert!(
            matches!(outcomes[1].status, ImageRemovalStatus::InUse { .. }),
            "{:?}",
            outcomes[1]
        );
        assert_eq!(outcomes[2].status, ImageRemovalStatus::Absent);
        assert!(!unused_left, "the unused image must be gone");
        assert!(
            in_use_left,
            "an image a container was created from must stay"
        );
        assert!(unrelated_left, "an image nobody asked about must stay");
    }
}
