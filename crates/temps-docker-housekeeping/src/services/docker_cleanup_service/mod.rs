// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker Cleanup Service
//!
//! Manages nightly cleanup of unused Docker images and build caches to save disk space.
//! Runs as a background task scheduled at 2 AM UTC daily.

use chrono::Timelike as _;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use super::local_image_pruning::{
    image_repository, prune_local_images_without_database, LocalImagePrunePolicy, LocalPrunePasses,
};

/// Trait for Docker operations (mockable for testing)
#[async_trait::async_trait]
pub trait DockerClient: Send + Sync {
    /// Remove unused Docker images
    async fn prune_images(&self, force: bool) -> Result<PruneStats, String>;

    /// Remove build cache unused for longer than the policy's age limit,
    /// then evict the least recently used records until the cache fits the
    /// policy's size cap.
    async fn prune_builder_cache(&self, policy: BuildCachePolicy) -> Result<String, String>;

    /// Remove each selected image by its reference (a `repo:tag`, a digest
    /// reference or an image ID), returning a per-image outcome in the same
    /// order. A reference is removed only while it still names the image
    /// ID the pass selected: a pull between the pass's inventory and the
    /// removal can repoint a tag at a newer image, which is then
    /// [`ImageDeleteStatus::Retained`] rather than deleted. Removal is never
    /// forced, so Docker itself refuses an image any container (running or
    /// stopped) still uses. Takes the whole batch (rather than one image per
    /// call) so the implementation opens a single Docker connection for the
    /// entire nightly pass instead of one per image.
    async fn remove_images(&self, removals: &[ImageRemoval]) -> Vec<ImageRemovalOutcome>;

    /// Every top-level image on the local daemon (`all=false`, so not the
    /// intermediate layers of a build). The retention pass only considers
    /// images that still exist here: once an image is removed its deployment
    /// rows keep naming it, so without this filter the oldest-first candidate
    /// window would refill with already-removed tags every night and never
    /// reach newer ones.
    async fn list_images(&self) -> Result<Vec<LocalImage>, DockerInventoryError>;

    /// Every container on the local daemon, running or stopped. An image any
    /// of them uses is never a removal candidate.
    async fn list_containers(&self) -> Result<Vec<LocalContainer>, DockerInventoryError>;
}

/// Why listing the local daemon's images or containers failed. Keeps the
/// underlying Docker error as its source instead of flattening it to text.
#[derive(Debug, thiserror::Error)]
pub enum DockerInventoryError {
    #[error("Failed to connect to Docker daemon to list local images and containers: {source}")]
    Connect {
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list local Docker images (all=false): {source}")]
    ListImages {
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list local Docker containers (all=true): {source}")]
    ListContainers {
        #[source]
        source: bollard::errors::Error,
    },
}

/// One image on the local daemon, as the image-pruning passes see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    /// Image ID (`sha256:...`).
    pub id: String,
    /// `repo:tag` references, without Docker's `<none>:<none>` placeholder.
    pub repo_tags: Vec<String>,
    /// `repo@sha256:...` references. Non-empty only for an image that came
    /// from (or was pushed to) a registry; an image pinned by digest and never
    /// tagged has only these, and shows up as `<none>` in `docker images`.
    pub repo_digests: Vec<String>,
    /// Image build time (Unix seconds), as recorded in the image config.
    pub created: i64,
    /// Size Docker reports for the image, shared layers included.
    pub size_bytes: u64,
}

impl LocalImage {
    /// The references a removal names this image by: its tags, or, for an
    /// image pulled by digest and never tagged, its digest references.
    ///
    /// Digests are only used when there is no tag: removing an image's last
    /// tag already drops its digest references from the same repository, so
    /// listing both would only produce a second, redundant removal.
    pub fn removable_references(&self) -> &[String] {
        if self.repo_tags.is_empty() {
            &self.repo_digests
        } else {
            &self.repo_tags
        }
    }
}

/// One container on the local daemon, running or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalContainer {
    /// The reference the container was created from (`repo:tag`,
    /// `repo@sha256:...`, or a bare image ID).
    pub image: String,
    /// The image ID the container runs, whatever its reference now points at.
    pub image_id: String,
    /// Whether Temps created it for a deployment (it carries the
    /// `sh.temps.deploy_id` label every deployment container gets).
    pub is_temps_deployment: bool,
}

/// Label every container Temps starts for a deployment carries.
pub const DEPLOYMENT_CONTAINER_LABEL: &str = "sh.temps.deploy_id";

/// Reference prefix reserved for images that exist only in the control
/// plane's own store (uploads, claimed daemon images): no registry serves it,
/// so such an image can never be pulled back. Mirrors
/// `temps_deployments::jobs::image_source::RESERVED_LOCAL_IMAGE_PREFIX`, which
/// this crate cannot depend on; a test there keeps the two equal.
pub const RESERVED_LOCAL_IMAGE_PREFIX: &str = "temps.internal/";

/// What happened when one image was asked to be removed by name, with a
/// plain non-forced `docker image rm`.
///
/// Distinct from `temps_docker_util::image_removal::ImageRemovalStatus`,
/// which reports the guarded removal used for deleted projects' images: that
/// one removes by a verified image ID and refuses an image carrying other
/// tags, so it never reports "untagged only". Here removing one of several
/// names is a normal outcome ([`ImageDeleteStatus::Untagged`]) that frees no
/// space, and the bytes-reclaimed summary depends on telling it apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDeleteStatus {
    /// Docker deleted the image.
    Deleted,
    /// Docker only removed this reference; another tag or digest still holds
    /// the image, so no space was freed.
    Untagged,
    /// Docker refused with a conflict (HTTP 409): a container still uses the
    /// image, or another repository references it. This is the intended
    /// safety net behind the passes' own in-use checks, not a failure.
    Retained { reason: String },
    /// The image was already gone (HTTP 404).
    NotFound,
    /// Any other failure (daemon unreachable, server error, ...).
    Failed { reason: String },
}

/// Result of attempting to remove one image during a pruning pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRemovalOutcome {
    pub image_name: String,
    pub status: ImageDeleteStatus,
}

impl ImageRemovalOutcome {
    /// Classify a Docker `remove_image` result.
    pub fn from_docker(
        image_name: &str,
        result: Result<Vec<bollard::models::ImageDeleteResponseItem>, bollard::errors::Error>,
    ) -> Self {
        let status = match result {
            Ok(items) if items.iter().any(|item| item.deleted.is_some()) => {
                ImageDeleteStatus::Deleted
            }
            Ok(_) => ImageDeleteStatus::Untagged,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                message,
            }) => ImageDeleteStatus::Retained { reason: message },
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => ImageDeleteStatus::NotFound,
            Err(error) => ImageDeleteStatus::Failed {
                reason: format!("Failed to remove image '{}': {}", image_name, error),
            },
        };
        Self {
            image_name: image_name.to_string(),
            status,
        }
    }
}

/// One image a pass selected for removal: the reference to remove it by,
/// and the local image ID that reference named when the pass decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRemoval {
    pub reference: String,
    pub image_id: String,
}

impl ImageRemoval {
    /// Remove `image_id` by the ID itself.
    pub fn by_id(image_id: &str) -> Self {
        Self {
            reference: image_id.to_string(),
            image_id: image_id.to_string(),
        }
    }

    /// Whether the reference has to be checked against the selected ID
    /// before removal. An image ID always names the same image; a tag can
    /// be moved to another one.
    pub fn needs_identity_check(&self) -> bool {
        self.reference != self.image_id
    }

    /// The outcome to report instead of removing, when the reference now
    /// names `current_id` rather than the selected image. `None` when it
    /// still names the selected image.
    pub fn moved_outcome(&self, current_id: &str) -> Option<ImageRemovalOutcome> {
        (current_id != self.image_id).then(|| ImageRemovalOutcome {
            image_name: self.reference.clone(),
            status: ImageDeleteStatus::Retained {
                reason: format!(
                    "'{}' now names image {} instead of {}, the image selected for removal",
                    self.reference, current_id, self.image_id
                ),
            },
        })
    }
}

/// Totals of one pruning pass, logged once at the end of it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImageRemovalSummary {
    pub deleted: usize,
    pub untagged: usize,
    pub retained: usize,
    pub not_found: usize,
    pub failed: usize,
    /// Sum of Docker's reported size for each deleted image. Layers shared
    /// with images that remain are not actually freed, so this is an upper
    /// bound on the disk space reclaimed.
    pub bytes_reclaimed: u64,
}

/// Log every outcome of a removal batch and the pass's totals.
///
/// A refusal because the image is still in use (or already gone) is the
/// expected safety net and is logged at `debug`; any other failure is a
/// `warn` naming the image and the reason. Neither aborts the rest of the
/// batch: by the time this runs every removal has already been attempted.
pub(crate) fn summarize_removals(
    pass: &'static str,
    outcomes: &[ImageRemovalOutcome],
    sizes: &HashMap<String, u64>,
) -> ImageRemovalSummary {
    let mut summary = ImageRemovalSummary::default();
    for outcome in outcomes {
        match &outcome.status {
            ImageDeleteStatus::Deleted => {
                summary.deleted += 1;
                summary.bytes_reclaimed = summary
                    .bytes_reclaimed
                    .saturating_add(sizes.get(&outcome.image_name).copied().unwrap_or(0));
                debug!(pass, image_name = %outcome.image_name, "Removed image");
            }
            ImageDeleteStatus::Untagged => {
                summary.untagged += 1;
                debug!(
                    pass,
                    image_name = %outcome.image_name,
                    "Removed image reference; another tag still holds the image"
                );
            }
            ImageDeleteStatus::Retained { reason } => {
                summary.retained += 1;
                debug!(
                    pass,
                    image_name = %outcome.image_name,
                    reason = %reason,
                    "Image retained: Docker reports it is still in use"
                );
            }
            ImageDeleteStatus::NotFound => {
                summary.not_found += 1;
                debug!(pass, image_name = %outcome.image_name, "Image already removed");
            }
            ImageDeleteStatus::Failed { reason } => {
                summary.failed += 1;
                warn!(
                    pass,
                    image_name = %outcome.image_name,
                    error = %reason,
                    "Could not remove image"
                );
            }
        }
    }
    info!(
        pass,
        deleted = summary.deleted,
        untagged = summary.untagged,
        retained_in_use = summary.retained,
        already_gone = summary.not_found,
        failed = summary.failed,
        bytes_reclaimed = summary.bytes_reclaimed,
        mb_reclaimed = summary.bytes_reclaimed / (1024 * 1024),
        "Image pruning pass finished"
    );
    summary
}

