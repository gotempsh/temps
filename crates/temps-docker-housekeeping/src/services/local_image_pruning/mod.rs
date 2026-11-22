// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Image pruning a host applies to its own Docker daemon without a database.
//!
//! Two rules, both decided from `docker image ls` and `docker ps -a` alone:
//!
//! - **Worker deployment images** ([`plan_node_deployment_image_removals`]):
//!   a worker agent has no database, so it keeps the newest images of each
//!   repository its Temps deployment containers use and removes older ones
//!   it can pull again.
//! - **Superseded Temps infrastructure images**
//!   ([`plan_superseded_infra_image_removals`]): old sandbox and preview
//!   gateway versions, pinned by digest and so never "dangling", on the
//!   console and on every worker.
//!
//! The console's database-backed deployment retention lives in
//! [`super::docker_cleanup_service`]; both are run by its nightly cleanup.

use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error};

use super::docker_cleanup_service::{
    summarize_removals, DockerClient, ImageRemoval, LocalContainer, LocalImage,
    RESERVED_LOCAL_IMAGE_PREFIX,
};

/// Repository part of an image reference: `ghcr.io/org/app:v1` and
/// `ghcr.io/org/app@sha256:...` both give `ghcr.io/org/app`, and a registry
/// port (`registry:5000/app:v1`) is not mistaken for a tag. `None` for a bare
/// image ID. Docker Hub references are normalised to the short form Docker
/// itself lists (`nginx`, `org/app`), so a container created from
/// `docker.io/library/nginx:1` matches the image listed as `nginx:1`.
pub fn image_repository(reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty() || reference.starts_with("sha256:") {
        return None;
    }
    let without_digest = reference
        .split_once('@')
        .map_or(reference, |(repository, _)| repository);
    let repository = match without_digest.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => repository,
        _ => without_digest,
    };
    let repository = ["docker.io/library/", "index.docker.io/library/"]
        .iter()
        .chain(["docker.io/", "index.docker.io/"].iter())
        .find_map(|prefix| repository.strip_prefix(prefix))
        .unwrap_or(repository);
    (!repository.is_empty()).then(|| repository.to_string())
}

/// Repository of the preview routing gateway image Temps pulls.
pub const TEMPS_PREVIEW_GATEWAY_REPOSITORY: &str = "ghcr.io/gotempsh/temps-preview-gateway";

/// Every repository of infrastructure images Temps pulls and runs itself:
/// the workspace daemons and sandbox runtimes a release pins (the same set
/// as the release manifest), and the preview gateway. Listed one by one so
/// a repository Temps does not publish never enters the deletion boundary,
/// whatever its name looks like.
const TEMPS_MANAGED_INFRA_REPOSITORIES: [&str; 9] = [
    "ghcr.io/gotempsh/temps-sandbox-nodejs",
    "ghcr.io/gotempsh/temps-sandbox-python",
    "ghcr.io/gotempsh/temps-sandbox-all",
    "ghcr.io/gotempsh/temps-sandbox-node",
    "ghcr.io/gotempsh/temps-sandbox-bun",
    "ghcr.io/gotempsh/temps-sandbox-rust",
    "ghcr.io/gotempsh/temps-sandbox-go",
    "ghcr.io/gotempsh/temps-sandbox-full",
    TEMPS_PREVIEW_GATEWAY_REPOSITORY,
];

/// Whether `repository` holds infrastructure images Temps itself pulls and
/// runs (sandbox runtimes, workspace daemons, the preview gateway), as
/// opposed to anything a user deploys.
///
/// Deliberately an exact allowlist inside Temps' own registry namespace
/// rather than "anything under `ghcr.io/gotempsh/`": that namespace also
/// holds the Temps server image an operator may run themselves (and roll
/// back to), and managed-database images whose older versions a restore can
/// still need. Neither is ours to remove.
pub fn is_temps_managed_infra_repository(repository: &str) -> bool {
    TEMPS_MANAGED_INFRA_REPOSITORIES.contains(&repository)
}

