// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::services::local_image_pruning::tests::{
    app_images, app_tag, gateway_image, local_image, serving,
};

#[derive(Clone)]
struct MockDockerClient {
    prune_images_result: Result<PruneStats, String>,
    prune_cache_result: Result<String, String>,
}

#[async_trait::async_trait]
impl DockerClient for MockDockerClient {
    async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
        self.prune_images_result.clone()
    }

    async fn prune_builder_cache(&self, _policy: BuildCachePolicy) -> Result<String, String> {
        self.prune_cache_result.clone()
    }

    async fn remove_images(&self, removals: &[ImageRemoval]) -> Vec<ImageRemovalOutcome> {
        removals
            .iter()
            .map(|removal| ImageRemovalOutcome {
                image_name: removal.reference.clone(),
                status: ImageDeleteStatus::Deleted,
            })
            .collect()
    }

    async fn list_images(&self) -> Result<Vec<LocalImage>, DockerInventoryError> {
        Ok(Vec::new())
    }

    async fn list_containers(&self) -> Result<Vec<LocalContainer>, DockerInventoryError> {
        Ok(Vec::new())
    }
}

/// Docker client that records every image it was asked to remove, so tests
/// can assert on *which* images the policy selected rather than only that
/// the call did not panic.
#[derive(Default)]
struct RecordingDockerClient {
    removed: std::sync::Mutex<Vec<String>>,
    /// Image names the fake daemon refuses to remove (simulating "image is
    /// being used by container").
    refuse: Vec<String>,
    /// Tags the fake daemon reports as present locally (one image each).
    local_images: Vec<String>,
    /// Full image inventory, used instead of `local_images` when set.
    images: Vec<LocalImage>,
    /// Containers the fake daemon reports.
    containers: Vec<LocalContainer>,
}

impl RecordingDockerClient {
    fn with_local_images(tags: &[&str]) -> Self {
        Self {
            local_images: tags.iter().map(|tag| tag.to_string()).collect(),
            ..Default::default()
        }
    }

    fn with_inventory(images: Vec<LocalImage>, containers: Vec<LocalContainer>) -> Self {
        Self {
            images,
            containers,
            ..Default::default()
        }
    }

    fn removed_sorted(&self) -> Vec<String> {
        let mut v = self
            .removed
            .lock()
            .expect("recording mock mutex poisoned")
            .clone();
        v.sort();
        v
    }
}

#[async_trait::async_trait]
impl DockerClient for RecordingDockerClient {
    async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
        Ok(PruneStats {
            images_deleted: 0,
            space_reclaimed_mb: 0,
        })
    }

    async fn prune_builder_cache(&self, _policy: BuildCachePolicy) -> Result<String, String> {
        Ok(String::new())
    }

    async fn remove_images(&self, removals: &[ImageRemoval]) -> Vec<ImageRemovalOutcome> {
        let image_names: Vec<String> = removals
            .iter()
            .map(|removal| removal.reference.clone())
            .collect();
        self.removed
            .lock()
            .expect("recording mock mutex poisoned")
            .extend(image_names.iter().cloned());
        image_names
            .iter()
            .map(|name| ImageRemovalOutcome {
                image_name: name.clone(),
                status: if self.refuse.contains(name) {
                    ImageDeleteStatus::Retained {
                        reason: format!("conflict: image {} is being used", name),
                    }
                } else {
                    ImageDeleteStatus::Deleted
                },
            })
            .collect()
    }

    async fn list_images(&self) -> Result<Vec<LocalImage>, DockerInventoryError> {
        if !self.images.is_empty() {
            return Ok(self.images.clone());
        }
        Ok(self
            .local_images
            .iter()
            .enumerate()
            .map(|(n, tag)| LocalImage {
                id: format!("sha256:{n:064x}"),
                repo_tags: vec![tag.clone()],
                repo_digests: Vec::new(),
                created: 0,
                size_bytes: 1024,
            })
            .collect())
    }

    async fn list_containers(&self) -> Result<Vec<LocalContainer>, DockerInventoryError> {
        Ok(self.containers.clone())
    }
}

/// Docker client whose image listing fails, to prove the pass fails
/// closed rather than guessing which images exist.
struct UnlistableDockerClient {
    removed: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl DockerClient for UnlistableDockerClient {
    async fn prune_images(&self, _force: bool) -> Result<PruneStats, String> {
        Ok(PruneStats {
            images_deleted: 0,
            space_reclaimed_mb: 0,
        })
    }

    async fn prune_builder_cache(&self, _policy: BuildCachePolicy) -> Result<String, String> {
        Ok(String::new())
    }

    async fn remove_images(&self, removals: &[ImageRemoval]) -> Vec<ImageRemovalOutcome> {
        self.removed
            .lock()
            .expect("unlistable mock mutex poisoned")
            .extend(removals.iter().map(|removal| removal.reference.clone()));
        Vec::new()
    }

    async fn list_images(&self) -> Result<Vec<LocalImage>, DockerInventoryError> {
        Err(DockerInventoryError::ListImages {
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "daemon unreachable".to_string(),
            },
        })
    }

    async fn list_containers(&self) -> Result<Vec<LocalContainer>, DockerInventoryError> {
        Err(DockerInventoryError::ListContainers {
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "daemon unreachable".to_string(),
            },
        })
    }
}

fn mock_db() -> Arc<sea_orm::DatabaseConnection> {
    Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
}

fn mock_file_store() -> Arc<dyn temps_file_store::FileStore> {
    Arc::new(temps_file_store::fs_store::FsFileStore::new(
        std::path::PathBuf::from("/tmp/temps-test-cleanup"),
    ))
}

#[test]
fn test_cleanup_hour_calculation() {
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store());
    let seconds = service.seconds_until_next_cleanup();

    // Should be positive and less than 24 hours
    assert!(seconds > 0);
    assert!(seconds <= 24 * 3600);
}

#[test]
fn test_seconds_until_next_cleanup_invalid_hour_retries_instead_of_panicking() {
    // 25 is out of chrono's 0..24 range for `with_hour`; every real
    // caller guards with `% 24`, but this must degrade to a 24h retry
    // rather than panic if that guard is ever skipped.
    assert_eq!(seconds_until_next_cleanup(25), 24 * 3600);
}

#[test]
fn test_custom_cleanup_hour() {
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
            .with_cleanup_hour(3);

    assert_eq!(service.cleanup_hour, 3);
}

#[test]
fn test_custom_cache_age() {
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
            .with_max_cache_age_days(14);

    assert_eq!(service.build_cache_policy.max_unused_days, 14);
}

fn cache_record(
    size_bytes: u64,
    in_use: bool,
    last_activity_secs_ago: Option<i64>,
    now: chrono::DateTime<chrono::Utc>,
) -> crate::services::BuildCacheRecord {
    crate::services::BuildCacheRecord {
        size_bytes,
        in_use,
        last_used_at: last_activity_secs_ago.map(|s| now - chrono::Duration::seconds(s)),
        created_at: None,
    }
}

#[test]
fn build_cache_policy_defaults_to_seven_days_and_ten_gigabytes() {
    let policy = BuildCachePolicy::default();
    assert_eq!(policy.max_unused_days, 7);
    assert_eq!(policy.max_size_bytes, Some(10 * 1024 * 1024 * 1024));
    let uncapped = BuildCachePolicy::from(&temps_config::BuildLimitsSettings {
        build_cache_max_gb: 0,
        build_cache_max_unused_days: 3,
        ..Default::default()
    });
    assert_eq!(uncapped.max_size_bytes, None);
    assert_eq!(uncapped.max_unused_days, 3);
}

#[test]
fn eviction_not_needed_when_the_cache_fits() {
    let now = chrono::Utc::now();
    let records = vec![cache_record(40, false, Some(100), now)];
    assert_eq!(plan_build_cache_eviction(&records, 40, now), None);
    assert_eq!(plan_build_cache_eviction(&[], 0, now), None);
}

#[test]
fn eviction_removes_least_recently_used_until_under_the_cap() {
    let now = chrono::Utc::now();
    // 100 bytes total, cap 50: evicting the two oldest (30 + 25) suffices.
    let records = vec![
        cache_record(25, false, Some(5_000), now),
        cache_record(30, false, Some(9_000), now),
        cache_record(20, false, Some(1_000), now),
        cache_record(25, false, Some(100), now),
    ];
    let plan = plan_build_cache_eviction(&records, 50, now).unwrap();
    assert_eq!(plan.total_bytes, 100);
    assert_eq!(plan.evicted_bytes, 55);
    // Must cover the 5_000s-old record but not the 1_000s-old one.
    assert!(plan.keep_secs < 5_000, "{plan:?}");
    assert!(plan.keep_secs >= 1_000, "{plan:?}");
}