/// What the nightly build cache prune keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCachePolicy {
    /// Records unused for longer than this are removed regardless of size.
    pub max_unused_days: i64,
    /// Total cache size to evict down to (least recently used first);
    /// `None` disables the size cap.
    pub max_size_bytes: Option<u64>,
}

impl Default for BuildCachePolicy {
    fn default() -> Self {
        Self::from(&temps_config::BuildLimitsSettings::default())
    }
}

impl From<&temps_config::BuildLimitsSettings> for BuildCachePolicy {
    fn from(settings: &temps_config::BuildLimitsSettings) -> Self {
        Self {
            max_unused_days: i64::from(settings.effective_build_cache_max_unused_days()),
            max_size_bytes: settings.build_cache_max_bytes(),
        }
    }
}

/// How to bring an over-cap build cache back under the cap with BuildKit's
/// only age-based knob: the `until` prune filter, which removes every
/// unused record whose last activity is older than the given duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCacheEviction {
    /// Value for the `until` filter, in whole seconds.
    pub keep_secs: i64,
    pub total_bytes: u64,
    /// Bytes the eviction is expected to free.
    pub evicted_bytes: u64,
}

/// Pick the `until` duration that evicts the least recently used, unused
/// build cache records until the total fits in `max_bytes`.
///
/// This is computed here rather than left to the daemon because Docker
/// before API 1.48 (Docker 27 and older) has no size-bounded prune that
/// works with `all`, and the newer `max-used-space` option is silently
/// ignored by those daemons. The `until` filter is understood by every
/// BuildKit-enabled daemon. Records a build is using are never counted as
/// evictable; records with no timestamp cannot be targeted by age.
/// Returns `None` when the cache already fits or nothing evictable exists.
pub fn plan_build_cache_eviction(
    records: &[crate::services::BuildCacheRecord],
    max_bytes: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<BuildCacheEviction> {
    let total_bytes: u64 = records.iter().map(|r| r.size_bytes).sum();
    if total_bytes <= max_bytes {
        return None;
    }
    let mut evictable: Vec<(chrono::DateTime<chrono::Utc>, u64)> = records
        .iter()
        .filter(|r| !r.in_use)
        .filter_map(|r| r.last_activity().map(|t| (t, r.size_bytes)))
        .collect();
    evictable.sort_by_key(|(t, _)| *t);

    let mut remaining = total_bytes;
    let mut cutoff = None;
    for (last_activity, size) in &evictable {
        // Everything at or before the cutoff goes, so include ties.
        if remaining <= max_bytes && cutoff.is_some_and(|c| c < *last_activity) {
            break;
        }
        remaining = remaining.saturating_sub(*size);
        cutoff = Some(*last_activity);
    }
    let cutoff = cutoff?;
    // `until` keeps records newer than now - keep; a second less than the
    // cutoff's age makes the cutoff record itself fall on the evict side.
    let keep_secs = ((now - cutoff).num_seconds() - 1).max(1);
    Some(BuildCacheEviction {
        keep_secs,
        total_bytes,
        evicted_bytes: total_bytes - remaining,
    })
}

/// Statistics from Docker prune operations
#[derive(Debug, Clone)]
pub struct PruneStats {
    pub images_deleted: u64,
    pub space_reclaimed_mb: u64,
}

/// Default Docker client implementation using the Docker daemon
#[derive(Clone)]
pub struct DefaultDockerClient;

#[async_trait::async_trait]
impl DockerClient for DefaultDockerClient {
    async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
        use bollard::query_parameters::PruneImagesOptionsBuilder;
        use bollard::Docker;
        use std::collections::HashMap;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;

        // Only prune images older than 7 days (168 hours)
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert("until".to_string(), vec!["168h".to_string()]);
        // Also only prune dangling images (not tagged)
        filters.insert("dangling".to_string(), vec!["true".to_string()]);

        let options = PruneImagesOptionsBuilder::default()
            .filters(&filters)
            .build();

        match docker.prune_images(Some(options)).await {
            Ok(result) => {
                let space_mb = result.space_reclaimed.unwrap_or(0) / (1024 * 1024);
                let count = result.images_deleted.map(|v| v.len()).unwrap_or(0) as u64;
                Ok(PruneStats {
                    images_deleted: count,
                    space_reclaimed_mb: space_mb as u64,
                })
            }
            Err(e) => Err(format!("Failed to prune images: {}", e)),
        }
    }

    async fn remove_images(&self, removals: &[ImageRemoval]) -> Vec<ImageRemovalOutcome> {
        use bollard::Docker;

        // One connection for the whole batch.
        let docker = match Docker::connect_with_unix_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                let reason = format!("Failed to connect to Docker daemon: {}", e);
                return removals
                    .iter()
                    .map(|removal| ImageRemovalOutcome {
                        image_name: removal.reference.clone(),
                        status: ImageDeleteStatus::Failed {
                            reason: reason.clone(),
                        },
                    })
                    .collect();
            }
        };

        let mut outcomes = Vec::with_capacity(removals.len());
        for removal in removals {
            let image_name = removal.reference.as_str();
            // The pass chose this reference from an earlier inventory. If a
            // pull has since moved it to another image, removing it by name
            // would delete that newer image instead.
            if removal.needs_identity_check() {
                match docker.inspect_image(image_name).await {
                    Ok(current) => {
                        if let Some(moved) =
                            removal.moved_outcome(current.id.as_deref().unwrap_or_default())
                        {
                            outcomes.push(moved);
                            continue;
                        }
                    }
                    Err(error) => {
                        outcomes.push(ImageRemovalOutcome::from_docker(image_name, Err(error)));
                        continue;
                    }
                }
            }

            // Non-forced: Docker refuses while any container (running or
            // stopped) still references the image. That is deliberate — it is
            // the last line of defence behind the passes' own in-use checks.
            let result = docker
                .remove_image(
                    image_name,
                    Some(bollard::query_parameters::RemoveImageOptions {
                        force: false,
                        ..Default::default()
                    }),
                    None,
                )
                .await;

            outcomes.push(ImageRemovalOutcome::from_docker(image_name, result));
        }

        outcomes
    }

    async fn list_images(&self) -> Result<Vec<LocalImage>, DockerInventoryError> {
        use bollard::Docker;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|source| DockerInventoryError::Connect { source })?;
        let images = docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|source| DockerInventoryError::ListImages { source })?;

        Ok(images
            .into_iter()
            .map(|image| LocalImage {
                id: image.id,
                repo_tags: image
                    .repo_tags
                    .into_iter()
                    .filter(|tag| tag != "<none>:<none>")
                    .collect(),
                repo_digests: image
                    .repo_digests
                    .into_iter()
                    .filter(|digest| digest != "<none>@<none>")
                    .collect(),
                created: image.created,
                size_bytes: u64::try_from(image.size).unwrap_or(0),
            })
            .collect())
    }

    async fn list_containers(&self) -> Result<Vec<LocalContainer>, DockerInventoryError> {
        use bollard::Docker;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|source| DockerInventoryError::Connect { source })?;
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|source| DockerInventoryError::ListContainers { source })?;

        Ok(containers
            .into_iter()
            .map(|container| LocalContainer {
                image: container.image.unwrap_or_default(),
                image_id: container.image_id.unwrap_or_default(),
                is_temps_deployment: container
                    .labels
                    .as_ref()
                    .is_some_and(|labels| labels.contains_key(DEPLOYMENT_CONTAINER_LABEL)),
            })
            .collect())
    }

    async fn prune_builder_cache(&self, policy: BuildCachePolicy) -> Result<String, String> {
        use bollard::Docker;

        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;

        // Age limit first: cheap, and often enough on its own.
        let age_duration = format!("{}h", policy.max_unused_days.max(1) * 24);
        let mut summary = prune_build_cache_until(&docker, &age_duration)
            .await
            .map_err(|e| {
                format!(
                    "Failed to prune build cache unused for {} day(s): {}",
                    policy.max_unused_days, e
                )
            })?;

        let Some(max_bytes) = policy.max_size_bytes else {
            return Ok(summary);
        };
        let records = crate::services::DockerDiskUsageService::local()
            .build_cache_records()
            .await
            .map_err(|e| {
                format!(
                    "{}; could not measure the build cache for the {} MB cap: {}",
                    if summary.is_empty() {
                        "age prune found nothing"
                    } else {
                        summary.as_str()
                    },
                    max_bytes / (1024 * 1024),
                    e
                )
            })?;
        let Some(eviction) = plan_build_cache_eviction(&records, max_bytes, chrono::Utc::now())
        else {
            return Ok(summary);
        };
        info!(
            total_mb = eviction.total_bytes / (1024 * 1024),
            cap_mb = max_bytes / (1024 * 1024),
            evict_mb = eviction.evicted_bytes / (1024 * 1024),
            keep_secs = eviction.keep_secs,
            "Docker build cache is over its size cap; evicting least recently used records"
        );
        let capped = prune_build_cache_until(&docker, &format!("{}s", eviction.keep_secs))
            .await
            .map_err(|e| {
                format!(
                    "Failed to prune build cache down to the {} MB cap: {}",
                    max_bytes / (1024 * 1024),
                    e
                )
            })?;
        if !capped.is_empty() {
            let capped = format!("size cap: {capped}");
            summary = if summary.is_empty() {
                capped
            } else {
                format!("{summary}; {capped}")
            };
        }
        Ok(summary)
    }
}

/// One `POST /build/prune` with `all=true` and an `until` age filter. Returns
/// a human summary, empty when nothing was removed.
async fn prune_build_cache_until(
    docker: &bollard::Docker,
    duration: &str,
) -> Result<String, bollard::errors::Error> {
    use bollard::query_parameters::PruneBuildOptionsBuilder;
    use std::collections::HashMap;

    // Build filters with "until" to prune cache older than the specified duration
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("until".to_string(), vec![duration.to_string()]);

    let options = PruneBuildOptionsBuilder::default()
        // Without `all`, the Build/prune API only removes cache marked
        // "dangling" (not shared by any remaining build lineage) --
        // the same distinction as `docker image prune` vs `-a`. Most
        // build cache (e.g. per-build COPY/RUN layers with unique
        // content) is never dangling, so leaving this unset meant the
        // nightly cleanup reclaimed almost none of it regardless of
        // the `until` age filter, letting the cache grow unbounded.
        .all(true)
        .filters(&filters)
        .build();

    let result = docker.prune_build(Some(options)).await?;
    let space_mb = result.space_reclaimed.unwrap_or(0) / (1024 * 1024);
    let caches_deleted = result.caches_deleted.map(|v| v.len()).unwrap_or(0);

    if caches_deleted > 0 || space_mb > 0 {
        Ok(format!(
            "removed {} build cache entries, freed {} MB",
            caches_deleted, space_mb
        ))
    } else {
        Ok(String::new())
    }
}

