// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

// ------------------------------------------------------------------
// Database-free pruning (worker nodes, superseded infrastructure)
// ------------------------------------------------------------------

pub(crate) fn local_image(id: &str, created: i64, tags: &[&str], digests: &[&str]) -> LocalImage {
    LocalImage {
        id: id.to_string(),
        repo_tags: tags.iter().map(|tag| tag.to_string()).collect(),
        repo_digests: digests.iter().map(|digest| digest.to_string()).collect(),
        created,
        size_bytes: 100,
    }
}

pub(crate) fn container(image: &str, image_id: &str, is_temps_deployment: bool) -> LocalContainer {
    LocalContainer {
        image: image.to_string(),
        image_id: image_id.to_string(),
        is_temps_deployment,
    }
}

pub(crate) const APP_REPO: &str = "ghcr.io/example-org/app";

/// A registry-pulled build of the app: tagged and carrying a digest from
/// its own repository. `n` doubles as its build time.
pub(crate) fn app_image(n: i64) -> LocalImage {
    local_image(
        &format!("sha256:app{n}"),
        n,
        &[&format!("{APP_REPO}:sha-{n}")],
        &[&format!("{APP_REPO}@sha256:digest{n}")],
    )
}

pub(crate) fn app_tag(n: i64) -> String {
    format!("{APP_REPO}:sha-{n}")
}

pub(crate) fn app_images(range: std::ops::RangeInclusive<i64>) -> Vec<LocalImage> {
    range.map(app_image).collect()
}

/// The currently deployed container, which is what marks the repository
/// as one Temps deploys on this node.
pub(crate) fn serving(n: i64) -> LocalContainer {
    container(&app_tag(n), &format!("sha256:app{n}"), true)
}

#[test]
fn image_repository_strips_tags_digests_and_docker_hub_prefixes() {
    assert_eq!(
        image_repository("ghcr.io/o/app:v1").as_deref(),
        Some("ghcr.io/o/app")
    );
    assert_eq!(
        image_repository("ghcr.io/o/app@sha256:abc").as_deref(),
        Some("ghcr.io/o/app")
    );
    assert_eq!(
        image_repository("registry:5000/team/app").as_deref(),
        Some("registry:5000/team/app"),
        "a registry port is not a tag"
    );
    assert_eq!(
        image_repository("registry:5000/team/app:2").as_deref(),
        Some("registry:5000/team/app")
    );
    assert_eq!(
        image_repository("docker.io/library/nginx:1").as_deref(),
        Some("nginx")
    );
    assert_eq!(
        image_repository("docker.io/org/tool:1").as_deref(),
        Some("org/tool")
    );
    assert_eq!(image_repository("nginx").as_deref(), Some("nginx"));
    assert_eq!(image_repository("sha256:0123"), None);
    assert_eq!(image_repository(""), None);
}

#[test]
fn infra_repositories_are_an_exact_allowlist() {
    assert!(is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-preview-gateway"
    ));
    assert!(is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-sandbox-nodejs"
    ));
    assert!(is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-sandbox-node"
    ));
    // The server image an operator may run (and roll back to) and the
    // managed-database images are in the same namespace but not ours.
    assert!(!is_temps_managed_infra_repository("ghcr.io/gotempsh/temps"));
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/mariadb-walg"
    ));
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-sandbox-"
    ));
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-preview-gateway-fork"
    ));
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/someone/temps-sandbox-nodejs"
    ));
    // Shaped like a sandbox repository, but not one Temps publishes.
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-sandbox-nodejs-fork"
    ));
    assert!(!is_temps_managed_infra_repository(
        "ghcr.io/gotempsh/temps-sandbox-custom"
    ));
}

/// Fix 2: a worker running image deployments keeps the newest N images
/// of each deployment repository and removes the rest, oldest first.
#[test]
fn node_pruning_keeps_the_newest_images_of_a_deployment_repository() {
    let images = app_images(1..=8);
    let policy = LocalImagePrunePolicy::default();

    let removals = plan_node_deployment_image_removals(&images, &[serving(8)], &policy);

    assert_eq!(removals, vec![app_tag(1), app_tag(2), app_tag(3)]);
}