#[test]
fn eviction_never_counts_in_use_or_undated_records() {
    let now = chrono::Utc::now();
    let records = vec![
        cache_record(80, true, Some(90_000), now),
        cache_record(10, false, None, now),
        cache_record(15, false, Some(60), now),
    ];
    let plan = plan_build_cache_eviction(&records, 50, now).unwrap();
    assert_eq!(plan.evicted_bytes, 15, "only the dated, unused record");

    let only_in_use = vec![cache_record(80, true, Some(90_000), now)];
    assert_eq!(plan_build_cache_eviction(&only_in_use, 50, now), None);
}

#[test]
fn eviction_takes_records_tied_with_the_cutoff_since_until_removes_them_too() {
    let now = chrono::Utc::now();
    let records = vec![
        cache_record(30, false, Some(500), now),
        cache_record(30, false, Some(500), now),
        cache_record(30, false, Some(10), now),
    ];
    let plan = plan_build_cache_eviction(&records, 70, now).unwrap();
    assert_eq!(plan.evicted_bytes, 60);
}

#[test]
fn test_docker_only_scheduler_defaults() {
    let scheduler = DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient));

    assert_eq!(scheduler.cleanup_hour, 2);
    assert_eq!(scheduler.build_cache_policy.max_unused_days, 7);
}

#[test]
fn test_docker_only_scheduler_custom_cleanup_hour() {
    let scheduler =
        DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient)).with_cleanup_hour(3);

    assert_eq!(scheduler.cleanup_hour, 3);
}

#[test]
fn test_docker_only_scheduler_custom_cache_age() {
    let scheduler =
        DockerOnlyCleanupScheduler::new(Arc::new(DefaultDockerClient)).with_max_cache_age_days(14);

    assert_eq!(scheduler.build_cache_policy.max_unused_days, 14);
}

#[tokio::test]
async fn test_perform_docker_prune_reports_success() {
    let client: Arc<dyn DockerClient> = Arc::new(MockDockerClient {
        prune_images_result: Ok(PruneStats {
            images_deleted: 2,
            space_reclaimed_mb: 128,
        }),
        prune_cache_result: Ok("removed 1 build cache entries, freed 64 MB".to_string()),
    });

    // Exercises the shared free function directly — success is "did
    // not panic and produced no error path" since prune_images/
    // prune_builder_cache results are only logged, not returned.
    perform_docker_prune(&client, BuildCachePolicy::default()).await;
}

#[tokio::test]
async fn test_perform_docker_prune_handles_errors_gracefully() {
    let client: Arc<dyn DockerClient> = Arc::new(MockDockerClient {
        prune_images_result: Err("daemon unreachable".to_string()),
        prune_cache_result: Err("builder prune not supported".to_string()),
    });

    // Both prune calls fail; the shared helper must not panic — errors
    // are logged and cleanup continues (e.g. images prune failing must
    // not skip the build-cache prune).
    perform_docker_prune(&client, BuildCachePolicy::default()).await;
}

#[test]
fn test_default_image_retention_hours_is_a_rollback_window() {
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store());
    // 14 days. A short default (e.g. 48h) silently destroys the rollback
    // history of any project that does not deploy over a long weekend.
    assert_eq!(service.default_image_retention_hours, 336);
    assert!(service.image_retention_enabled);
    assert_eq!(service.keep_recent_deployment_images, 5);
    assert_eq!(service.max_deployment_images_per_run, 500);
    assert_eq!(
        service.default_image_retention_hours,
        temps_config::ImageRetentionSettings::default().default_hours,
        "service default must track the settings default"
    );
}

#[test]
fn test_cleanup_limits_are_configurable() {
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
            .with_keep_recent_deployment_images(10)
            .with_max_deployment_images_per_run(50);

    assert_eq!(service.keep_recent_deployment_images, 10);
    assert_eq!(service.max_deployment_images_per_run, 50);
}

#[test]
fn test_operator_settings_override_retention() {
    let settings = temps_config::ImageRetentionSettings {
        enabled: false,
        default_hours: 72,
    };
    let service =
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
            .with_image_retention(&settings);

    assert_eq!(service.default_image_retention_hours, 72);
    assert!(!service.image_retention_enabled);
}

/// A panic while the retention cache was locked must not make every
/// later nightly run panic too.
#[tokio::test]
async fn test_poisoned_retention_cache_still_returns_last_known_policy() {
    let settings = temps_config::ImageRetentionSettings {
        enabled: false,
        default_hours: 72,
    };
    let service = Arc::new(
        DockerCleanupService::new(Arc::new(DefaultDockerClient), mock_db(), mock_file_store())
            .with_image_retention(&settings),
    );
    let poisoner = Arc::clone(&service);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.last_known_image_retention.lock().unwrap();
        panic!("poison the retention cache");
    })
    .join();
    assert!(service.last_known_image_retention.is_poisoned());

    assert_eq!(service.current_image_retention().await, (false, 72));
}

#[test]
fn test_out_of_range_operator_setting_is_clamped() {
    // A hand-edited settings row must not be able to produce a cutoff that
    // deletes images the moment they are built.
    let zero = temps_config::ImageRetentionSettings {
        enabled: true,
        default_hours: 0,
    };
    let huge = temps_config::ImageRetentionSettings {
        enabled: true,
        default_hours: 999_999,
    };

    assert_eq!(zero.effective_default_hours(), 1);
    assert_eq!(huge.effective_default_hours(), 8760);
}

/// A protection resolves its name through the local index to the image it
/// names: an uploaded tag protects that image's ID.
#[test]
fn test_protection_records_the_image_id() {
    let uploaded = "temps-demo-prod:upload-1750000000";
    let index = LocalImageIndex::new(vec![local_image("sha256:up", 1, &[uploaded], &[])]);
    let mut protected = HashSet::new();
    DockerCleanupService::protect_image(&mut protected, &index, uploaded);
    assert_eq!(protected, HashSet::from(["sha256:up".to_string()]));
}

/// Protection used to skip any name without a `temps-` prefix, which
/// would have left a builder-produced `<slug>:latest` tag unprotected
/// once such tags became candidates. It must apply to any name now.
#[test]
fn test_protect_applies_to_builder_tags_without_temps_prefix() {
    let index = LocalImageIndex::new(vec![local_image("sha256:b", 1, &["my-app-12:latest"], &[])]);
    let mut protected = HashSet::new();
    DockerCleanupService::protect_image(&mut protected, &index, "my-app-12:latest");
    assert!(protected.contains("sha256:b"));
}

/// A protection recorded under one spelling of a Docker Hub name covers a
/// candidate recorded under another: Docker resolves both to one image.
/// So does a recorded name with no tag, which Docker pulls as `:latest`.
#[test]
fn test_protection_matches_every_docker_hub_spelling() {
    let index = LocalImageIndex::new(vec![
        local_image("sha256:n", 1, &["nginx:1"], &[]),
        local_image("sha256:l", 1, &["redis:latest"], &[]),
    ]);
    let mut protected = HashSet::new();
    DockerCleanupService::protect_image(&mut protected, &index, "index.docker.io/nginx:1");
    DockerCleanupService::protect_image(&mut protected, &index, "docker.io/library/redis");
    assert_eq!(
        protected,
        HashSet::from(["sha256:n".to_string(), "sha256:l".to_string()])
    );
}

/// `app:v1` and `app@sha256:...` are one image: removing its last tag also
/// drops the digest, so a deployment that recorded either one protects it.
/// An image ID recorded by a pull protects it too.
#[test]
fn test_protection_covers_every_reference_of_an_image() {
    let index = LocalImageIndex::new(vec![local_image(
        "sha256:app",
        1,
        &["ghcr.io/example-org/app:v1"],
        &["ghcr.io/example-org/app@sha256:d1"],
    )]);
    for protected_as in [
        "ghcr.io/example-org/app:v1",
        "ghcr.io/example-org/app@sha256:d1",
        "sha256:app",
    ] {
        let mut protected = HashSet::new();
        DockerCleanupService::protect_image(&mut protected, &index, protected_as);
        assert!(protected.contains("sha256:app"), "{protected_as}");
    }
    // A name that is not on this daemon protects nothing.
    let mut protected = HashSet::new();
    DockerCleanupService::protect_image(&mut protected, &index, "ghcr.io/other/app:v1");
    assert!(protected.is_empty());
}