/// Calculate seconds until the next occurrence of `cleanup_hour` (UTC).
/// Shared by `DockerCleanupService` (console) and `DockerOnlyCleanupScheduler`
/// (worker agents) so both run on the same nightly cadence.
fn seconds_until_next_cleanup(cleanup_hour: u32) -> u64 {
    let now = chrono::Utc::now();

    // Calculate target time (today at cleanup_hour). `with_hour` etc. only
    // return None for an out-of-range component; every caller already
    // constrains `cleanup_hour` to 0..24 via `% 24`, but this runs on every
    // scheduler tick for the life of the process (console and every worker
    // agent), so a future caller skipping that guard must not crash the
    // spawned task -- retry in 24h instead of panicking.
    let target_time = match now
        .with_hour(cleanup_hour)
        .and_then(|t| t.with_minute(0))
        .and_then(|t| t.with_second(0))
    {
        Some(t) => t,
        None => {
            error!(
                cleanup_hour,
                "Invalid cleanup hour; docker cleanup scheduler will retry in 24h"
            );
            return 24 * 3600;
        }
    };

    let next_cleanup = if target_time > now {
        // Cleanup time hasn't passed today
        target_time
    } else {
        // Cleanup time already passed today, schedule for tomorrow
        target_time + chrono::Duration::days(1)
    };

    let duration = next_cleanup - now;
    duration.num_seconds().max(0) as u64
}

/// Prune unused Docker images and stale build cache via `docker_client`.
/// Shared by `DockerCleanupService` (console, which also cleans up the
/// DB-backed static asset cache) and `DockerOnlyCleanupScheduler` (worker
/// agents, which have no database connection).
async fn perform_docker_prune(
    docker_client: &Arc<dyn DockerClient>,
    build_cache_policy: BuildCachePolicy,
) {
    // Cleanup unused images
    match docker_client.prune_images(true).await {
        Ok(stats) => {
            if stats.images_deleted > 0 {
                info!(
                    "✅ Removed {} unused Docker images, freed {} MB",
                    stats.images_deleted, stats.space_reclaimed_mb
                );
            } else {
                info!("✅ No unused Docker images to remove");
            }
        }
        Err(e) => {
            error!("❌ Failed to prune Docker images: {}", e);
        }
    }

    // Cleanup old build cache
    match docker_client.prune_builder_cache(build_cache_policy).await {
        Ok(output) => {
            // Parse output for statistics
            if output.contains("freed") || output.contains("removed") {
                info!("✅ Docker build cache cleanup completed: {}", output.trim());
            } else if output.is_empty() {
                info!("✅ No old Docker build cache to remove");
            } else {
                debug!("Docker build cache cleanup output: {}", output);
            }
        }
        Err(e) => {
            // Builder prune might not be available in all Docker versions
            warn!(
                "⚠️ Failed to prune Docker builder cache (may not be available): {}",
                e
            );
        }
    }
}

/// Lightweight Docker-only cleanup scheduler for hosts without a database
/// connection — namely worker agent nodes (`temps agent`), which are a
/// separate process from the console and never register the plugin system
/// that wires up `DockerCleanupService`. Worker nodes still build images
/// and accumulate build cache locally, so they need the same nightly
/// image + build-cache prune; they just skip the DB-backed static asset
/// cache and chunk cleanup steps, which are console-only concerns.
///
/// Without a database a worker cannot apply the console's image retention
/// policy, so it prunes old deployment images by a stricter local rule
/// ([`crate::services::local_image_pruning::plan_node_deployment_image_removals`])
/// and superseded Temps infrastructure images
/// ([`crate::services::local_image_pruning::plan_superseded_infra_image_removals`]).
pub struct DockerOnlyCleanupScheduler {
    docker_client: Arc<dyn DockerClient>,
    /// Hour of day (UTC) to run cleanup (default: 2 AM)
    cleanup_hour: u32,
    /// Build cache age limit and size cap. Worker agents have no database to
    /// read the operator's `build_limits` from, so they apply the defaults
    /// (7 days unused, 10 GB).
    build_cache_policy: BuildCachePolicy,
    /// Rollback floor and per-run cap of the local image pruning.
    local_image_policy: LocalImagePrunePolicy,
}

impl DockerOnlyCleanupScheduler {
    pub fn new(docker_client: Arc<dyn DockerClient>) -> Self {
        Self {
            docker_client,
            cleanup_hour: 2, // 2 AM UTC
            build_cache_policy: BuildCachePolicy::default(),
            local_image_policy: LocalImagePrunePolicy::default(),
        }
    }

    pub fn with_cleanup_hour(mut self, hour: u32) -> Self {
        self.cleanup_hour = hour % 24;
        self
    }

    pub fn with_max_cache_age_days(mut self, days: i64) -> Self {
        self.build_cache_policy.max_unused_days = days;
        self
    }

    pub fn with_build_cache_policy(mut self, policy: BuildCachePolicy) -> Self {
        self.build_cache_policy = policy;
        self
    }

    pub fn with_local_image_policy(mut self, policy: LocalImagePrunePolicy) -> Self {
        self.local_image_policy = policy;
        self
    }

    /// One nightly pass: dangling images and build cache, then old
    /// deployment images and superseded infrastructure images.
    async fn perform_cleanup(&self) {
        info!("🧹 Starting nightly Docker cleanup (agent node)");
        perform_docker_prune(&self.docker_client, self.build_cache_policy).await;
        prune_local_images_without_database(
            &self.docker_client,
            self.local_image_policy,
            LocalPrunePasses::WorkerNode,
        )
        .await;
        info!("Nightly Docker cleanup completed (agent node)");
    }

    /// Start the cleanup scheduler (blocking, should be spawned in a tokio task).
    pub async fn start_cleanup_scheduler(&self) {
        info!(
            "Docker cleanup scheduler started (agent node, cleanup hour: {}:00 UTC)",
            self.cleanup_hour
        );

        loop {
            let seconds_until_cleanup = seconds_until_next_cleanup(self.cleanup_hour);
            let hours = seconds_until_cleanup / 3600;
            let minutes = (seconds_until_cleanup % 3600) / 60;

            debug!(
                "Next Docker cleanup scheduled in {} hours {} minutes",
                hours, minutes
            );

            sleep(Duration::from_secs(seconds_until_cleanup)).await;

            self.perform_cleanup().await;

            // Sleep for 1 minute to avoid running cleanup multiple times in the same minute
            sleep(Duration::from_secs(60)).await;
        }
    }
}

/// SQL predicate over a `deployments d` row: its image was handed to this
/// server rather than built or pulled by it, so nothing can produce it again.
///
/// - an upload (`POST .../deployments/upload`, trigger `image_upload`, or
///   `imageUploadedLocally`/`uploadedImageId` in the metadata);
/// - a claimed local image (a reserved `temps.internal/` reference);
/// - an external-image ID with no registry reference recorded next to it:
///   the image is known only by a local ID, with nothing to pull it from.
///
/// Every field is read through `COALESCE`/`NULLIF` so a NULL or empty value
/// never reads as "not an upload". No user input is interpolated.
const SQL_HANDED_OVER_IMAGE: &str = r#"(
       COALESCE(d.context_vars ->> 'trigger', '') = 'image_upload'
    OR COALESCE(d.metadata ->> 'imageUploadedLocally', 'false') <> 'false'
    OR NULLIF(d.metadata ->> 'uploadedImageId', '') IS NOT NULL
    OR COALESCE(d.metadata ->> 'externalImageRef', '') LIKE 'temps.internal/%'
    OR (d.metadata ->> 'externalImageId' IS NOT NULL
        AND NULLIF(d.metadata ->> 'externalImageRef', '') IS NULL)
)"#;

/// SQL predicate over `deployments d` joined to `projects p`: this deployment
/// row makes its image unrecoverable, so it must never be pruned.
///
/// Either the image was handed over ([`SQL_HANDED_OVER_IMAGE`]), or it
/// belongs to a non-git project and is not a registry pull. The second arm is
/// deliberately broader than strictly necessary: a `Manual`-source project
/// can deploy a rebuildable git-built image, but its row doesn't say so.
///
/// Rollback and promotion rows record no image metadata of their own (their
/// provenance is that of the deployment they reuse), so they are neutral
/// here rather than protecting every image they ever reused. That is safe
/// because the original deployment always takes part in the same decision:
/// a candidate exists only because a build record or a registry deployment
/// names it, and that row is one of its references.
fn sql_unrecoverable_deployment() -> String {
    format!(
        r#"COALESCE(
            {SQL_HANDED_OVER_IMAGE}
            OR (
                p.source_type <> 'git'
                AND NULLIF(d.metadata ->> 'externalImageRef', '') IS NULL
                AND COALESCE(d.context_vars ->> 'trigger', '') NOT IN ('rollback', 'promotion')
            ),
            TRUE
        )"#
    )
}

/// SQL predicate over a `deployments d` row: a registry image deployment whose
/// image can be pulled again from the reference it recorded.
fn sql_registry_pull() -> String {
    format!(
        r#"(NULLIF(d.metadata ->> 'externalImageRef', '') IS NOT NULL
            AND NOT {SQL_HANDED_OVER_IMAGE})"#
    )
}