/// The infrastructure images this build pins (embedded at compile time by
/// official releases). Always kept, even when no container runs them yet,
/// so the next sandbox or gateway start does not have to pull them again.
fn pinned_infra_image_references() -> Vec<&'static str> {
    use temps_core::release_images as pinned;
    [
        pinned::PREVIEW_GATEWAY,
        pinned::DAEMON_NODEJS,
        pinned::DAEMON_PYTHON,
        pinned::DAEMON_ALL,
        pinned::SANDBOX_NODE,
        pinned::SANDBOX_BUN,
        pinned::SANDBOX_PYTHON,
        pinned::SANDBOX_RUST,
        pinned::SANDBOX_GO,
        pinned::SANDBOX_FULL,
    ]
    .into_iter()
    .flatten()
    .chain(std::iter::once(pinned::LOCAL_PREVIEW_GATEWAY_IMAGE))
    .collect()
}

/// Limits of the database-free image pruning a host applies to its own
/// Docker daemon (worker agents, and the infrastructure-image sweep on the
/// console).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalImagePrunePolicy {
    /// Newest distinct images kept in each deployment repository on a worker
    /// node, whatever their age. Mirrors the console's
    /// `keep_recent_deployment_images` rollback floor.
    pub keep_newest_deployment_images: usize,
    /// Newest images kept in each Temps infrastructure repository, on top of
    /// the ones a container uses and the ones this release pins.
    pub keep_newest_infra_images: usize,
    /// Most images a single pass removes in one run; the rest are picked up
    /// on the following nights.
    pub max_removals_per_run: usize,
}

impl Default for LocalImagePrunePolicy {
    fn default() -> Self {
        Self {
            keep_newest_deployment_images: 5,
            keep_newest_infra_images: 1,
            max_removals_per_run: 500,
        }
    }
}

/// Image IDs and references that some container on this daemon (running or
/// stopped) uses. Built once per run and consulted by every local pass.
struct ImagesInUse<'a> {
    ids: std::collections::HashSet<&'a str>,
    references: std::collections::HashSet<&'a str>,
}

impl<'a> ImagesInUse<'a> {
    fn new(containers: &'a [LocalContainer]) -> Self {
        Self {
            ids: containers
                .iter()
                .map(|c| c.image_id.as_str())
                .filter(|id| !id.is_empty())
                .collect(),
            references: containers
                .iter()
                .map(|c| c.image.as_str())
                .filter(|image| !image.is_empty())
                .collect(),
        }
    }

    fn contains(&self, image: &LocalImage, reference: &str) -> bool {
        self.ids.contains(image.id.as_str()) || self.references.contains(reference)
    }
}