/// A tag a pull moved to a newer image after the pass chose it is kept,
/// not removed: removing it by name would delete the newer image. An image
/// ID always names the same image, so removing by ID needs no check.
#[test]
fn test_a_moved_reference_is_retained() {
    let removal = ImageRemoval {
        reference: "registry.example.com/team/app:latest".to_string(),
        image_id: "sha256:old".to_string(),
    };
    assert!(removal.needs_identity_check());
    assert_eq!(removal.moved_outcome("sha256:old"), None);
    let moved = removal.moved_outcome("sha256:new");
    assert!(
        matches!(
            moved,
            Some(ImageRemovalOutcome {
                ref image_name,
                status: ImageDeleteStatus::Retained { ref reason },
            }) if image_name == "registry.example.com/team/app:latest"
                && reason.contains("sha256:new")
                && reason.contains("sha256:old")
        ),
        "{moved:?}"
    );
    assert!(!ImageRemoval::by_id("sha256:old").needs_identity_check());
}

/// An expired image is removed only by the names deployments recorded:
/// a tag someone else put on it survives, and an untagged image (a
/// superseded `app:latest`) goes by its digest in the deployment's
/// repository.
#[test]
fn test_removal_names_only_cover_what_deployments_recorded() {
    let index = LocalImageIndex::new(vec![
        local_image(
            "sha256:tagged",
            1,
            &["ghcr.io/example-org/app:v1", "mirror.internal/keep/app:v1"],
            &["ghcr.io/example-org/app@sha256:t"],
        ),
        local_image(
            "sha256:superseded",
            1,
            &[],
            &[
                "ghcr.io/example-org/app@sha256:old",
                "mirror.internal/keep/app@sha256:old",
            ],
        ),
    ]);
    let named = vec![canonical_image_reference("ghcr.io/example-org/app:latest")];
    let named_v1 = vec![canonical_image_reference("ghcr.io/example-org/app:v1")];

    assert_eq!(
        index.removal_names("sha256:tagged", &named_v1),
        vec!["ghcr.io/example-org/app:v1".to_string()]
    );
    // Linked only by the ID its pull recorded: the tag it carries now was
    // never recorded by a deployment, so it is not ours to drop.
    assert!(index.removal_names("sha256:tagged", &named).is_empty());
    assert_eq!(
        index.removal_names("sha256:superseded", &named),
        vec!["ghcr.io/example-org/app@sha256:old".to_string()]
    );
    assert!(index.removal_names("sha256:missing", &named).is_empty());
}

/// The candidate query sees each reference with how `removal_names` could
/// remove the image by it, so it can drop an unremovable image before its
/// `LIMIT`: tags of a tagged image, digests only of an untagged one.
#[test]
fn test_query_input_marks_how_each_reference_removes_its_image() {
    let index = LocalImageIndex::new(vec![
        local_image(
            "sha256:tagged",
            1,
            &["ghcr.io/example-org/app:v1"],
            &["ghcr.io/example-org/app@sha256:t"],
        ),
        local_image(
            "sha256:untagged",
            1,
            &[],
            &["ghcr.io/example-org/app@sha256:u"],
        ),
    ]);

    let mut rows: Vec<(String, String, Option<String>)> = index
        .query_input()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row[0].as_str().unwrap_or_default().to_string(),
                row[1].as_str().unwrap_or_default().to_string(),
                row[2].as_str().map(str::to_string),
            )
        })
        .collect();
    rows.sort();

    let row = |id: &str, reference: &str, removable_as: Option<&str>| {
        (
            id.to_string(),
            reference.to_string(),
            removable_as.map(str::to_string),
        )
    };
    assert_eq!(
        rows,
        vec![
            row("sha256:tagged", "ghcr.io/example-org/app:v1", Some("tag")),
            row("sha256:tagged", "ghcr.io/example-org/app@sha256:t", None),
            row(
                "sha256:untagged",
                "ghcr.io/example-org/app@sha256:u",
                Some("digest")
            ),
        ]
    );
}

#[test]
fn test_canonical_image_reference() {
    for spelling in [
        "nginx:1",
        "docker.io/nginx:1",
        "docker.io/library/nginx:1",
        "index.docker.io/library/nginx:1",
        "index.docker.io/nginx:1",
    ] {
        assert_eq!(
            canonical_image_reference(spelling),
            "docker.io/library/nginx:1",
            "{spelling}"
        );
    }
    assert_eq!(
        canonical_image_reference("org/app:1"),
        "docker.io/org/app:1"
    );
    assert_eq!(
        canonical_image_reference("docker.io/org/app:1"),
        "docker.io/org/app:1"
    );
    assert_eq!(
        canonical_image_reference("nginx@sha256:abc"),
        "docker.io/library/nginx@sha256:abc"
    );
    // No tag and no digest means `:latest`, as Docker stores the pull.
    for (untagged, expected) in [
        ("nginx", "docker.io/library/nginx:latest"),
        ("docker.io/library/nginx", "docker.io/library/nginx:latest"),
        ("org/app", "docker.io/org/app:latest"),
        ("ghcr.io/org/app", "ghcr.io/org/app:latest"),
        ("localhost:5000/app", "localhost:5000/app:latest"),
    ] {
        assert_eq!(canonical_image_reference(untagged), expected, "{untagged}");
        assert_eq!(
            canonical_image_reference(untagged),
            canonical_image_reference(&format!("{untagged}:latest")),
            "{untagged}"
        );
    }
    // Registry-qualified references and image IDs are left alone.
    for unchanged in [
        "ghcr.io/org/app:1",
        "registry.example.com/team/app@sha256:abc",
        "localhost:5000/app:1",
        "localhost/app:1",
        "sha256:abc",
        "",
    ] {
        assert_eq!(canonical_image_reference(unchanged), unchanged);
    }
}

/// The candidate query only considers images present on the local
/// daemon, so an image listing failure must abort the pass rather than
/// guess. The mock DB has no queued results, so reaching any query would
/// also fail.
#[tokio::test]
async fn test_image_listing_failure_skips_retention() {
    let docker = Arc::new(UnlistableDockerClient {
        removed: Default::default(),
    });
    let service = DockerCleanupService::new(docker.clone(), mock_db(), mock_file_store())
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert!(
        docker
            .removed
            .lock()
            .expect("unlistable mock mutex poisoned")
            .is_empty(),
        "no image may be removed when the local image list is unavailable"
    );
}

#[tokio::test]
async fn test_retention_disabled_removes_nothing() {
    let docker = Arc::new(RecordingDockerClient::default());
    let service = DockerCleanupService::new(docker.clone(), mock_db(), mock_file_store())
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: false,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert!(
        docker.removed_sorted().is_empty(),
        "no Docker call may be made when retention is disabled"
    );
}

/// A run where every removal is refused must not report success. Before
/// this, `total_removed == 0` logged "nothing to remove", which reads to an
/// operator as "cleanup is healthy" when it is in fact doing nothing.
#[tokio::test]
async fn test_refused_removals_are_counted_separately() {
    let docker = Arc::new(RecordingDockerClient {
        refuse: vec!["temps-a:1".to_string()],
        ..Default::default()
    });

    let outcomes = docker
        .remove_images(&[
            ImageRemoval::by_id("temps-a:1"),
            ImageRemoval::by_id("temps-b:1"),
        ])
        .await;

    let summary = summarize_removals(
        "test",
        &outcomes,
        &HashMap::from([("temps-b:1".to_string(), 7)]),
    );
    assert_eq!(summary.deleted, 1);
    assert_eq!(summary.retained, 1);
    assert_eq!(summary.failed, 0, "an in-use refusal is not a failure");
    assert_eq!(summary.bytes_reclaimed, 7);
    assert_eq!(
        docker.removed_sorted(),
        vec!["temps-a:1".to_string(), "temps-b:1".to_string()]
    );
}