/// No image a container uses, running or stopped, by ID or by the
/// reference it was created from, is ever selected.
#[test]
fn node_pruning_never_removes_an_image_a_container_uses() {
    let images = app_images(1..=8);
    let containers = vec![
        serving(8),
        // A stopped deployment container still pins its image.
        container(&app_tag(1), "sha256:app1", true),
        // A container that is not a Temps deployment, holding sha-2 by ID.
        container("sha256:app2", "sha256:app2", false),
    ];

    let removals = plan_node_deployment_image_removals(
        &images,
        &containers,
        &LocalImagePrunePolicy::default(),
    );

    assert_eq!(removals, vec![app_tag(3)]);
}

/// Images in a repository no Temps deployment container on this node was
/// created from are a user's (or a managed service's), however old.
#[test]
fn node_pruning_never_touches_images_outside_temps_deployment_repositories() {
    let mut images = app_images(1..=3);
    for n in 1..=8 {
        images.push(local_image(
            &format!("sha256:tool{n}"),
            n,
            &[&format!("ghcr.io/someone/tool:{n}")],
            &[&format!("ghcr.io/someone/tool@sha256:t{n}")],
        ));
    }
    let containers = vec![
        serving(3),
        container("ghcr.io/someone/tool:8", "sha256:tool8", false),
    ];

    let removals = plan_node_deployment_image_removals(
        &images,
        &containers,
        &LocalImagePrunePolicy {
            keep_newest_deployment_images: 1,
            ..Default::default()
        },
    );

    assert_eq!(
        removals,
        vec![app_tag(1), app_tag(2)],
        "only the Temps deployment repository may be pruned"
    );
}

/// An image with no registry digest was transferred from the control
/// plane or built on this node: this copy may be the only one, so it is
/// never removed by the local rule.
#[test]
fn node_pruning_keeps_images_it_cannot_pull_again() {
    let mut images = app_images(4..=8);
    for n in 1..=3 {
        images.push(local_image(
            &format!("sha256:app{n}"),
            n,
            &[&app_tag(n)],
            &[],
        ));
    }
    // A digest from another repository does not make it re-pullable here.
    images.push(local_image(
        "sha256:app0",
        0,
        &[&app_tag(0)],
        &["ghcr.io/someone-else/app@sha256:x"],
    ));

    let removals = plan_node_deployment_image_removals(
        &images,
        &[serving(8)],
        &LocalImagePrunePolicy {
            keep_newest_deployment_images: 2,
            ..Default::default()
        },
    );

    assert_eq!(removals, vec![app_tag(4), app_tag(5), app_tag(6)]);
}

/// A deployment pinned by digest (`repo@sha256:...`) pulls an image that
/// has no tag at all. It must still age out, named by its digest
/// reference, while a digest-pinned image a container runs is kept.
#[test]
fn node_pruning_removes_untagged_images_pulled_by_digest() {
    let digest_ref = |n: i64| format!("{APP_REPO}@sha256:digest{n}");
    let images: Vec<LocalImage> = (1..=4)
        .map(|n| local_image(&format!("sha256:app{n}"), n, &[], &[&digest_ref(n)]))
        .collect();
    let containers = vec![
        // Serving the newest, created from its digest reference.
        container(&digest_ref(4), "sha256:app4", true),
        // A stopped deployment container still pins the oldest.
        container(&digest_ref(1), "sha256:app1", true),
    ];

    let removals = plan_node_deployment_image_removals(
        &images,
        &containers,
        &LocalImagePrunePolicy {
            keep_newest_deployment_images: 1,
            ..Default::default()
        },
    );

    assert_eq!(removals, vec![digest_ref(2), digest_ref(3)]);
}

/// A tagged image is removed by its tag only: dropping the last tag also
/// drops its digest references, so naming both would be redundant.
#[test]
fn removable_references_prefer_tags_over_digests() {
    let tagged = app_image(1);
    assert_eq!(tagged.removable_references(), &[app_tag(1)]);
    let untagged = local_image("sha256:x", 1, &[], &["ghcr.io/example-org/app@sha256:x"]);
    assert_eq!(
        untagged.removable_references(),
        &["ghcr.io/example-org/app@sha256:x".to_string()]
    );
    assert!(local_image("sha256:y", 1, &[], &[])
        .removable_references()
        .is_empty());
}

#[test]
fn node_pruning_is_capped_per_run() {
    let removals = plan_node_deployment_image_removals(
        &app_images(1..=20),
        &[serving(20)],
        &LocalImagePrunePolicy {
            max_removals_per_run: 2,
            ..Default::default()
        },
    );

    assert_eq!(removals, vec![app_tag(1), app_tag(2)], "oldest first");
}