/// Which registry-pulled deployment images a worker node may remove, oldest
/// first, as `repo:tag` references (or `repo@sha256:...` for an image that
/// was pulled by digest and carries no tag).
///
/// A worker has no database, so it cannot apply the console's retention
/// policy. It applies a stricter local rule instead, and only to images it
/// can prove are both Temps deployment images and re-pullable:
///
/// - **Temps deployment repository.** A tag is considered only when its
///   repository is one a Temps deployment container on this node (one with
///   the `sh.temps.deploy_id` label, running or stopped) was created from.
///   Images in any other repository — a user's own, a managed service's, or
///   Temps infrastructure — are never touched by this rule.
/// - **Re-pullable.** The image must carry a registry digest for that same
///   repository, i.e. it was pulled from a registry that can serve it again
///   (a rollback on a worker pulls a missing registry image itself). Images
///   transferred from the control plane or built on this node have no such
///   digest and are always kept: for those this node may be the only copy.
/// - **Rollback floor.** The newest `keep_newest_deployment_images` distinct
///   images of each repository are kept regardless of age.
/// - **Never in use.** An image any container uses (by ID or by the
///   reference it was created from) is kept, and removal is non-forced so
///   Docker refuses one that became in use meanwhile.
///
/// The result is capped at `max_removals_per_run`.
pub fn plan_node_deployment_image_removals(
    images: &[LocalImage],
    containers: &[LocalContainer],
    policy: &LocalImagePrunePolicy,
) -> Vec<String> {
    let in_use = ImagesInUse::new(containers);
    let deployment_repositories: std::collections::HashSet<String> = containers
        .iter()
        .filter(|c| c.is_temps_deployment)
        .filter(|c| !c.image.starts_with(RESERVED_LOCAL_IMAGE_PREFIX))
        .filter_map(|c| image_repository(&c.image))
        .filter(|repository| !is_temps_managed_infra_repository(repository))
        .collect();
    if deployment_repositories.is_empty() {
        return Vec::new();
    }

    let mut by_repository: std::collections::BTreeMap<String, Vec<(&LocalImage, &str)>> =
        std::collections::BTreeMap::new();
    for image in images {
        for tag in image.removable_references() {
            if let Some(repository) = image_repository(tag) {
                if deployment_repositories.contains(&repository) {
                    by_repository
                        .entry(repository)
                        .or_default()
                        .push((image, tag.as_str()));
                }
            }
        }
    }

    let mut removable: Vec<(&LocalImage, &str)> = Vec::new();
    for (repository, mut tags) in by_repository {
        // Newest first; ties broken deterministically.
        tags.sort_by(|(a, a_tag), (b, b_tag)| {
            b.created
                .cmp(&a.created)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a_tag.cmp(b_tag))
        });
        let mut kept_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (image, tag) in tags {
            if kept_ids.contains(image.id.as_str())
                || kept_ids.len() < policy.keep_newest_deployment_images
            {
                kept_ids.insert(image.id.as_str());
                continue;
            }
            if in_use.contains(image, tag) {
                continue;
            }
            let re_pullable = image
                .repo_digests
                .iter()
                .any(|digest| image_repository(digest).as_deref() == Some(repository.as_str()));
            if !re_pullable {
                continue;
            }
            removable.push((image, tag));
        }
    }

    removable
        .sort_by(|(a, a_tag), (b, b_tag)| a.created.cmp(&b.created).then_with(|| a_tag.cmp(b_tag)));
    removable
        .into_iter()
        .take(policy.max_removals_per_run)
        .map(|(_, tag)| tag.to_string())
        .collect()
}

/// Which superseded Temps infrastructure images may be removed, oldest
/// first, as image IDs.
///
/// Sandbox and gateway images are pinned by digest, so an old version is
/// left with no tag (`<none>` in `docker images`) but still holds a digest
/// reference. Docker's `dangling=true` prune never matches such an image,
/// which is why every Temps update used to leave the previous versions
/// behind for good. This rule removes them, but only when:
///
/// - **every** reference of the image (tags and digests) is in one Temps
///   infrastructure repository ([`is_temps_managed_infra_repository`]), so an
///   image a user also tagged into their own repository is never touched;
/// - it can be pulled again: it carries a registry digest and no `:dev` tag.
///   A local build (no digest, or a `:dev` tag, which Temps never pulls) may
///   be the only copy of that image, even under the official repository
///   name, so it is the operator's to remove;
/// - no container, running or stopped, uses it;
/// - it is not one this release pins ([`pinned_infra_image_references`]);
/// - and it is not among the newest `keep_newest_infra_images` of its
///   repository.
///
/// Removal is by image ID and non-forced, so Docker still refuses one that is
/// referenced from another repository or became in use meanwhile.
pub fn plan_superseded_infra_image_removals(
    images: &[LocalImage],
    containers: &[LocalContainer],
    pinned: &[&str],
    policy: &LocalImagePrunePolicy,
) -> Vec<String> {
    let in_use = ImagesInUse::new(containers);
    let mut by_repository: std::collections::BTreeMap<String, Vec<&LocalImage>> =
        std::collections::BTreeMap::new();
    for image in images {
        let references: Vec<&String> = image
            .repo_tags
            .iter()
            .chain(image.repo_digests.iter())
            .collect();
        let repositories: std::collections::BTreeSet<Option<String>> = references
            .iter()
            .map(|reference| image_repository(reference))
            .collect();
        let mut repositories = repositories.into_iter();
        let (Some(Some(repository)), None) = (repositories.next(), repositories.next()) else {
            // No reference, an unparseable one, or more than one repository.
            continue;
        };
        if !is_temps_managed_infra_repository(&repository) {
            continue;
        }
        by_repository.entry(repository).or_default().push(image);
    }

    let mut removable: Vec<&LocalImage> = Vec::new();
    for (_, mut repository_images) in by_repository {
        repository_images.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
        for image in repository_images
            .into_iter()
            .skip(policy.keep_newest_infra_images)
        {
            let re_pullable = !image.repo_digests.is_empty()
                && !image.repo_tags.iter().any(|tag| tag.ends_with(":dev"));
            if !re_pullable {
                continue;
            }
            let pinned_here = image
                .repo_tags
                .iter()
                .chain(image.repo_digests.iter())
                .any(|reference| pinned.contains(&reference.as_str()));
            let used = in_use.ids.contains(image.id.as_str())
                || image
                    .repo_tags
                    .iter()
                    .chain(image.repo_digests.iter())
                    .any(|reference| in_use.references.contains(reference.as_str()));
            if pinned_here || used {
                continue;
            }
            removable.push(image);
        }
    }

    removable.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    removable
        .into_iter()
        .take(policy.max_removals_per_run)
        .map(|image| image.id.clone())
        .collect()
}