/// End-to-end proof against a real Docker daemon that
/// `DefaultDockerClient::remove_images` actually removes an unreferenced
/// image and actually *retains* one that a container still references.
///
/// The retention rule is only as good as the non-forced removal underneath
/// it, and that behaviour lives in bollard rather than in our code — so it
/// is worth asserting against the real daemon. Skips gracefully when Docker
/// is unavailable (per project policy, no `#[ignore]`).
///
/// Both images are *built* rather than tagged from a shared base, because
/// `remove_image` on a tag that shares an image ID with another tag merely
/// untags it without consulting container references. Temps builds one
/// `<deployment slug>:latest` tag per deployment, so the single-tag case
/// tested here is the shape that actually ships.
#[tokio::test]
async fn test_real_docker_removes_unused_and_retains_in_use_image() {
    use bollard::query_parameters::{
        BuildImageOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
        RemoveContainerOptions, RemoveImageOptions,
    };
    use bollard::Docker;
    use futures_util::StreamExt as _;

    let Ok(docker) = Docker::connect_with_unix_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    if docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }

    // Base layer for both test images.
    let mut pull = docker.create_image(
        Some(
            CreateImageOptionsBuilder::default()
                .from_image("busybox")
                .tag("latest")
                .build(),
        ),
        None,
        None,
    );
    while let Some(step) = pull.next().await {
        if step.is_err() {
            println!("Could not pull busybox, skipping");
            return;
        }
    }

    // Each image gets its own ID (distinct ENV) so its tag is the sole
    // reference to it -- matching a real per-deployment build.
    async fn build(docker: &Docker, tag: &str, marker: &str) -> bool {
        let dockerfile = format!("FROM busybox:latest\nENV TEMPS_TEST_MARKER={}\n", marker);
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        let mut builder = tar::Builder::new(Vec::new());
        if builder
            .append_data(&mut header, "Dockerfile", dockerfile.as_bytes())
            .is_err()
        {
            return false;
        }
        let Ok(context) = builder.into_inner() else {
            return false;
        };

        let mut build = docker.build_image(
            BuildImageOptionsBuilder::default().t(tag).build(),
            None,
            Some(http_body_util::Either::Left(http_body_util::Full::new(
                bytes::Bytes::from(context),
            ))),
        );
        while let Some(step) = build.next().await {
            if step.is_err() {
                return false;
            }
        }
        true
    }

    let unused = "temps-retention-test-unused:1";
    let in_use = "temps-retention-test-inuse:1";
    if !build(&docker, unused, "unused").await || !build(&docker, in_use, "inuse").await {
        println!("Could not build test images, skipping");
        let opts = Some(RemoveImageOptions {
            force: true,
            ..Default::default()
        });
        let _ = docker.remove_image(unused, opts.clone(), None).await;
        let _ = docker.remove_image(in_use, opts, None).await;
        return;
    }

    // Hold `in_use` with a container, exactly like a deployed service does.
    let container = docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name("temps-retention-test-holder")
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(in_use.to_string()),
                cmd: Some(vec!["true".to_string()]),
                ..Default::default()
            },
        )
        .await;
    let container_id = match container {
        Ok(c) => c.id,
        Err(e) => {
            println!("Could not create holder container ({e}), skipping");
            let opts = Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            });
            let _ = docker.remove_image(unused, opts.clone(), None).await;
            let _ = docker.remove_image(in_use, opts, None).await;
            return;
        }
    };

    let image_id = |name: &'static str| {
        let docker = docker.clone();
        async move {
            docker
                .inspect_image(name)
                .await
                .ok()
                .and_then(|image| image.id)
                .unwrap_or_default()
        }
    };
    let unused_id = image_id(unused).await;
    let in_use_id = image_id(in_use).await;

    // The first removal was selected when `unused` named another image, as
    // after a pull moved the tag: it must leave the image alone.
    let outcomes = DefaultDockerClient
        .remove_images(&[
            ImageRemoval {
                reference: unused.to_string(),
                image_id: in_use_id.clone(),
            },
            ImageRemoval {
                reference: unused.to_string(),
                image_id: unused_id,
            },
            ImageRemoval {
                reference: in_use.to_string(),
                image_id: in_use_id,
            },
        ])
        .await;

    assert_eq!(outcomes.len(), 3, "one outcome per removal: {outcomes:?}");
    let moved_outcome = outcomes[0].clone();
    let unused_outcome = outcomes[1].clone();
    let in_use_outcome = outcomes[2].clone();
    let unused_still_present = docker.inspect_image(unused).await.is_ok();

    // Clean up before asserting so a failing assert cannot leak state.
    let _ = docker
        .remove_container(
            &container_id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    let _ = docker
        .remove_image(
            in_use,
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await;

    assert!(
        matches!(moved_outcome.status, ImageDeleteStatus::Retained { .. }),
        "a reference that no longer names the selected image must be kept: {:?}",
        moved_outcome.status
    );
    assert_eq!(
        unused_outcome.status,
        ImageDeleteStatus::Deleted,
        "an unreferenced expired image must actually be removed"
    );
    assert!(
        !unused_still_present,
        "the unused image must be gone from the daemon after removal"
    );
    assert!(
        matches!(in_use_outcome.status, ImageDeleteStatus::Retained { .. }),
        "an image still referenced by a container must be retained (a 409 \
         conflict), not untagged underneath the service running it: {:?}",
        in_use_outcome.status
    );
}

async fn cleanup_integration_tests_available() -> bool {
    std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
        || tokio::process::Command::new("docker")
            .arg("info")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
}

/// Record the persisted output of a successful `BuildImageJob`, which is
/// what the retention pass treats as proof that Temps built `image_tag`.
async fn record_build(
    db: &sea_orm::DatabaseConnection,
    deployment_id: i32,
    image_tag: &str,
) -> Result<(), sea_orm::DbErr> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{deployment_jobs, types::JobStatus};

    deployment_jobs::ActiveModel {
        deployment_id: Set(deployment_id),
        job_id: Set("build_image".to_string()),
        job_type: Set("BuildImageJob".to_string()),
        name: Set("Build Container Image".to_string()),
        log_id: Set(format!("build-log-{deployment_id}")),
        status: Set(JobStatus::Success),
        outputs: Set(Some(serde_json::json!({ "image_tag": image_tag }))),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// A git project with a tight 1-hour retention override and one
/// environment, so "old" only needs to be a few hours in the past.
async fn retention_fixture(
    db: &sea_orm::DatabaseConnection,
    slug: &str,
) -> Result<
    (
        temps_entities::projects::Model,
        temps_entities::environments::Model,
    ),
    sea_orm::DbErr,
> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::preset::Preset;
    use temps_entities::upstream_config::UpstreamList;
    use temps_entities::{environments, projects};

    let now = chrono::Utc::now();
    let project = projects::ActiveModel {
        name: Set(format!("Project {slug}")),
        slug: Set(slug.to_string()),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("test-repo".to_string()),
        preset: Set(Preset::Dockerfile),
        directory: Set("/".to_string()),
        main_branch: Set("main".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        image_retention_hours: Set(Some(1)),
        ..Default::default()
    }
    .insert(db)
    .await?;

    let environment = environments::ActiveModel {
        project_id: Set(project.id),
        name: Set("Test Environment".to_string()),
        slug: Set("test".to_string()),
        host: Set(format!("{slug}.example.com")),
        upstreams: Set(UpstreamList::default()),
        current_deployment_id: Set(None),
        subdomain: Set(format!("{slug}.example.com")),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await?;

    Ok((project, environment))
}

/// End-to-end proof against a real database that eligibility comes from
/// what the builder recorded, not from a `temps-` name prefix. The
/// project slug deliberately does not start with `temps-`; before the
/// fix, nothing here was ever a candidate. Skips gracefully when
/// Docker/Postgres is unavailable (no `#[ignore]`, per project policy).
#[tokio::test]
async fn test_prune_old_deployment_images_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_entities::{deployments, environments};

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping retention integration test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = retention_fixture(db.as_ref(), "retention-app").await?;
    let now = Utc::now();
    let old = now - Duration::hours(72);

    let insert_deployment =
        |slug: &str, state: &str, image_name: Option<&str>, created_at: chrono::DateTime<Utc>| {
            deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(slug.to_string()),
                state: Set(state.to_string()),
                image_name: Set(image_name.map(str::to_string)),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(created_at),
                updated_at: Set(created_at),
                ..Default::default()
            }
        };

    // Built, superseded and expired: must be removed even though the
    // slug has no `temps-` prefix.
    let expired = insert_deployment(
        "retention-app-1",
        "stopped",
        Some("retention-app-1:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), expired.id, "retention-app-1:latest").await?;

    // Built by an old deployment, reused by a recent rollback: the
    // recent reference keeps it (BOOL_AND over every reference).
    let reused = insert_deployment(
        "retention-app-2",
        "stopped",
        Some("retention-app-2:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), reused.id, "retention-app-2:latest").await?;
    insert_deployment(
        "retention-app-9",
        "stopped",
        Some("retention-app-2:latest"),
        now,
    )
    .insert(db.as_ref())
    .await?;

    // Build succeeded, deploy failed: `image_name` is never written, but
    // the image is on disk and must still expire.
    let failed = insert_deployment("retention-app-3", "failed", None, old)
        .insert(db.as_ref())
        .await?;
    record_build(db.as_ref(), failed.id, "retention-app-3:latest").await?;

    // Same shape, but the deployment is still rolling out: nothing else
    // protects it yet, so the in-flight guard must.
    let in_flight = insert_deployment("retention-app-4", "running", None, old)
        .insert(db.as_ref())
        .await?;
    record_build(db.as_ref(), in_flight.id, "retention-app-4:latest").await?;

    // Built and expired, but the environment is serving it.
    let active = insert_deployment(
        "retention-app-5",
        "completed",
        Some("retention-app-5:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), active.id, "retention-app-5:latest").await?;
    let mut environment_update: environments::ActiveModel = environment.clone().into();
    environment_update.current_deployment_id = Set(Some(active.id));
    environment_update.update(db.as_ref()).await?;

    // A built tag that an image upload later reused under the same
    // name: the upload cannot be rebuilt, so the tag must survive.
    let built_then_uploaded = insert_deployment(
        "retention-app-6",
        "stopped",
        Some("retention-app-6:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;
    record_build(
        db.as_ref(),
        built_then_uploaded.id,
        "retention-app-6:latest",
    )
    .await?;
    deployments::ActiveModel {
        context_vars: Set(Some(serde_json::json!({ "trigger": "image_upload" }))),
        ..insert_deployment(
            "retention-app-7",
            "stopped",
            Some("retention-app-6:latest"),
            old,
        )
    }
    .insert(db.as_ref())
    .await?;

    // Built and expired, but already gone from the daemon (removed on an
    // earlier night): must not be requested again.
    let already_removed = insert_deployment(
        "retention-app-8",
        "stopped",
        Some("retention-app-8:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), already_removed.id, "retention-app-8:latest").await?;

    // Images Temps neither built nor pulled for an image deployment,
    // all present locally and expired: a name recorded with no
    // provenance at all, and a name that merely looks like a builder
    // tag. Neither has a build record or a registry reference, so
    // neither may be touched.
    insert_deployment("retention-app-10", "stopped", Some("nginx:latest"), old)
        .insert(db.as_ref())
        .await?;
    // An expired registry image deployment: it can be pulled again from
    // its registry (a rollback does so), so retention now removes it.
    deployments::ActiveModel {
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_ref: Some("registry.example.com/team/app:1".to_string()),
            ..Default::default()
        })),
        ..insert_deployment(
            "retention-app-11",
            "stopped",
            Some("registry.example.com/team/app:1"),
            old,
        )
    }
    .insert(db.as_ref())
    .await?;
    insert_deployment(
        "retention-app-12",
        "stopped",
        Some("retention-app-12:latest"),
        old,
    )
    .insert(db.as_ref())
    .await?;

    let docker = Arc::new(RecordingDockerClient::with_local_images(&[
        "retention-app-1:latest",
        "retention-app-2:latest",
        "retention-app-3:latest",
        "retention-app-4:latest",
        "retention-app-5:latest",
        "retention-app-6:latest",
        "nginx:latest",
        "registry.example.com/team/app:1",
        "retention-app-12:latest",
    ]));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(0)
        .with_max_deployment_images_per_run(500)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 336,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![
            "registry.example.com/team/app:1".to_string(),
            "retention-app-1:latest".to_string(),
            "retention-app-3:latest".to_string(),
        ],
        "only expired, locally present images Temps built or pulled from a \
         registry may be removed; reused, in-flight, active, uploaded, \
         already-removed and provenance-less images must all survive"
    );

    // Pruning removes the Docker image, never the deployment record.
    let still_present = deployments::Entity::find_by_id(expired.id)
        .one(db.as_ref())
        .await?;
    assert!(
        still_present.is_some(),
        "pruning must never delete the deployment row, only the image"
    );

    Ok(())
}

/// The newest `keep_recent_deployment_images` deployments per
/// project+environment are a rollback floor whatever their age. With
/// three expired built images and a floor of two, only the oldest may go.
#[tokio::test]
async fn test_rollback_floor_protects_newest_built_images() -> Result<(), Box<dyn std::error::Error>>
{
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::deployments;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping rollback floor test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = retention_fixture(db.as_ref(), "floor-app").await?;
    let now = Utc::now();

    for (n, age_hours) in [(1, 96), (2, 72), (3, 48)] {
        let created_at = now - Duration::hours(age_hours);
        let tag = format!("floor-app-{n}:latest");
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("floor-app-{n}")),
            state: Set("stopped".to_string()),
            image_name: Set(Some(tag.clone())),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), deployment.id, &tag).await?;
    }

    let docker = Arc::new(RecordingDockerClient::with_local_images(&[
        "floor-app-1:latest",
        "floor-app-2:latest",
        "floor-app-3:latest",
    ]));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(2)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 336,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec!["floor-app-1:latest".to_string()],
        "the two newest deployments are rollback targets and must keep their images"
    );

    Ok(())
}

/// Regression test for a livelock the `ORDER BY ... LIMIT` bound on
/// `expired_image_candidates` could otherwise introduce: if permanently
/// protected images (here, built tags later reused by uploads)
/// outnumber `max_deployment_images_per_run` and are older than a
/// genuinely removable image, a LIMIT applied *before* protection would
/// fill the entire candidate window with rows `protect_image`
/// immediately discards, reclaiming zero bytes forever instead of "the
/// remainder next run". The protective flags in
/// `expired_image_candidates` exclude those images on the SQL side, so
/// the removable image must still surface with a `LIMIT` far smaller
/// than the protected count.
#[tokio::test]
async fn test_permanently_protected_images_do_not_starve_the_limit_window(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::deployments;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping livelock regression test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = retention_fixture(db.as_ref(), "livelock-app").await?;
    let now = Utc::now();
    let very_old = now - Duration::hours(200);
    let old = now - Duration::hours(72);

    // Three permanently-protected images, older than the one
    // genuinely-removable image below: enough to fill a LIMIT of 2 on
    // their own if protection isn't applied before the LIMIT.
    let mut local_images = Vec::new();
    for i in 0..3 {
        let tag = format!("livelock-app-{i}:latest");
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("livelock-app-{i}")),
            state: Set("stopped".to_string()),
            image_name: Set(Some(tag.clone())),
            // A real upload: `image_uploaded_locally` is what makes it
            // permanently protected. An `external_image_ref` alone no
            // longer does, since a registry image can be pulled again.
            metadata: Set(Some(deployments::DeploymentMetadata {
                external_image_ref: Some(format!("livelock-app-{i}:latest")),
                image_uploaded_locally: true,
                ..Default::default()
            })),
            created_at: Set(very_old),
            updated_at: Set(very_old),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        record_build(db.as_ref(), deployment.id, &tag).await?;
        local_images.push(tag);
    }

    // The one image that should actually be removed: newer than the
    // protected ones (so it would lose an oldest-first LIMIT race
    // against them) but still expired against the 1h retention window.
    let removable = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("livelock-app-3".to_string()),
        state: Set("stopped".to_string()),
        image_name: Set(Some("livelock-app-3:latest".to_string())),
        metadata: Set(Some(deployments::DeploymentMetadata::default())),
        created_at: Set(old),
        updated_at: Set(old),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), removable.id, "livelock-app-3:latest").await?;
    local_images.push("livelock-app-3:latest".to_string());

    let docker = Arc::new(RecordingDockerClient {
        local_images,
        ..Default::default()
    });
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(0)
        // Smaller than the protected-image count: if protection were
        // applied after the LIMIT, the 3 protected images alone would
        // exhaust it and the removable image would never be considered.
        .with_max_deployment_images_per_run(2)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 336,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec!["livelock-app-3:latest".to_string()],
        "a removable image must not be starved out of the LIMIT window by \
         permanently-protected images that will never actually be removed"
    );

    Ok(())
}