pub(crate) fn gateway_image(n: i64) -> LocalImage {
    // Pinned by digest: no tag, only a digest reference (`<none>` in
    // `docker images`, and never matched by `dangling=true`).
    local_image(
        &format!("sha256:gw{n}"),
        n,
        &[],
        &[&format!(
            "{TEMPS_PREVIEW_GATEWAY_REPOSITORY}@sha256:gwdigest{n}"
        )],
    )
}

/// Fix 3: superseded gateway/sandbox versions are removed only when no
/// container (running or stopped) references them; the newest version of
/// each repository is kept.
#[test]
fn superseded_infra_images_are_removed_only_when_unreferenced() {
    let images = vec![
        gateway_image(1),
        gateway_image(2),
        gateway_image(3),
        gateway_image(4),
        local_image(
            "sha256:node032",
            1,
            &["ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.2"],
            &["ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:n032"],
        ),
        local_image(
            "sha256:node034",
            5,
            &["ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4"],
            &["ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:n034"],
        ),
    ];
    // A stopped sandbox still on the second gateway version.
    let containers = vec![container(
        &format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}@sha256:gwdigest2"),
        "sha256:gw2",
        false,
    )];

    let removals = plan_superseded_infra_image_removals(
        &images,
        &containers,
        &[],
        &LocalImagePrunePolicy::default(),
    );

    assert_eq!(
        removals,
        vec![
            "sha256:gw1".to_string(),
            "sha256:node032".to_string(),
            "sha256:gw3".to_string(),
        ]
    );
}

#[test]
fn pinned_infra_images_are_kept_even_when_unused() {
    let images = vec![gateway_image(1), gateway_image(2)];
    let pinned = format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}@sha256:gwdigest1");

    let removals = plan_superseded_infra_image_removals(
        &images,
        &[],
        &[pinned.as_str()],
        &LocalImagePrunePolicy::default(),
    );

    assert!(removals.is_empty(), "{removals:?}");
}

/// A local build under the official repository name may be the only copy:
/// one with no registry digest, or a `:dev` tag (never pulled), is kept
/// even when newer versions exist and no container uses it.
#[test]
fn infra_pruning_keeps_local_builds_it_cannot_pull_again() {
    let images = vec![
        local_image(
            "sha256:devbuild",
            1,
            &[&format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}:dev")],
            &[],
        ),
        // Pushed somewhere, so it has a digest, but tagged `:dev`.
        local_image(
            "sha256:devpushed",
            2,
            &[&format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}:dev")],
            &[&format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}@sha256:dev")],
        ),
        local_image(
            "sha256:nodelocal",
            1,
            &["ghcr.io/gotempsh/temps-sandbox-nodejs:local"],
            &[],
        ),
        local_image(
            "sha256:node035",
            5,
            &["ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.5"],
            &["ghcr.io/gotempsh/temps-sandbox-nodejs@sha256:n035"],
        ),
        gateway_image(3),
        gateway_image(4),
    ];

    let removals =
        plan_superseded_infra_image_removals(&images, &[], &[], &LocalImagePrunePolicy::default());

    assert_eq!(
        removals,
        vec!["sha256:gw3".to_string()],
        "only the superseded registry gateway image may go"
    );
}

/// Only images whose every reference is in a Temps infrastructure
/// repository are considered.
#[test]
fn infra_pruning_never_touches_other_images() {
    let images = vec![
        gateway_image(9),
        local_image("sha256:server1", 1, &["ghcr.io/gotempsh/temps:v1"], &[]),
        local_image("sha256:server2", 2, &["ghcr.io/gotempsh/temps:v2"], &[]),
        local_image(
            "sha256:walg",
            1,
            &[],
            &["ghcr.io/gotempsh/mariadb-walg@sha256:w"],
        ),
        local_image("sha256:user", 1, &["nginx:1"], &["nginx@sha256:u"]),
        // An old gateway version a user also tagged into their own repo.
        local_image(
            "sha256:gwcopy",
            1,
            &["registry.internal/mirror/gateway:old"],
            &[&format!("{TEMPS_PREVIEW_GATEWAY_REPOSITORY}@sha256:copy")],
        ),
        local_image("sha256:untagged", 1, &[], &[]),
    ];

    let removals =
        plan_superseded_infra_image_removals(&images, &[], &[], &LocalImagePrunePolicy::default());

    assert!(removals.is_empty(), "{removals:?}");
}