/// The one spelling an image reference is compared by, so that every way of
/// writing a Docker Hub name matches the others: `nginx:1`,
/// `docker.io/nginx:1`, `index.docker.io/library/nginx:1` and
/// `docker.io/library/nginx:1` all give `docker.io/library/nginx:1`, and
/// `org/app:1` gives `docker.io/org/app:1`. Docker resolves all of them to the
/// same local image, so a protection recorded under one spelling must cover a
/// candidate recorded under another.
///
/// A reference that names a registry host (its first component has a `.` or
/// `:`, or is `localhost`) keeps its registry, and a bare image ID is
/// returned unchanged.
///
/// A reference with neither a tag nor a digest gets `:latest`, the tag
/// Docker pulls and stores it under: `nginx` gives
/// `docker.io/library/nginx:latest`, so a deployment that recorded `nginx`
/// matches the local `nginx:latest` tag.
///
/// [`sql_canonical_image_reference`] is the same rule in SQL; keep them in
/// step.
pub fn canonical_image_reference(reference: &str) -> String {
    if reference.is_empty() || reference.starts_with("sha256:") {
        return reference.to_string();
    }
    let canonical = canonical_repository_spelling(reference);
    let has_tag = canonical
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains(':'));
    if canonical.contains('@') || has_tag {
        canonical
    } else {
        format!("{canonical}:latest")
    }
}

/// [`canonical_image_reference`] before the default tag: the registry and
/// repository spelled one way, tag or digest kept as written.
fn canonical_repository_spelling(reference: &str) -> String {
    let reference = match reference.strip_prefix("index.docker.io/") {
        Some(rest) => format!("docker.io/{rest}"),
        None => reference.to_string(),
    };
    if let Some(rest) = reference.strip_prefix("docker.io/") {
        return if rest.contains('/') {
            reference
        } else {
            format!("docker.io/library/{rest}")
        };
    }
    match reference.split_once('/') {
        None => format!("docker.io/library/{reference}"),
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => {
            reference
        }
        Some(_) => format!("docker.io/{reference}"),
    }
}

/// SQL expression giving [`canonical_image_reference`] of `expr`, a column
/// expression (never user input). NULL stays NULL.
fn sql_canonical_image_reference(expr: &str) -> String {
    let c = sql_canonical_repository_spelling(expr);
    // A colon after the last `/` is a tag; `@` is a digest.
    format!(
        r"(CASE
            WHEN {c} = '' OR {c} LIKE 'sha256:%'
              OR strpos({c}, '@') > 0 OR {c} ~ ':[^/]*$' THEN {c}
            ELSE {c} || ':latest'
        END)"
    )
}

/// SQL expression giving `canonical_repository_spelling` of `expr`.
fn sql_canonical_repository_spelling(expr: &str) -> String {
    let n = format!(r"regexp_replace({expr}, '^index\.docker\.io/', 'docker.io/')");
    format!(
        r"(CASE
            WHEN {n} = '' OR {n} LIKE 'sha256:%' THEN {n}
            WHEN {n} LIKE 'docker.io/%' THEN
                CASE WHEN strpos(substr({n}, 11), '/') = 0
                     THEN 'docker.io/library/' || substr({n}, 11)
                     ELSE {n}
                END
            WHEN strpos({n}, '/') = 0 THEN 'docker.io/library/' || {n}
            WHEN split_part({n}, '/', 1) ~ '[.:]'
              OR split_part({n}, '/', 1) = 'localhost' THEN {n}
            ELSE 'docker.io/' || {n}
        END)"
    )
}

/// SQL expression giving the repository of a canonical image key
/// ([`sql_canonical_image_reference`]): the key without its `@digest` or
/// `:tag`. On two canonical keys, equality agrees with comparing their
/// [`image_repository`] in Rust.
fn sql_canonical_key_repository(expr: &str) -> String {
    format!(r"regexp_replace(regexp_replace({expr}, '@.*$', ''), ':[^:/]*$', '')")
}

/// SQL expression normalising an image ID column expression to the
/// `sha256:<hex>` form Docker lists, so a row that recorded the bare hex
/// matches too. Empty and NULL give NULL.
fn sql_normalized_image_id(expr: &str) -> String {
    format!(
        r"(CASE
            WHEN NULLIF({expr}, '') IS NULL THEN NULL
            WHEN {expr} LIKE 'sha256:%' THEN {expr}
            ELSE 'sha256:' || {expr}
        END)"
    )
}

/// SQL scalar subquery: the local image ID the registry pull of deployment
/// `deployment_id` (a column expression) recorded, or NULL. A mutable tag
/// (`app:latest`) moves on to newer images, so this ID — not the tag — is
/// what still identifies the image that deployment actually ran.
fn sql_pulled_image_id(deployment_id: &str) -> String {
    format!(
        r"(SELECT {image_id}
           FROM deployment_jobs pj
           WHERE pj.deployment_id = {deployment_id}
             AND pj.job_type = 'PullExternalImageJob'
             AND NULLIF(pj.outputs ->> 'image_id', '') IS NOT NULL
           ORDER BY pj.id DESC
           LIMIT 1)",
        image_id = sql_normalized_image_id("pj.outputs ->> 'image_id'"),
    )
}

/// The local images deployment retention may act on, indexed by every way a
/// deployment can name one: any of its tags or digest references, in any
/// Docker Hub spelling ([`canonical_image_reference`]), or its image ID.
///
/// Retention decides per image, not per name: `app:v1` and `app@sha256:...`
/// are one image, and removing its last tag removes its digest references
/// too, so a protection under either name must cover both.
struct LocalImageIndex {
    images: HashMap<String, LocalImage>,
    ids_by_key: HashMap<String, String>,
}

impl LocalImageIndex {
    fn new(images: Vec<LocalImage>) -> Self {
        let mut ids_by_key = HashMap::new();
        for image in &images {
            for reference in image.repo_tags.iter().chain(image.repo_digests.iter()) {
                ids_by_key.insert(canonical_image_reference(reference), image.id.clone());
            }
        }
        let images = images
            .into_iter()
            .map(|image| (image.id.clone(), image))
            .collect();
        Self { images, ids_by_key }
    }

    fn is_empty(&self) -> bool {
        self.ids_by_key.is_empty()
    }

    /// `[[image_id, reference, removable_as], ...]` for every tag and digest
    /// reference, the candidate query's view of this daemon.
    ///
    /// `removable_as` says how [`Self::removal_names`] may remove the image
    /// by that reference: `"tag"` when its key matches a recorded name,
    /// `"digest"` when its repository matches one (only for an untagged
    /// image), `null` when never. The query uses it to drop, before its
    /// `LIMIT`, an image no recorded name would remove, so such images
    /// cannot fill every run's window.
    fn query_input(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.images
                .values()
                .flat_map(|image| {
                    let untagged = image.repo_tags.is_empty();
                    let tags = image
                        .repo_tags
                        .iter()
                        .map(|reference| serde_json::json!([image.id, reference, "tag"]));
                    let digests = image.repo_digests.iter().map(move |reference| {
                        let removable_as = untagged.then_some("digest");
                        serde_json::json!([image.id, reference, removable_as])
                    });
                    tags.chain(digests)
                })
                .collect(),
        )
    }

    /// The local image `name` (a reference in any spelling, or an image ID)
    /// identifies, if it is on this daemon.
    fn image_id_for(&self, name: &str) -> Option<&str> {
        if let Some(image) = self.images.get(name) {
            return Some(image.id.as_str());
        }
        self.ids_by_key
            .get(&canonical_image_reference(name))
            .map(String::as_str)
    }

    /// The references to remove `image_id` by, given the canonical names
    /// deployments recorded for it.
    ///
    /// Only names a deployment recorded are removed, so a tag someone else
    /// put on the same image survives (and keeps the image). An untagged
    /// image (a superseded `app:latest`, or one pulled by digest) is named by
    /// its digest references in the repositories deployments named.
    fn removal_names(&self, image_id: &str, named_keys: &[String]) -> Vec<String> {
        let Some(image) = self.images.get(image_id) else {
            return Vec::new();
        };
        let named: HashSet<&str> = named_keys.iter().map(String::as_str).collect();
        if !image.repo_tags.is_empty() {
            return image
                .repo_tags
                .iter()
                .filter(|tag| named.contains(canonical_image_reference(tag).as_str()))
                .cloned()
                .collect();
        }
        let repositories: HashSet<String> = named_keys
            .iter()
            .filter_map(|key| image_repository(key))
            .collect();
        image
            .repo_digests
            .iter()
            .filter(|digest| {
                image_repository(digest)
                    .is_some_and(|repository| repositories.contains(&repository))
            })
            .cloned()
            .collect()
    }

    fn size_of(&self, image_id: &str) -> u64 {
        self.images
            .get(image_id)
            .map_or(0, |image| image.size_bytes)
    }
}

/// Docker cleanup service that runs nightly
pub struct DockerCleanupService {
    docker_client: Arc<dyn DockerClient>,
    db: Arc<temps_database::DbConnection>,
    file_store: Arc<dyn temps_file_store::FileStore>,
    /// Hour of day (UTC) to run cleanup (default: 2 AM)
    cleanup_hour: u32,
    /// Build cache age limit and size cap used when no `ConfigService` is
    /// wired up or the settings row cannot be read. With a `ConfigService`,
    /// every run re-reads `AppSettings.build_limits` instead.
    build_cache_policy: BuildCachePolicy,
    /// Base directory for static files (for persisted chunks cleanup)
    static_dir: Option<PathBuf>,
    /// Maximum age of persisted chunk directories in hours (default: 24)
    max_chunk_age_hours: u64,
    /// Maximum age of static asset cache entries in days (default: 7)
    max_asset_cache_age_days: i64,
    /// Number of most-recent deployment images to always keep per
    /// project+environment, regardless of age (default: 5)
    keep_recent_deployment_images: u64,
    /// Maximum number of stale deployment images to remove in a single
    /// nightly run (default: 500). Bounds memory and Docker API calls on
    /// installs with very large deployment histories; any remainder is
    /// picked up on the following night's run.
    max_deployment_images_per_run: u64,
    /// System-wide default: how many hours to keep a built deployment image before
    /// it is eligible for removal. Projects can override this via their
    /// `image_retention_hours` column. Sourced from
    /// `AppSettings.image_retention.default_hours` at startup.
    default_image_retention_hours: i64,
    /// When false, the retention pass is skipped entirely and built images are
    /// kept forever. Sourced from `AppSettings.image_retention.enabled`.
    image_retention_enabled: bool,
    /// When set, `prune_old_deployment_images` re-reads `image_retention`
    /// from this service at the start of every run instead of relying on the
    /// `image_retention_enabled`/`default_image_retention_hours` snapshot
    /// taken at construction. Without this, an operator disabling retention
    /// via the settings UI would have no effect until the process restarts —
    /// `ConfigService::get_settings` is TTL-cached (5s, NOTIFY-backed), so
    /// this costs nothing on a job that runs once nightly. `None` in tests
    /// that construct the service directly, which fall back to the
    /// constructor/builder-supplied values.
    config_service: Option<Arc<temps_config::ConfigService>>,
    /// (enabled, default_hours) from the most recent successful read —
    /// either the constructor/builder snapshot, or the latest live read via
    /// `config_service`. Used as the fallback on a *failed* live read
    /// instead of always reverting to the boot-time snapshot: an operator
    /// who disabled retention, followed by a transient settings-read error,
    /// must not have deletions silently resume under the stale boot policy.
    last_known_image_retention: std::sync::Mutex<(bool, i64)>,
    /// The sweep of images whose project no longer exists, on this host
    /// and on reachable worker nodes (`orphaned_images`).
    orphan_sweeper: super::orphaned_images::OrphanImageSweeper,
}