/// Regression test for the fix that made retention settings live: before
/// this, `DockerCleanupService` only ever read `AppSettings.image_retention`
/// once at construction, so an operator disabling retention (or changing
/// the default window) after boot had no effect until the process
/// restarted. This proves a settings row written *after* the service was
/// built is picked up on the very next run, with no rebuild in between.
#[tokio::test]
async fn test_disabling_retention_via_settings_takes_effect_without_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    use sea_orm::ActiveModelTrait;
    use temps_entities::preset::Preset;
    use temps_entities::upstream_config::UpstreamList;
    use temps_entities::{deployments, environments, projects};

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping live-settings regression test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let now = chrono::Utc::now();
    let old = now - chrono::Duration::hours(72);

    let project = projects::ActiveModel {
        name: sea_orm::ActiveValue::Set("Live Settings Test Project".to_string()),
        slug: sea_orm::ActiveValue::Set("live-settings-test-project".to_string()),
        repo_owner: sea_orm::ActiveValue::Set("test-owner".to_string()),
        repo_name: sea_orm::ActiveValue::Set("test-repo".to_string()),
        preset: sea_orm::ActiveValue::Set(Preset::Dockerfile),
        directory: sea_orm::ActiveValue::Set("/".to_string()),
        main_branch: sea_orm::ActiveValue::Set("main".to_string()),
        created_at: sea_orm::ActiveValue::Set(now),
        updated_at: sea_orm::ActiveValue::Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let environment = environments::ActiveModel {
        project_id: sea_orm::ActiveValue::Set(project.id),
        name: sea_orm::ActiveValue::Set("Test Environment".to_string()),
        slug: sea_orm::ActiveValue::Set("test".to_string()),
        host: sea_orm::ActiveValue::Set("live-settings-test.example.com".to_string()),
        upstreams: sea_orm::ActiveValue::Set(UpstreamList::default()),
        current_deployment_id: sea_orm::ActiveValue::Set(None),
        subdomain: sea_orm::ActiveValue::Set("live-settings-test.example.com".to_string()),
        created_at: sea_orm::ActiveValue::Set(now),
        updated_at: sea_orm::ActiveValue::Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let expired = deployments::ActiveModel {
        project_id: sea_orm::ActiveValue::Set(project.id),
        environment_id: sea_orm::ActiveValue::Set(environment.id),
        slug: sea_orm::ActiveValue::Set("live-settings-app-1".to_string()),
        state: sea_orm::ActiveValue::Set("stopped".to_string()),
        image_name: sea_orm::ActiveValue::Set(Some("live-settings-app-1:latest".to_string())),
        metadata: sea_orm::ActiveValue::Set(Some(deployments::DeploymentMetadata::default())),
        created_at: sea_orm::ActiveValue::Set(old),
        updated_at: sea_orm::ActiveValue::Set(old),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;
    record_build(db.as_ref(), expired.id, "live-settings-app-1:latest").await?;

    let server_config = Arc::new(
        temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            None,
        )
        .unwrap(),
    );
    let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

    // Settings row starts with retention enabled and a 1-hour window, so
    // the seeded deployment above is expired.
    config_service
        .update_settings(temps_config::AppSettings {
            image_retention: temps_config::ImageRetentionSettings {
                enabled: true,
                default_hours: 1,
            },
            ..Default::default()
        })
        .await?;

    let docker = Arc::new(RecordingDockerClient::with_local_images(&[
        "live-settings-app-1:latest",
    ]));
    // Constructed with a stale "enabled" snapshot on purpose — this is
    // what the service would have looked like right after boot, before
    // the settings row below is written.
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(0)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        })
        .with_config_service(config_service.clone());

    // Operator disables retention via the settings row *after* the
    // service was constructed — no restart, no rebuild.
    config_service
        .update_settings(temps_config::AppSettings {
            image_retention: temps_config::ImageRetentionSettings {
                enabled: false,
                default_hours: 1,
            },
            ..Default::default()
        })
        .await?;

    service.prune_old_deployment_images().await;

    assert!(
        docker.removed_sorted().is_empty(),
        "disabling retention after construction must be honored on the very \
         next run, not require a restart"
    );

    Ok(())
}