/// Which database-free pruning passes a host runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalPrunePasses {
    /// Superseded infrastructure images only. The console prunes its
    /// deployment images with the database-backed retention policy instead.
    InfraOnly,
    /// A worker node: deployment images by the local rule, then superseded
    /// infrastructure images.
    WorkerNode,
}

/// List this daemon's containers and images once, then run the requested
/// database-free passes against them.
///
/// Fails closed: if either listing fails nothing is removed, because the
/// in-use check would be guessing.
pub(crate) async fn prune_local_images_without_database(
    docker_client: &Arc<dyn DockerClient>,
    policy: LocalImagePrunePolicy,
    passes: LocalPrunePasses,
) {
    // Containers first: one created after this listing makes its image "in
    // use" at removal time, and non-forced removal then refuses it.
    let containers = match docker_client.list_containers().await {
        Ok(containers) => containers,
        Err(e) => {
            error!(
                error = %e,
                "Could not list local containers; skipping local image pruning this run"
            );
            return;
        }
    };
    let images = match docker_client.list_images().await {
        Ok(images) => images,
        Err(e) => {
            error!(
                error = %e,
                "Could not list local images; skipping local image pruning this run"
            );
            return;
        }
    };

    if passes == LocalPrunePasses::WorkerNode {
        let tags = plan_node_deployment_image_removals(&images, &containers, &policy);
        if tags.is_empty() {
            debug!("No superseded registry-pulled deployment images on this node");
        } else {
            // Each reference with the image it named in this inventory, so
            // a tag a pull moves in the meantime is not removed.
            let selected: HashMap<&str, &LocalImage> = images
                .iter()
                .flat_map(|image| {
                    image
                        .removable_references()
                        .iter()
                        .map(move |reference| (reference.as_str(), image))
                })
                .collect();
            let sizes: HashMap<String, u64> = selected
                .iter()
                .map(|(reference, image)| (reference.to_string(), image.size_bytes))
                .collect();
            let removals: Vec<ImageRemoval> = tags
                .into_iter()
                .filter_map(|reference| {
                    let image_id = selected.get(reference.as_str())?.id.clone();
                    Some(ImageRemoval {
                        reference,
                        image_id,
                    })
                })
                .collect();
            let outcomes = docker_client.remove_images(&removals).await;
            summarize_removals("node_deployment_images", &outcomes, &sizes);
        }
    }

    let pinned = pinned_infra_image_references();
    let ids = plan_superseded_infra_image_removals(&images, &containers, &pinned, &policy);
    if ids.is_empty() {
        debug!("No superseded Temps infrastructure images to remove");
        return;
    }
    let sizes: HashMap<String, u64> = images
        .iter()
        .map(|image| (image.id.clone(), image.size_bytes))
        .collect();
    let removals: Vec<ImageRemoval> = ids.iter().map(|id| ImageRemoval::by_id(id)).collect();
    let outcomes = docker_client.remove_images(&removals).await;
    summarize_removals("superseded_infra_images", &outcomes, &sizes);
}

#[cfg(test)]
pub(crate) mod tests;