impl DockerCleanupService {
    pub fn new(
        docker_client: Arc<dyn DockerClient>,
        db: Arc<temps_database::DbConnection>,
        file_store: Arc<dyn temps_file_store::FileStore>,
    ) -> Self {
        let db_for_sweeper = db.clone();
        Self {
            docker_client,
            db,
            file_store,
            cleanup_hour: 2, // 2 AM UTC
            build_cache_policy: BuildCachePolicy::default(),
            static_dir: None,
            max_chunk_age_hours: 24,
            max_asset_cache_age_days: 7,
            keep_recent_deployment_images: 5,
            max_deployment_images_per_run: 500,
            // Mirrors ImageRetentionSettings::default(). Deleting an image
            // makes rollback to that deployment impossible, so this is a
            // rollback window (14 days), not a cache TTL.
            default_image_retention_hours: 336,
            image_retention_enabled: true,
            config_service: None,
            last_known_image_retention: std::sync::Mutex::new((true, 336)),
            orphan_sweeper: super::orphaned_images::OrphanImageSweeper::new(db_for_sweeper),
        }
    }

    /// How images of deleted projects are swept (this host, worker nodes,
    /// the data directory holding the install's owner ID).
    pub fn with_orphan_sweeper(
        mut self,
        sweeper: super::orphaned_images::OrphanImageSweeper,
    ) -> Self {
        self.orphan_sweeper = sweeper;
        self
    }

    pub fn with_static_dir(mut self, static_dir: PathBuf) -> Self {
        self.static_dir = Some(static_dir);
        self
    }

    pub fn with_cleanup_hour(mut self, hour: u32) -> Self {
        self.cleanup_hour = hour % 24;
        self
    }

    pub fn with_max_cache_age_days(mut self, days: i64) -> Self {
        self.build_cache_policy.max_unused_days = days;
        self
    }

    pub fn with_build_cache_policy(mut self, policy: BuildCachePolicy) -> Self {
        self.build_cache_policy = policy;
        self
    }

    pub fn with_max_asset_cache_age_days(mut self, days: i64) -> Self {
        self.max_asset_cache_age_days = days;
        self
    }

    pub fn with_keep_recent_deployment_images(mut self, count: u64) -> Self {
        self.keep_recent_deployment_images = count;
        self
    }

    pub fn with_max_deployment_images_per_run(mut self, count: u64) -> Self {
        self.max_deployment_images_per_run = count;
        self
    }

    /// Apply the operator-configured retention policy from `AppSettings`.
    /// Used as the boot-time value and, until the first live read completes,
    /// as the `last_known_image_retention` fallback.
    pub fn with_image_retention(mut self, settings: &temps_config::ImageRetentionSettings) -> Self {
        self.image_retention_enabled = settings.enabled;
        self.default_image_retention_hours = settings.effective_default_hours();
        self.last_known_image_retention = std::sync::Mutex::new((
            self.image_retention_enabled,
            self.default_image_retention_hours,
        ));
        self
    }

    /// Re-read `image_retention` from this service at the start of every
    /// nightly run instead of only once at construction, so an operator
    /// disabling retention takes effect on the very next run rather than
    /// requiring a restart.
    pub fn with_config_service(mut self, config_service: Arc<temps_config::ConfigService>) -> Self {
        self.config_service = Some(config_service);
        self
    }

    /// The cached `(enabled, default_hours)` pair. It is a `Copy` value
    /// replaced whole, so a lock poisoned by an unrelated panic still holds a
    /// consistent value and is recovered rather than taking down every later
    /// cleanup run.
    fn lock_last_known_image_retention(&self) -> std::sync::MutexGuard<'_, (bool, i64)> {
        self.last_known_image_retention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn last_known_image_retention(&self) -> (bool, i64) {
        *self.lock_last_known_image_retention()
    }

    /// Current retention policy: the fresh settings-row value when a
    /// `ConfigService` is wired up, falling back to `last_known_image_retention`
    /// otherwise (all existing tests, before the first live read) or on a
    /// read error. Deliberately the *last successful* read rather than the
    /// boot-time snapshot: an operator who disabled retention and then hit a
    /// transient settings-read error must not have deletions silently resume
    /// under a stale "enabled" policy from startup.
    async fn current_image_retention(&self) -> (bool, i64) {
        let Some(config_service) = &self.config_service else {
            return self.last_known_image_retention();
        };
        match config_service.get_settings().await {
            Ok(settings) => {
                let fresh = (
                    settings.image_retention.enabled,
                    settings.image_retention.effective_default_hours(),
                );
                *self.lock_last_known_image_retention() = fresh;
                fresh
            }
            Err(e) => {
                let last_known = self.last_known_image_retention();
                error!(
                    error = %e,
                    "Could not read image retention settings; using last-known values"
                );
                last_known
            }
        }
    }

    /// Build cache policy for this run: the operator's current
    /// `build_limits` when a `ConfigService` is wired up, otherwise (or on a
    /// read error) the constructor/builder value. Unlike image retention, a
    /// stale value here is harmless: the cache is rebuildable, so the worst
    /// case is one night pruned to the default cap.
    async fn current_build_cache_policy(&self) -> BuildCachePolicy {
        let Some(config_service) = &self.config_service else {
            return self.build_cache_policy;
        };
        match config_service.get_settings().await {
            Ok(settings) => BuildCachePolicy::from(&settings.build_limits),
            Err(e) => {
                warn!(
                    error = %e,
                    "Could not read build cache limits; using {} day(s) / {:?} bytes",
                    self.build_cache_policy.max_unused_days,
                    self.build_cache_policy.max_size_bytes
                );
                self.build_cache_policy
            }
        }
    }