/// The worker's nightly run applies both local rules and removes exactly
/// what they select.
#[tokio::test]
async fn worker_nightly_cleanup_prunes_deployment_and_infra_images() {
    let mut images = app_images(1..=6);
    images.push(gateway_image(1));
    images.push(gateway_image(2));
    let docker = Arc::new(RecordingDockerClient::with_inventory(
        images,
        vec![serving(6)],
    ));
    let scheduler = DockerOnlyCleanupScheduler::new(docker.clone());

    scheduler.perform_cleanup().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![app_tag(1), "sha256:gw1".to_string()]
    );
}

/// The console prunes deployment images with the database policy; its
/// local pass must only touch infrastructure images.
#[tokio::test]
async fn console_local_pass_only_removes_infra_images() {
    let mut images = app_images(1..=8);
    images.push(gateway_image(1));
    images.push(gateway_image(2));
    let docker: Arc<RecordingDockerClient> = Arc::new(RecordingDockerClient::with_inventory(
        images,
        vec![serving(8)],
    ));
    let client: Arc<dyn DockerClient> = docker.clone();

    prune_local_images_without_database(
        &client,
        LocalImagePrunePolicy::default(),
        LocalPrunePasses::InfraOnly,
    )
    .await;

    assert_eq!(docker.removed_sorted(), vec!["sha256:gw1".to_string()]);
}

/// Without a container listing the in-use check would be guessing, so
/// nothing may be removed.
#[tokio::test]
async fn local_pruning_fails_closed_when_the_daemon_cannot_be_listed() {
    let docker = Arc::new(UnlistableDockerClient {
        removed: Default::default(),
    });
    let client: Arc<dyn DockerClient> = docker.clone();

    prune_local_images_without_database(
        &client,
        LocalImagePrunePolicy::default(),
        LocalPrunePasses::WorkerNode,
    )
    .await;

    assert!(docker
        .removed
        .lock()
        .expect("unlistable mock mutex poisoned")
        .is_empty());
}

#[test]
fn removal_outcomes_classify_docker_responses() {
    let conflict = ImageRemovalOutcome::from_docker(
        "a:1",
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "image is being used by stopped container abc".to_string(),
        }),
    );
    assert!(matches!(
        conflict.status,
        ImageDeleteStatus::Retained { .. }
    ));

    let gone = ImageRemovalOutcome::from_docker(
        "a:1",
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "No such image".to_string(),
        }),
    );
    assert_eq!(gone.status, ImageDeleteStatus::NotFound);

    let failed = ImageRemovalOutcome::from_docker(
        "a:1",
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "boom".to_string(),
        }),
    );
    assert!(
        matches!(&failed.status, ImageDeleteStatus::Failed { reason } if reason.contains("a:1"))
    );

    let untagged = ImageRemovalOutcome::from_docker(
        "a:1",
        Ok(vec![bollard::models::ImageDeleteResponseItem {
            untagged: Some("a:1".to_string()),
            deleted: None,
        }]),
    );
    assert_eq!(untagged.status, ImageDeleteStatus::Untagged);
}

// ------------------------------------------------------------------
// Registry images under the database retention policy (fix 1)
// ------------------------------------------------------------------

/// `retention_fixture`, as a `docker_image` project: the shape of a
/// project deployed with `temps deploy:image`.
async fn registry_fixture(
    db: &sea_orm::DatabaseConnection,
    slug: &str,
) -> Result<
    (
        temps_entities::projects::Model,
        temps_entities::environments::Model,
    ),
    sea_orm::DbErr,
> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    let (project, environment) = retention_fixture(db, slug).await?;
    let mut update: temps_entities::projects::ActiveModel = project.into();
    update.source_type = Set(temps_entities::source_type::SourceType::DockerImage);
    let project = update.update(db).await?;
    Ok((project, environment))
}

fn registry_deployment(
    project_id: i32,
    environment_id: i32,
    slug: &str,
    image: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> temps_entities::deployments::ActiveModel {
    use sea_orm::ActiveValue::Set;
    use temps_entities::deployments;
    deployments::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(environment_id),
        slug: Set(slug.to_string()),
        state: Set("stopped".to_string()),
        image_name: Set(Some(image.to_string())),
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_ref: Some(image.to_string()),
            deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
            ..Default::default()
        })),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        ..Default::default()
    }
}

/// Fix 1: registry-pulled images expire like built ones — the newest N
/// per project+environment and anything inside the retention window are
/// kept, older ones are removed. A rollback row that reused an image
/// (no image metadata of its own) does not pin it forever.
#[tokio::test]
async fn test_registry_images_expire_beyond_the_floor_and_window(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping registry retention test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "registry-app").await?;
    let now = Utc::now();
    let image = |n: u32| format!("registry.example.com/team/app:sha-{n}");

    // Retention window is 1h (fixture override); floor is 1.
    for (n, age) in [
        (1, Duration::hours(96)),
        (2, Duration::hours(72)),
        (3, Duration::hours(48)),
        (4, Duration::minutes(10)),
        (5, Duration::minutes(1)),
    ] {
        registry_deployment(
            project.id,
            environment.id,
            &format!("registry-app-{n}"),
            &image(n),
            now - age,
        )
        .insert(db.as_ref())
        .await?;
    }
    // An old rollback onto sha-1: no metadata of its own.
    temps_entities::deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set("registry-app-6".to_string()),
        state: Set("stopped".to_string()),
        image_name: Set(Some(image(1))),
        metadata: Set(Some(temps_entities::deployments::DeploymentMetadata {
            is_rollback: true,
            ..Default::default()
        })),
        context_vars: Set(Some(serde_json::json!({ "trigger": "rollback" }))),
        created_at: Set(now - Duration::hours(90)),
        updated_at: Set(now - Duration::hours(90)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await?;

    let local: Vec<String> = (1..=5).map(image).collect();
    let local: Vec<&str> = local.iter().map(String::as_str).collect();
    let docker = Arc::new(RecordingDockerClient::with_local_images(&local));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(1)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 336,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![image(1), image(2), image(3)],
        "sha-5 is the rollback floor and sha-4 is inside the retention window"
    );
    Ok(())
}

/// A deployment pinned by digest pulls an image Docker lists with no tag,
/// only `repo@sha256:...`. It must still expire, by that reference.
#[tokio::test]
async fn test_registry_images_pulled_by_digest_expire() -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::ActiveModelTrait;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping digest retention test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "digest-app").await?;
    let now = Utc::now();
    let image = |n: u32| format!("registry.example.com/team/app@sha256:{n:064x}");

    for (n, age) in [(1, Duration::hours(96)), (2, Duration::hours(72))] {
        registry_deployment(
            project.id,
            environment.id,
            &format!("digest-app-{n}"),
            &image(n),
            now - age,
        )
        .insert(db.as_ref())
        .await?;
    }

    // Pulled by digest and never tagged: no `repo_tags` at all.
    let inventory = (1..=2)
        .map(|n| LocalImage {
            id: format!("sha256:image{n}"),
            repo_tags: Vec::new(),
            repo_digests: vec![image(n)],
            created: i64::from(n),
            size_bytes: 1024,
        })
        .collect();
    let docker = Arc::new(RecordingDockerClient::with_inventory(inventory, Vec::new()));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(1)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![image(1)],
        "the older digest-pinned image expires; the newest is the rollback floor"
    );
    Ok(())
}

/// An upload recorded as `example/shared:1` and an expired registry pull
/// recorded as `docker.io/example/shared:1` name the same local image, so
/// the upload protects it. An expired pull with no such twin, recorded in
/// the long form while Docker lists the short one, is still removed.
#[tokio::test]
async fn test_registry_retention_matches_docker_hub_spellings(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::deployments;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping Docker Hub spelling test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "spelling-app").await?;
    let (pid, eid) = (project.id, environment.id);
    let old = Utc::now() - Duration::hours(72);

    deployments::ActiveModel {
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_ref: Some("example/shared:1".to_string()),
            image_uploaded_locally: true,
            uploaded_image_id: Some("sha256:upload".to_string()),
            ..Default::default()
        })),
        context_vars: Set(Some(serde_json::json!({ "trigger": "image_upload" }))),
        ..registry_deployment(pid, eid, "spelling-app-1", "example/shared:1", old)
    }
    .insert(db.as_ref())
    .await?;
    registry_deployment(
        pid,
        eid,
        "spelling-app-2",
        "docker.io/example/shared:1",
        old,
    )
    .insert(db.as_ref())
    .await?;
    registry_deployment(
        pid,
        eid,
        "spelling-app-3",
        "docker.io/library/old-tool:1",
        old,
    )
    .insert(db.as_ref())
    .await?;
    // The newest deployment absorbs the rollback floor.
    registry_deployment(
        pid,
        eid,
        "spelling-app-4",
        "registry.example.com/team/app:current",
        Utc::now(),
    )
    .insert(db.as_ref())
    .await?;

    // Docker lists Docker Hub images by their short names.
    let docker = Arc::new(RecordingDockerClient::with_local_images(&[
        "example/shared:1",
        "old-tool:1",
        "registry.example.com/team/app:current",
    ]));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(1)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec!["old-tool:1".to_string()],
        "the upload's image is kept whatever spelling the registry row used"
    );
    Ok(())
}

/// Record the persisted output of a successful `PullExternalImageJob`:
/// the local image ID the registry pull resolved.
async fn record_pull(
    db: &sea_orm::DatabaseConnection,
    deployment_id: i32,
    image_id: &str,
) -> Result<(), sea_orm::DbErr> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{deployment_jobs, types::JobStatus};

    deployment_jobs::ActiveModel {
        deployment_id: Set(deployment_id),
        job_id: Set("pull_external_image".to_string()),
        job_type: Set("PullExternalImageJob".to_string()),
        name: Set("Pull External Image".to_string()),
        log_id: Set(format!("pull-log-{deployment_id}")),
        status: Set(JobStatus::Success),
        outputs: Set(Some(serde_json::json!({ "image_id": image_id }))),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(())
}

/// Every deployment pulls `app:latest`, which moves on to the newest
/// image and leaves the older ones untagged (digest only). No row names
/// those digests, so they are matched by the image ID each pull recorded:
/// the ones past the window and the floor go, the floor keeps the rest.
#[tokio::test]
async fn test_superseded_mutable_tag_images_expire_by_recorded_identity(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::ActiveModelTrait;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping mutable tag retention test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "latest-app").await?;
    let now = Utc::now();
    let latest = "registry.example.com/team/app:latest";
    let digest = |n: u32| format!("registry.example.com/team/app@sha256:{n:064x}");

    for (n, age) in [
        (1, Duration::hours(96)),
        (2, Duration::hours(72)),
        (3, Duration::hours(48)),
    ] {
        let deployment = registry_deployment(
            project.id,
            environment.id,
            &format!("latest-app-{n}"),
            latest,
            now - age,
        )
        .insert(db.as_ref())
        .await?;
        record_pull(db.as_ref(), deployment.id, &format!("sha256:image{n}")).await?;
    }

    // Only the newest still carries the tag.
    let inventory = (1..=3)
        .map(|n| LocalImage {
            id: format!("sha256:image{n}"),
            repo_tags: if n == 3 {
                vec![latest.to_string()]
            } else {
                Vec::new()
            },
            repo_digests: vec![digest(n)],
            created: i64::from(n),
            size_bytes: 1024,
        })
        .collect();
    let docker = Arc::new(RecordingDockerClient::with_inventory(inventory, Vec::new()));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(2)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![digest(1)],
        "image 1 is past the window and the floor of two; images 2 and 3 are the floor"
    );
    Ok(())
}

/// Project A's only deployment is old, so its image is the oldest
/// expired one, but it is also A's rollback floor. It must not take the
/// run's only slot: project B's expired image behind it is removed.
#[tokio::test]
async fn test_rollback_floor_images_do_not_fill_the_run_window(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::ActiveModelTrait;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping rollback floor window test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let now = Utc::now();
    let (project_a, environment_a) = registry_fixture(db.as_ref(), "floor-a").await?;
    let (project_b, environment_b) = registry_fixture(db.as_ref(), "floor-b").await?;
    let image = |project: &str, tag: &str| format!("registry.example.com/{project}/app:{tag}");

    for (project, environment, slug, reference, age) in [
        (
            &project_a,
            &environment_a,
            "floor-a-1",
            image("a", "v1"),
            200,
        ),
        (
            &project_b,
            &environment_b,
            "floor-b-1",
            image("b", "v1"),
            150,
        ),
        (
            &project_b,
            &environment_b,
            "floor-b-2",
            image("b", "v2"),
            100,
        ),
    ] {
        registry_deployment(
            project.id,
            environment.id,
            slug,
            &reference,
            now - Duration::hours(age),
        )
        .insert(db.as_ref())
        .await?;
    }

    let inventory = [("a", "v1", 1), ("b", "v1", 2), ("b", "v2", 3)]
        .into_iter()
        .map(|(project, tag, n)| LocalImage {
            id: format!("sha256:image{n}"),
            repo_tags: vec![image(project, tag)],
            repo_digests: vec![format!(
                "registry.example.com/{project}/app@sha256:{n:064x}"
            )],
            created: i64::from(n),
            size_bytes: 1024,
        })
        .collect();
    let docker = Arc::new(RecordingDockerClient::with_inventory(inventory, Vec::new()));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(1)
        .with_max_deployment_images_per_run(1)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![image("b", "v1")],
        "A's floor image is kept without using the run's only slot"
    );
    Ok(())
}