    /// Local image IDs retention must keep whatever their age: the rollback
    /// floor, images that cannot be obtained again, what each environment
    /// serves, and built images worker nodes roll back from. `None` when any
    /// of them cannot be read: the pass is then skipped rather than run
    /// without that protection.
    async fn protected_image_ids(&self, index: &LocalImageIndex) -> Option<HashSet<String>> {
        let mut protected = HashSet::new();

        // Preserve main's rollback floor from #645: regardless of the age
        // policy, keep the newest N deployment images for every
        // project+environment. Computed via a window function so the result
        // is bounded by (project, environment) pairs × N, not by total
        // deployment history.
        match self
            .recently_protected_image_names(self.keep_recent_deployment_images)
            .await
        {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut protected, index, name);
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine recently-deployed images; skipping image \
                     retention this run to avoid removing a rollback target"
                );
                return None;
            }
        }

        // Protect images that can be neither rebuilt nor pulled again.
        match self.unrecoverable_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut protected, index, name);
                }
                debug!(
                    protected = names.len(),
                    "Protected unrecoverable deployment images from retention"
                );
            }
            Err(e) => {
                // Failing open here would delete uploaded images. Abort the
                // whole pass instead and say so — a skipped night costs disk,
                // a wrong deletion costs the user their deployment.
                error!(
                    error = %e,
                    "Could not determine which deployment images are recoverable; \
                     skipping image retention this run to avoid deleting an \
                     unrecoverable image"
                );
                return None;
            }
        }

        // Protect whatever each environment is currently serving. Bounded by
        // environment count via a direct join rather than a scan of
        // deployment history.
        match self.actively_served_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut protected, index, name);
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine active deployments; skipping image \
                     retention this run to avoid removing a live image"
                );
                return None;
            }
        }

        // Protect built images whose containers live on a worker node: the
        // control plane's copy is what a rollback there streams from. A
        // registry image is exempt, since the worker pulls it itself.
        match self.remote_node_image_names().await {
            Ok(names) => {
                for name in &names {
                    Self::protect_image(&mut protected, index, name);
                }
                if !names.is_empty() {
                    debug!(
                        remote = names.len(),
                        "Skipping deployment images owned by worker nodes"
                    );
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Could not determine which deployment images are node-local; \
                     skipping image retention this run"
                );
                return None;
            }
        }

        Some(protected)
    }

    /// Mark an image as permanently protected, whatever its age.
    ///
    /// Used for images we cannot recover (uploaded tarballs, claimed local
    /// images, non-git builds), the image each environment is currently
    /// serving, the rollback floor and worker-node rollback sources. No name
    /// filter: which images are candidates at all is decided by
    /// `expired_image_candidates` from the builder's own records, so a
    /// protection for any other image is simply never consulted.
    ///
    /// `protected` holds local image IDs, and `image_name` (a reference in
    /// any spelling, or an image ID) is resolved through `index`: protecting
    /// `nginx:1` protects the image a candidate recorded as
    /// `docker.io/library/nginx:1`, or as its digest. A name not on this
    /// daemon protects nothing, because there is nothing to remove.
    fn protect_image(protected: &mut HashSet<String>, index: &LocalImageIndex, image_name: &str) {
        if let Some(image_id) = index.image_id_for(image_name) {
            protected.insert(image_id.to_string());
        }
    }

    /// Calculate seconds until the next scheduled cleanup
    fn seconds_until_next_cleanup(&self) -> u64 {
        seconds_until_next_cleanup(self.cleanup_hour)
    }

    /// Start the cleanup scheduler (blocking, should be spawned in tokio task)
    pub async fn start_cleanup_scheduler(&self) {
        info!(
            "Docker cleanup scheduler started (cleanup hour: {}:00 UTC)",
            self.cleanup_hour
        );

        loop {
            let seconds_until_cleanup = self.seconds_until_next_cleanup();
            let hours = seconds_until_cleanup / 3600;
            let minutes = (seconds_until_cleanup % 3600) / 60;

            debug!(
                "Next Docker cleanup scheduled in {} hours {} minutes",
                hours, minutes
            );

            sleep(Duration::from_secs(seconds_until_cleanup)).await;

            // Run cleanup
            self.perform_cleanup().await;

            // Sleep for 1 minute to avoid running cleanup multiple times in the same minute
            sleep(Duration::from_secs(60)).await;
        }
    }

    /// Remove deployment images that are older than their project's retention period.
    ///
    /// Two kinds of image are candidates: tags the Temps image builder
    /// recorded producing, and images pulled from a registry for an image
    /// deployment (`temps deploy:image`). Both can be obtained again — the
    /// first by redeploying from source, the second by pulling the same
    /// reference, which a rollback onto it does automatically.
    ///
    /// An image is eligible only when **every** deployment row that references it is
    /// older than its owning project's retention period, so an image reused by a newer
    /// rollback or promotion survives. On top of that expiry rule these are
    /// protected unconditionally, whatever their age:
    ///
    /// 1. **Images we cannot recover** — uploaded tarballs (`POST .../deployments/upload`),
    ///    claimed local images, images recorded only by an external-image ID with no
    ///    registry reference, and builds of non-git projects. There is no source to get
    ///    them from again, so removing one is data loss, not disk reclamation.
    /// 2. **The image each environment is currently serving**
    ///    (`environments.current_deployment_id`), so a live service can never lose the
    ///    image it is running even if it has not been redeployed in months.
    /// 3. **Images a container on this host still uses**, according to
    ///    `deployment_containers` (and, as the final safety net, Docker itself: removal
    ///    is non-forced, so an image any container references is refused).
    /// 4. **Built images belonging to deployments on other nodes** — the control plane's
    ///    copy of a built image is what a worker rollback streams from. A registry image
    ///    is not protected this way: a worker pulls it from the registry itself.
    ///
    /// The newest `keep_recent_deployment_images` deployment rows in each
    /// project+environment are retained as a rollback floor, and each pass removes at
    /// most `max_deployment_images_per_run` candidates, oldest first. Only images that
    /// still exist on the local daemon are considered.
    async fn prune_old_deployment_images(&self) {
        let (image_retention_enabled, default_image_retention_hours) =
            self.current_image_retention().await;
        if !image_retention_enabled {
            debug!("Deployment image retention is disabled; skipping");
            return;
        }

        let local_images = match self.docker_client.list_images().await {
            Ok(images) => images,
            Err(e) => {
                error!(
                    error = %e,
                    "Could not list local Docker images; skipping image retention this run"
                );
                return;
            }
        };
        // Every tag and digest reference of every image, so a deployment
        // that recorded any of them (in any Docker Hub spelling) is matched to
        // the one image they all name.
        let index = LocalImageIndex::new(local_images);
        if index.is_empty() {
            debug!("No named local Docker images; skipping image retention");
            return;
        }

        // Every protection is read before the candidate query and handed to
        // it, so a protected image is dropped before its `LIMIT` and never
        // takes one of the run's slots. Otherwise a project whose only
        // expired deployment is also its rollback floor (or what it serves)
        // could hold the oldest slots every night, and no other project's
        // expired images would ever be reached.
        let Some(protected) = self.protected_image_ids(&index).await else {
            return;
        };

        // Eligibility (and the oldest reference per image, used to order
        // removal) is computed inside Postgres via GROUP BY/HAVING rather
        // than by pulling every deployment row into the process, and capped
        // at `max_deployment_images_per_run` — see the doc comment on
        // `expired_image_candidates` for why this table's row count doesn't
        // shrink to "how many images exist".
        let oldest_and_names_by_image = match self
            .expired_image_candidates(default_image_retention_hours, &index, &protected)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(
                    error = %e,
                    "Could not compute deployment image retention eligibility; \
                     skipping image retention this run"
                );
                return;
            }
        };

        if oldest_and_names_by_image.is_empty() {
            debug!("No expired deployment images to consider");
            return;
        }

        // The query already excluded `protected`; checking again here keeps
        // a protected image safe even if the two ever disagree.
        let mut expired: Vec<(String, chrono::DateTime<chrono::Utc>)> = oldest_and_names_by_image
            .iter()
            .filter(|(image_id, _)| !protected.contains(*image_id))
            .map(|(image_id, (oldest, _))| (image_id.clone(), *oldest))
            .collect();

        // Preserve main's bounded nightly work from #645. Oldest candidates
        // go first; the remainder is picked up on later runs.
        expired.sort_by(|(a_id, a), (b_id, b)| a.cmp(b).then_with(|| a_id.cmp(b_id)));
        expired.truncate(self.max_deployment_images_per_run.min(usize::MAX as u64) as usize);

        let mut image_sizes: HashMap<String, u64> = HashMap::new();
        let mut removals: Vec<ImageRemoval> = Vec::new();
        for (image_id, _) in &expired {
            let named_keys = oldest_and_names_by_image
                .get(image_id)
                .map(|(_, names)| names.as_slice())
                .unwrap_or_default();
            let names = index.removal_names(image_id, named_keys);
            if names.is_empty() {
                debug!(
                    image_id = %image_id,
                    "Expired deployment image carries no name a deployment recorded; keeping it"
                );
                continue;
            }
            for name in names {
                image_sizes.insert(name.clone(), index.size_of(image_id));
                removals.push(ImageRemoval {
                    reference: name,
                    image_id: image_id.clone(),
                });
            }
        }
        let expired = removals;

        if expired.is_empty() {
            debug!("No expired deployment images to remove");
            return;
        }

        // Every outcome is logged individually and the totals reported
        // explicitly: reporting only successes made a run where every removal
        // was refused look identical to a run with nothing to do.
        let outcomes = self.docker_client.remove_images(&expired).await;
        summarize_removals("deployment_image_retention", &outcomes, &image_sizes);
    }

    /// Remove images this install built for projects that no longer exist
    /// (see `orphaned_images` for the safety rules). Skipped, like the
    /// retention pass, when the operator turned image retention off: built
    /// images are then kept forever.
    async fn remove_orphaned_project_images(&self) {
        let (image_retention_enabled, _) = self.current_image_retention().await;
        if !image_retention_enabled {
            debug!(
                "Deployment image retention is disabled; not removing images of deleted projects"
            );
            return;
        }
        self.orphan_sweeper.run().await;
    }

    /// Nightly sweep of images of deleted projects only, for a process that
    /// runs no workloads itself (`--profile control-plane`): it has no local
    /// Docker to clean, but its worker nodes do.
    pub async fn start_orphan_sweep_scheduler(&self) {
        info!(
            "Image sweep scheduler started for worker nodes (hour: {}:00 UTC)",
            self.cleanup_hour
        );
        loop {
            sleep(Duration::from_secs(self.seconds_until_next_cleanup())).await;
            self.remove_orphaned_project_images().await;
            sleep(Duration::from_secs(60)).await;
        }
    }

    /// Per-image eligibility and oldest reference, computed inside Postgres.
    ///
    /// **What is a candidate.** Only an image Temps itself produced or
    /// fetched for a deployment, by recorded provenance — eligibility is
    /// never inferred from the tag text:
    ///
    /// - a tag the Temps image builder recorded producing: the `image_tag`
    ///   output a successful `BuildImageJob` persists to
    ///   `deployment_jobs.outputs`. The builder tags `<deployment slug>:latest`,
    ///   so an earlier `image_name LIKE 'temps-%'` filter matched only projects
    ///   whose slug happened to start with `temps-` and left every other
    ///   project's images on disk forever. Provenance from the build job also
    ///   covers a build that succeeded ahead of a deploy that failed: the
    ///   image is on disk, but `deployments.image_name` is only written on
    ///   completion and stays NULL;
    /// - a registry reference an image deployment pulled
    ///   (`metadata.externalImageRef`, [`sql_registry_pull`]). Unlike an
    ///   upload it can be pulled again from that registry, and a rollback
    ///   onto it does so. A deployment that only names an external-image ID
    ///   with no registry reference, or any kind of upload, never makes its
    ///   image a candidate.
    ///
    /// Every name is compared by [`canonical_image_reference`] (in SQL,
    /// [`sql_canonical_image_reference`]): Docker resolves `nginx:1` and
    /// `docker.io/library/nginx:1` to the same local image, so an upload
    /// recorded under one spelling must protect a registry pull recorded under
    /// the other. Each result row is one image: its canonical key and one
    /// recorded name to remove it by.
    ///
    /// Candidates are further restricted to `local_image_tags`, the tags that
    /// still exist on this daemon, so an image removed on an earlier night
    /// (whose deployment rows still name it) does not take up a slot in the
    /// `LIMIT` window again.
    ///
    /// **When it is eligible.** An image's references are every deployment
    /// that built it, recorded it in `image_name` (rollbacks and promotions
    /// reuse the tag), or recorded it as its external image reference
    /// (uploads included). The image is eligible only when **every**
    /// reference is older than its owning project's retention window
    /// (`BOOL_AND`, with the per-project override applied via `COALESCE`),
    /// so an image reused by a newer rollback survives, and only when **no**
    /// reference is
    ///
    /// - still in flight (a deployment that built its image and is still
    ///   rolling out has no `image_name` yet, and nothing else protects it),
    /// - unrecoverable ([`sql_unrecoverable_deployment`], the same predicate
    ///   as `unrecoverable_image_names`) — so a tag that was ever an upload
    ///   stays protected even if a registry deployment later used the name,
    /// - running a container on this host that has not been removed
    ///   (`deployment_containers` with no `deleted_at`),
    /// - or, for a built image, running containers on a worker node
    ///   (`remote_node_image_names`). A registry-only image is exempt: the
    ///   worker pulls it from the registry, never from this copy.
    ///
    /// Each protective flag is `COALESCE`d to `TRUE`, so a NULL column
    /// protects the image rather than releasing it.
    ///
    /// **Nothing protected takes a slot.** Every protection is applied here,
    /// before `LIMIT`, not after it. An install with
    /// `max_deployment_images_per_run` or more old, protected images would
    /// otherwise fill the whole `ORDER BY MIN(created_at) ASC LIMIT` window
    /// with rows the pass then keeps, and every run would reclaim nothing
    /// instead of picking up the remainder on the next run. The protections
    /// above are evaluated in the query; the rest (the keep-recent-N rollback
    /// floor, what each environment serves, worker-node rollback sources)
    /// are read first and passed in as `protected`, local image IDs. Being a
    /// project's newest deployment does not make an image newer than another
    /// project's expired ones, so those compete for the oldest-first slots
    /// too. An image no recorded name would remove (`removable_images`,
    /// mirroring [`LocalImageIndex::removal_names`]), such as a pulled image
    /// that now carries only an operator's own tag, is dropped the same way.
    ///
    /// `HAVING` drops non-expired groups before they leave Postgres, and
    /// `LIMIT` gives the candidate scan the same bound as the removal batch.
    async fn expired_image_candidates(
        &self,
        default_hours: i64,
        index: &LocalImageIndex,
        protected: &HashSet<String>,
    ) -> Result<HashMap<String, (chrono::DateTime<chrono::Utc>, Vec<String>)>, sea_orm::DbErr> {
        use sea_orm::{ActiveEnum, ConnectionTrait, Statement};

        let limit = i64::try_from(self.max_deployment_images_per_run).unwrap_or(i64::MAX);
        let build_succeeded = temps_entities::types::JobStatus::Success.to_value();
        let local_references = index.query_input();
        let protected_ids = serde_json::Value::Array(
            protected
                .iter()
                .map(|id| serde_json::Value::String(id.clone()))
                .collect(),
        );
        // Only fixed predicates are interpolated; every value is a bind
        // parameter.
        let sql = format!(
            r#"
                WITH local_refs AS (
                    SELECT DISTINCT
                        e ->> 0 AS image_id,
                        {key_e} AS image_key,
                        e ->> 2 AS removable_as
                    FROM jsonb_array_elements($4::jsonb) AS e
                ),
                local_ids AS (
                    SELECT DISTINCT image_id FROM local_refs
                ),
                built AS (
                    SELECT DISTINCT
                        dj.deployment_id,
                        dj.outputs ->> 'image_tag' AS image_name
                    FROM deployment_jobs dj
                    WHERE dj.job_type = 'BuildImageJob'
                      AND dj.status = $3
                      AND dj.outputs ->> 'image_tag' IS NOT NULL
                ),
                pulled_ids AS (
                    SELECT DISTINCT dj.deployment_id, {pulled_id} AS image_id
                    FROM deployment_jobs dj
                    WHERE dj.job_type = 'PullExternalImageJob'
                      AND dj.status = $3
                      AND NULLIF(dj.outputs ->> 'image_id', '') IS NOT NULL
                ),
                candidate_ids AS (
                    SELECT lr.image_id
                    FROM built b
                    JOIN local_refs lr ON lr.image_key = {key_b}
                    WHERE b.image_name NOT LIKE '%/%'
                    UNION
                    SELECT lr.image_id
                    FROM deployments d
                    JOIN local_refs lr ON lr.image_key = {key_d_ref}
                    WHERE {registry_pull}
                    UNION
                    SELECT pi.image_id
                    FROM pulled_ids pi
                    JOIN local_ids li ON li.image_id = pi.image_id
                    JOIN deployments d ON d.id = pi.deployment_id
                    WHERE {registry_pull}
                ),
                image_keys AS (
                    SELECT lr.image_id, lr.image_key
                    FROM local_refs lr
                    JOIN candidate_ids ci ON ci.image_id = lr.image_id
                ),
                refs AS (
                    SELECT ik.image_id, b.deployment_id, ik.image_key AS name_key
                    FROM built b
                    JOIN image_keys ik ON ik.image_key = {key_b}
                    UNION
                    SELECT ik.image_id, d.id, ik.image_key
                    FROM deployments d
                    JOIN image_keys ik ON ik.image_key = {key_d_name}
                    UNION
                    SELECT ik.image_id, d.id, ik.image_key
                    FROM deployments d
                    JOIN image_keys ik ON ik.image_key = {key_d_ref}
                    UNION
                    SELECT pi.image_id, d.id, {key_d_ref}
                    FROM pulled_ids pi
                    JOIN candidate_ids ci ON ci.image_id = pi.image_id
                    JOIN deployments d ON d.id = pi.deployment_id
                    UNION
                    SELECT ci.image_id, d.id, NULL
                    FROM deployments d
                    JOIN candidate_ids ci ON ci.image_id = {uploaded_id}
                ),
                -- Mirrors `LocalImageIndex::removal_names`: an image whose
                -- removable references match no recorded name is kept, so
                -- it must not take a slot in the LIMIT window either.
                removable_images AS (
                    SELECT DISTINCT r.image_id
                    FROM refs r
                    JOIN local_refs lr ON lr.image_id = r.image_id
                    WHERE (lr.removable_as = 'tag' AND lr.image_key = r.name_key)
                       OR (lr.removable_as = 'digest' AND {repo_lr} = {repo_r})
                ),
                built_keys AS (
                    SELECT DISTINCT {key_b} AS image_key FROM built b
                ),
                live_local_keys AS (
                    SELECT DISTINCT {key_dci} AS image_key
                    FROM deployment_containers dci
                    WHERE dci.node_id IS NULL
                      AND dci.deleted_at IS NULL
                ),
                image_flags AS (
                    SELECT
                        ci.image_id,
                        NOT EXISTS (
                            SELECT 1
                            FROM image_keys ik
                            JOIN built_keys bk ON bk.image_key = ik.image_key
                            WHERE ik.image_id = ci.image_id
                        ) AS registry_only,
                        EXISTS (
                            SELECT 1
                            FROM image_keys ik
                            JOIN live_local_keys lk ON lk.image_key = ik.image_key
                            WHERE ik.image_id = ci.image_id
                        ) AS named_by_live_container
                    FROM candidate_ids ci
                ),
                ref_rows AS (
                    SELECT
                        r.image_id,
                        r.name_key,
                        f.registry_only,
                        f.named_by_live_container,
                        d.created_at,
                        d.created_at < NOW() - (
                            COALESCE(p.image_retention_hours, $1) * INTERVAL '1 hour'
                        ) AS expired,
                        COALESCE(
                            d.state IN (
                                'pending', 'creating', 'running',
                                'in_progress', 'built', 'paused'
                            ),
                            TRUE
                        ) AS in_flight,
                        {unrecoverable} AS unrecoverable,
                        EXISTS (
                            SELECT 1
                            FROM deployment_containers dcl
                            WHERE dcl.deployment_id = d.id
                              AND dcl.node_id IS NULL
                              AND dcl.deleted_at IS NULL
                        ) AS has_live_local_container,
                        EXISTS (
                            SELECT 1
                            FROM deployment_containers dcr
                            WHERE dcr.deployment_id = d.id
                              AND dcr.node_id IS NOT NULL
                        ) AS on_remote_node
                    FROM refs r
                    JOIN image_flags f ON f.image_id = r.image_id
                    JOIN deployments d ON d.id = r.deployment_id
                    JOIN projects p ON p.id = d.project_id
                )
                SELECT
                    image_id,
                    MIN(created_at) AS oldest_reference,
                    string_agg(DISTINCT name_key, E'\n') AS named_keys
                FROM ref_rows
                WHERE image_id IN (SELECT image_id FROM removable_images)
                  AND image_id NOT IN (
                      SELECT jsonb_array_elements_text($5::jsonb)
                  )
                GROUP BY image_id
                HAVING BOOL_AND(expired)
                   AND NOT BOOL_OR(in_flight)
                   AND NOT BOOL_OR(unrecoverable)
                   AND NOT BOOL_OR(has_live_local_container)
                   AND (BOOL_AND(registry_only) OR NOT BOOL_OR(on_remote_node))
                   AND NOT BOOL_OR(named_by_live_container)
                ORDER BY MIN(created_at) ASC
                LIMIT $2
                "#,
            registry_pull = sql_registry_pull(),
            unrecoverable = sql_unrecoverable_deployment(),
            key_e = sql_canonical_image_reference("(e ->> 1)"),
            key_b = sql_canonical_image_reference("b.image_name"),
            key_d_name = sql_canonical_image_reference("d.image_name"),
            key_d_ref = sql_canonical_image_reference("d.metadata ->> 'externalImageRef'"),
            key_dci = sql_canonical_image_reference("dci.image_name"),
            pulled_id = sql_normalized_image_id("dj.outputs ->> 'image_id'"),
            uploaded_id = sql_normalized_image_id("d.metadata ->> 'uploadedImageId'"),
            repo_lr = sql_canonical_key_repository("lr.image_key"),
            repo_r = sql_canonical_key_repository("r.name_key"),
        );
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                vec![
                    default_hours.into(),
                    limit.into(),
                    build_succeeded.into(),
                    local_references.into(),
                    protected_ids.into(),
                ],
            ))
            .await?;

        let mut oldest_and_names_by_image = HashMap::new();
        for row in rows {
            let image_id: String = row.try_get("", "image_id")?;
            let oldest_reference: chrono::DateTime<chrono::Utc> =
                row.try_get("", "oldest_reference")?;
            let named_keys: Option<String> = row.try_get("", "named_keys")?;
            let named_keys: Vec<String> = named_keys
                .as_deref()
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            oldest_and_names_by_image.insert(image_id, (oldest_reference, named_keys));
        }

        Ok(oldest_and_names_by_image)
    }

    /// Image names among the newest `keep_recent` deployments for each
    /// project+environment scope, via a window function. Bounded by
    /// (project, environment) pairs × `keep_recent`, not total deployment
    /// history.
    async fn recently_protected_image_names(
        &self,
        keep_recent: u64,
    ) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        // The image ID the pull recorded is listed too: under a mutable tag
        // (`app:latest`) the name now points at a newer image than the one
        // these deployments ran.
        let sql = format!(
            r#"
                SELECT DISTINCT n.image_name
                FROM (
                    SELECT
                        id,
                        image_name,
                        ROW_NUMBER() OVER (
                            PARTITION BY project_id, environment_id
                            ORDER BY created_at DESC
                        ) AS rn
                    FROM deployments
                    WHERE image_name IS NOT NULL
                ) ranked
                CROSS JOIN LATERAL (
                    VALUES (ranked.image_name), ({pulled_id})
                ) AS n(image_name)
                WHERE ranked.rn <= $1
                  AND n.image_name IS NOT NULL
                "#,
            pulled_id = sql_pulled_image_id("ranked.id"),
        );
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                // `as i64` would silently wrap a value above i64::MAX negative,
                // making `rn <= $1` match zero rows and emptying the
                // rollback-floor protection right before an irreversible
                // delete — every other error path in this function fails
                // closed (aborts the run), so this must too.
                vec![i64::try_from(keep_recent).unwrap_or(i64::MAX).into()],
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names for whatever each environment is currently serving.
    /// Bounded by environment count via a direct join, not deployment
    /// history size.
    async fn actively_served_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        // With the image ID its pull recorded, as for the rollback floor.
        let sql = format!(
            r#"
                SELECT DISTINCT n.image_name
                FROM environments e
                JOIN deployments d ON d.id = e.current_deployment_id
                CROSS JOIN LATERAL (
                    VALUES (d.image_name), ({pulled_id})
                ) AS n(image_name)
                WHERE e.current_deployment_id IS NOT NULL
                  AND n.image_name IS NOT NULL
                "#,
            pulled_id = sql_pulled_image_id("d.id"),
        );
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names that must never be pruned because nothing can produce them
    /// again ([`sql_unrecoverable_deployment`]): uploads, claimed local
    /// images, external images known only by ID, and builds of non-git
    /// projects. A registry image deployment is *not* listed: it can be
    /// pulled again.
    ///
    /// Matched on the deployment's own provenance rather than on the tag text,
    /// because the upload endpoint lets the caller supply an arbitrary `tag`.
    /// Both the recorded `image_name` and the external image reference are
    /// listed, since a failed upload deployment never writes `image_name`
    /// while its image is still on disk.
    ///
    /// `source_type <> 'git'` is deliberately broader than strictly necessary:
    /// a `Manual`-source project (`SourceType::Manual`, "accepts any
    /// deployment method") *can* deploy from a rebuildable git-built image,
    /// but the project row doesn't distinguish that case from a genuinely
    /// unrebuildable one, so every non-registry image from a non-`Git`
    /// project is protected. This costs disk on `Manual` projects that happen
    /// to be rebuildable; the alternative — trying to infer rebuildability
    /// from something other than the source_type column — risks a false
    /// negative that permanently deletes an image with no way back.
    /// Over-retention is the safe direction here; under-retention is not.
    async fn unrecoverable_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        // No user input is interpolated — this is a fixed predicate over JSON
        // columns that Sea-ORM's query builder cannot express directly.
        let sql = format!(
            r#"
                SELECT DISTINCT n.image_name
                FROM deployments d
                JOIN projects p ON p.id = d.project_id
                CROSS JOIN LATERAL (
                    VALUES
                        (d.image_name),
                        (NULLIF(d.metadata ->> 'externalImageRef', '')),
                        ({uploaded_id})
                ) AS n(image_name)
                WHERE n.image_name IS NOT NULL
                  AND {unrecoverable}
                "#,
            unrecoverable = sql_unrecoverable_deployment(),
            uploaded_id = sql_normalized_image_id("d.metadata ->> 'uploadedImageId'"),
        );
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Image names whose deployment containers are recorded against a worker
    /// node rather than the control plane, unless the image is purely a
    /// registry pull: the control plane's copy of a built image is what a
    /// rollback on that worker streams from, while a registry image is pulled
    /// by the worker itself. A name any build recorded stays protected, even
    /// if a registry deployment also used it.
    async fn remote_node_image_names(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let sql = format!(
            r#"
                SELECT DISTINCT od.image_name
                FROM deployments od
                JOIN deployment_containers dc ON dc.deployment_id = od.id
                WHERE od.image_name IS NOT NULL
                  AND dc.node_id IS NOT NULL
                  AND (
                      EXISTS (
                          SELECT 1
                          FROM deployment_jobs dj
                          WHERE dj.job_type = 'BuildImageJob'
                            AND dj.outputs ->> 'image_tag' = od.image_name
                      )
                      OR NOT EXISTS (
                          SELECT 1
                          FROM deployments d
                          WHERE d.metadata ->> 'externalImageRef' = od.image_name
                            AND {registry_pull}
                      )
                  )
                "#,
            registry_pull = sql_registry_pull(),
        );
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await?;

        rows.into_iter()
            .map(|row| row.try_get::<String>("", "image_name"))
            .collect()
    }

    /// Perform the actual cleanup
    async fn perform_cleanup(&self) {
        info!("🧹 Starting nightly Docker cleanup");

        let build_cache_policy = self.current_build_cache_policy().await;
        perform_docker_prune(&self.docker_client, build_cache_policy).await;

        // Remove old deployment images per project retention policy
        self.prune_old_deployment_images().await;

        // Remove images built for projects that have since been deleted,
        // which the row-based pass above can no longer see
        self.remove_orphaned_project_images().await;

        // Remove superseded sandbox/gateway images. Deployment images are
        // the retention pass's job here, so only the infrastructure rule runs.
        prune_local_images_without_database(
            &self.docker_client,
            LocalImagePrunePolicy::default(),
            LocalPrunePasses::InfraOnly,
        )
        .await;

        // Cleanup old persisted static asset chunks
        if let Some(ref static_dir) = self.static_dir {
            let chunks_base = static_dir.join("chunks");
            if chunks_base.exists() {
                let (dirs_deleted, bytes_reclaimed) =
                    Self::cleanup_stale_chunks(&chunks_base, self.max_chunk_age_hours).await;
                if dirs_deleted > 0 {
                    info!(
                        "Removed {} stale chunk directories, freed {} MB",
                        dirs_deleted,
                        bytes_reclaimed / (1024 * 1024)
                    );
                } else {
                    debug!("No stale chunk directories to remove");
                }
            }
        }

        // Cleanup stale static asset cache entries and orphaned CAS blobs
        self.cleanup_stale_asset_cache().await;

        info!("Nightly cleanup completed");
    }

    /// Delete static_asset_cache rows older than `max_asset_cache_age_days`
    /// and garbage-collect CAS blobs no longer referenced by any row.
    async fn cleanup_stale_asset_cache(&self) {
        use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};
        use temps_entities::static_asset_cache;

        let cutoff = chrono::Utc::now() - chrono::Duration::days(self.max_asset_cache_age_days);

        // 1. Find hashes that will become orphaned after deletion
        let stale_rows = match static_asset_cache::Entity::find()
            .filter(static_asset_cache::Column::CreatedAt.lt(cutoff))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                error!("Failed to query stale static asset cache rows: {}", e);
                return;
            }
        };

        if stale_rows.is_empty() {
            debug!("No stale static asset cache entries to clean up");
            return;
        }

        let stale_hashes: HashSet<String> =
            stale_rows.iter().map(|r| r.content_hash.clone()).collect();
        let stale_count = stale_rows.len();

        // 2. Delete stale rows (parameterized query to prevent SQL injection)
        let delete_result = self
            .db
            .as_ref()
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "DELETE FROM static_asset_cache WHERE created_at < $1",
                [cutoff.into()],
            ))
            .await;

        match delete_result {
            Ok(result) => {
                info!(
                    "🧹 Deleted {} stale static asset cache entries (older than {} days)",
                    result.rows_affected(),
                    self.max_asset_cache_age_days
                );
            }
            Err(e) => {
                error!("Failed to delete stale static asset cache entries: {}", e);
                return;
            }
        }

        // 3. Garbage-collect orphaned blobs (hashes no longer referenced)
        let mut blobs_deleted = 0u64;
        for hash in &stale_hashes {
            // Check if any remaining row still references this hash
            let still_referenced = static_asset_cache::Entity::find()
                .filter(static_asset_cache::Column::ContentHash.eq(hash.as_str()))
                .count(self.db.as_ref())
                .await
                .unwrap_or(1); // If query fails, assume referenced (safe)

            if still_referenced == 0 {
                match self.file_store.delete_blob(hash).await {
                    Ok(true) => {
                        blobs_deleted += 1;
                    }
                    Ok(false) => {} // Already gone
                    Err(e) => {
                        let hash_prefix: String = hash.chars().take(8).collect();
                        warn!("Failed to delete orphaned blob {}: {}", hash_prefix, e);
                    }
                }
            }
        }

        if blobs_deleted > 0 {
            info!(
                "🧹 Garbage-collected {} orphaned CAS blobs (from {} stale entries)",
                blobs_deleted, stale_count
            );
        }
    }

    /// Remove persisted chunk directories older than `max_age_hours`.
    async fn cleanup_stale_chunks(chunks_base: &std::path::Path, max_age_hours: u64) -> (u64, u64) {
        let max_age = Duration::from_secs(max_age_hours * 3600);
        let mut dirs_deleted = 0u64;
        let mut bytes_reclaimed = 0u64;

        // Walk: chunks/{project_id}/{environment_id}/{deployment_id}/
        let project_dirs = match std::fs::read_dir(chunks_base) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read chunks directory: {}", e);
                return (0, 0);
            }
        };

        for project_entry in project_dirs.flatten() {
            if !project_entry.path().is_dir() {
                continue;
            }

            let env_dirs = match std::fs::read_dir(project_entry.path()) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for env_entry in env_dirs.flatten() {
                if !env_entry.path().is_dir() {
                    continue;
                }

                let deploy_dirs = match std::fs::read_dir(env_entry.path()) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };

                for deploy_entry in deploy_dirs.flatten() {
                    let deploy_path = deploy_entry.path();
                    if !deploy_path.is_dir() {
                        continue;
                    }

                    let age = deploy_entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok());

                    if let Some(age) = age {
                        if age > max_age {
                            let size = Self::dir_size_sync(&deploy_path);
                            match std::fs::remove_dir_all(&deploy_path) {
                                Ok(()) => {
                                    dirs_deleted += 1;
                                    bytes_reclaimed += size;
                                    debug!(
                                        "Removed stale chunk dir: {} (age: {}h)",
                                        deploy_path.display(),
                                        age.as_secs() / 3600,
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to remove chunk dir {}: {}",
                                        deploy_path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                }

                // Remove empty environment directory
                if std::fs::read_dir(env_entry.path())
                    .map(|mut e| e.next().is_none())
                    .unwrap_or(false)
                {
                    let _ = std::fs::remove_dir(env_entry.path());
                }
            }

            // Remove empty project directory
            if std::fs::read_dir(project_entry.path())
                .map(|mut e| e.next().is_none())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_dir(project_entry.path());
            }
        }

        (dirs_deleted, bytes_reclaimed)
    }

    fn dir_size_sync(path: &std::path::Path) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    total += Self::dir_size_sync(&p);
                } else if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
        total
    }
}

#[cfg(test)]
mod tests;