/// Expired images linked only by their recorded pull ID, that now carry
/// only a tag someone else added, are kept: no recorded name removes them.
/// They must not fill the `LIMIT` window either, or every run would select
/// the same kept images and never reach a removable one behind them.
#[tokio::test]
async fn test_unremovable_pulled_images_do_not_fill_the_run_window(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::ActiveModelTrait;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping retention window test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "window-app").await?;
    let now = Utc::now();
    let latest = "registry.example.com/team/app:latest";
    let digest = |n: u32| format!("registry.example.com/team/app@sha256:{n:064x}");

    for n in 1..=5u32 {
        let deployment = registry_deployment(
            project.id,
            environment.id,
            &format!("window-app-{n}"),
            latest,
            now - Duration::hours(i64::from(200 - n * 10)),
        )
        .insert(db.as_ref())
        .await?;
        record_pull(db.as_ref(), deployment.id, &format!("sha256:image{n}")).await?;
    }

    // Images 1-3 (the oldest) were retagged by hand after `latest` moved on;
    // image 4 is a plain superseded pull; image 5 still carries `latest`.
    let inventory = (1..=5u32)
        .map(|n| LocalImage {
            id: format!("sha256:image{n}"),
            repo_tags: match n {
                1..=3 => vec![format!("registry.example.com/team/app:kept-{n}")],
                5 => vec![latest.to_string()],
                _ => Vec::new(),
            },
            repo_digests: vec![digest(n)],
            created: i64::from(n),
            size_bytes: 1024,
        })
        .collect();
    let docker = Arc::new(RecordingDockerClient::with_inventory(inventory, Vec::new()));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(1)
        // Fewer slots than kept images: if they were selected, image 4
        // would never be reached.
        .with_max_deployment_images_per_run(1)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![digest(4)],
        "the hand-tagged images are kept without using the run's only slot"
    );
    Ok(())
}

/// `app:v1` and `app@sha256:...` are one image. A recent deployment that
/// recorded the digest keeps it, even though an expired one recorded the
/// tag: removing the tag would remove the digest with it.
#[tokio::test]
async fn test_a_recent_reference_by_digest_keeps_an_image_its_tag_expired(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::ActiveModelTrait;

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping tag/digest retention test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "pair-app").await?;
    let tag = "registry.example.com/team/app:v1";
    let digest = format!("registry.example.com/team/app@sha256:{:064x}", 1);

    registry_deployment(
        project.id,
        environment.id,
        "pair-app-1",
        tag,
        Utc::now() - Duration::hours(96),
    )
    .insert(db.as_ref())
    .await?;
    // Inside the retention window, and not the floor (floor is 0).
    registry_deployment(
        project.id,
        environment.id,
        "pair-app-2",
        &digest,
        Utc::now() - Duration::minutes(5),
    )
    .insert(db.as_ref())
    .await?;

    let inventory = vec![LocalImage {
        id: "sha256:pair".to_string(),
        repo_tags: vec![tag.to_string()],
        repo_digests: vec![digest.clone()],
        created: 1,
        size_bytes: 1024,
    }];
    let docker = Arc::new(RecordingDockerClient::with_inventory(inventory, Vec::new()));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(0)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 1,
        });

    service.prune_old_deployment_images().await;

    assert!(
        docker.removed_sorted().is_empty(),
        "the digest reference is inside the window: {:?}",
        docker.removed_sorted()
    );
    Ok(())
}

/// Fix 1's safety side: an upload (even one sharing a name with a
/// registry deployment), an image known only by external-image ID, the
/// image an environment serves, and one a live container on this host
/// runs are all kept. A registry image whose container was removed is
/// the positive control.
#[tokio::test]
async fn test_registry_retention_keeps_uploads_serving_and_live_images(
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{deployment_containers, deployments, environments};

    if !cleanup_integration_tests_available().await {
        eprintln!("Docker/Postgres unavailable; skipping registry protection test");
        return Ok(());
    }

    let test_db = temps_test_utils::TestDatabase::with_migrations().await?;
    let db = test_db.connection_arc();
    let (project, environment) = registry_fixture(db.as_ref(), "guard-app").await?;
    let old = Utc::now() - Duration::hours(72);
    let (pid, eid) = (project.id, environment.id);

    // An uploaded tarball.
    deployments::ActiveModel {
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_ref: Some("guard-app:upload-1".to_string()),
            image_uploaded_locally: true,
            uploaded_image_id: Some("sha256:upload".to_string()),
            ..Default::default()
        })),
        context_vars: Set(Some(serde_json::json!({ "trigger": "image_upload" }))),
        ..registry_deployment(pid, eid, "guard-app-1", "guard-app:upload-1", old)
    }
    .insert(db.as_ref())
    .await?;

    // An upload whose tag a registry deployment later used too: the
    // local image may be the uploaded content, which no registry has.
    let shared = "registry.example.com/team/shared:1";
    registry_deployment(pid, eid, "guard-app-2", shared, old)
        .insert(db.as_ref())
        .await?;
    deployments::ActiveModel {
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_ref: Some(shared.to_string()),
            image_uploaded_locally: true,
            ..Default::default()
        })),
        ..registry_deployment(pid, eid, "guard-app-3", shared, old)
    }
    .insert(db.as_ref())
    .await?;

    // Known only by external-image ID: no registry reference to pull.
    deployments::ActiveModel {
        metadata: Set(Some(deployments::DeploymentMetadata {
            external_image_id: Some(42),
            ..Default::default()
        })),
        ..registry_deployment(pid, eid, "guard-app-4", "idonly:1", old)
    }
    .insert(db.as_ref())
    .await?;

    // Served by the environment.
    let live = "registry.example.com/team/live:1";
    let serving = registry_deployment(pid, eid, "guard-app-5", live, old)
        .insert(db.as_ref())
        .await?;
    let mut env_update: environments::ActiveModel = environment.clone().into();
    env_update.current_deployment_id = Set(Some(serving.id));
    env_update.update(db.as_ref()).await?;

    // A container on this host still runs it.
    let running = "registry.example.com/team/running:1";
    let running_deployment = registry_deployment(pid, eid, "guard-app-6", running, old)
        .insert(db.as_ref())
        .await?;
    // Its container was removed: the positive control.
    let gone = "registry.example.com/team/gone:1";
    let gone_deployment = registry_deployment(pid, eid, "guard-app-7", gone, old)
        .insert(db.as_ref())
        .await?;
    for (deployment_id, image, deleted_at) in [
        (running_deployment.id, running, None),
        (gone_deployment.id, gone, Some(old)),
    ] {
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            container_id: Set(format!("container-{deployment_id}")),
            container_name: Set(format!("guard-app-{deployment_id}")),
            health_state: Set("healthy".to_string()),
            image_name: Set(Some(image.to_string())),
            status: Set(Some("running".to_string())),
            deployed_at: Set(old),
            deleted_at: Set(deleted_at),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
    }

    let docker = Arc::new(RecordingDockerClient::with_local_images(&[
        "guard-app:upload-1",
        shared,
        "idonly:1",
        live,
        running,
        gone,
    ]));
    let service = DockerCleanupService::new(docker.clone(), db.clone(), mock_file_store())
        .with_keep_recent_deployment_images(0)
        .with_image_retention(&temps_config::ImageRetentionSettings {
            enabled: true,
            default_hours: 336,
        });

    service.prune_old_deployment_images().await;

    assert_eq!(
        docker.removed_sorted(),
        vec![gone.to_string()],
        "uploads, ID-only external images, the serving image and a live \
         container's image must all survive"
    );
    Ok(())
}
