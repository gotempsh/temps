// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    let request = ImageRemovalRequest::new("app-1:latest", Some(APP.to_string())).with_owner_guard(
        OwnerGuard {
            keep_unlabelled_when_shared: true,
            ..guard(Some("staging.marker"))
        },
    );
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

/// The page is chosen before anything is converted, so a listing far
/// larger than the page keeps exactly the oldest `max` after the cursor.
#[test]
fn a_page_keeps_only_the_oldest_after_the_cursor() {
    let images: Vec<LabelledImage> = (0..50)
        .rev()
        .map(|n| listed(&format!("img-{n:02}"), n / 2))
        .collect();
    let page = page_labelled_images(images.clone(), None, 5);
    let ids: Vec<&str> = page.images.iter().map(|i| i.image_id.as_str()).collect();
    assert_eq!(ids, ["img-00", "img-01", "img-02", "img-03", "img-04"]);
    assert_eq!(page.unlisted, 45);
    let after = page.next.as_ref().expect("more to list");
    assert_eq!((after.created, after.image_id.as_str()), (2, "img-04"));
    let rest = page_labelled_images(images.clone(), Some(after), 100);
    assert_eq!(rest.images.len(), 45);
    assert_eq!(rest.images[0].image_id, "img-05");
    assert_eq!((rest.unlisted, rest.next), (0, None));
    let none = page_labelled_images(images, None, 0);
    assert!(none.images.is_empty());
    assert_eq!(none.unlisted, 50);
}

/// Only an image without an owner label, under a guard that keeps such
/// images on a shared host, needs the daemon-wide listing of owners; a
/// batch of labelled images never lists them.
#[test]
fn only_an_unlabelled_image_needs_the_shared_host_listing() {
    let keeping = OwnerGuard {
        keep_unlabelled_when_shared: true,
        ..guard(Some("a.b"))
    };
    let request = ImageRemovalRequest::new("app-1:latest", None).with_owner_guard(keeping);
    let unlabelled = inspected(APP, &["app-1:latest"]);
    assert_eq!(
        guard_needing_shared_host(&request, &unlabelled).map(|g| g.label.as_str()),
        Some(OWNER_LABEL)
    );
    for owner in ["a.b", "other"] {
        let mut labelled = inspected(APP, &["app-1:latest"]);
        labelled
            .labels
            .insert(OWNER_LABEL.to_string(), owner.to_string());
        assert!(guard_needing_shared_host(&request, &labelled).is_none());
    }
    let sweeping =
        ImageRemovalRequest::new("app-1:latest", None).with_owner_guard(guard(Some("a.b")));
    assert!(guard_needing_shared_host(&sweeping, &unlabelled).is_none());
    let unguarded = ImageRemovalRequest::new("app-1:latest", None);
    assert!(guard_needing_shared_host(&unguarded, &unlabelled).is_none());
}

#[test]
fn a_request_without_a_guard_round_trips_for_older_agents() {
    let json =
        serde_json::to_value(ImageRemovalRequest::new("app-1:latest", None)).expect("serialize");
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
    let requests = vec![ImageRemovalRequest::new("app:latest", None); MAX_IMAGE_REMOVAL_BATCH + 1];
    let result = remove_unused_images(&docker, &requests).await;
    assert!(matches!(
        result,
        Err(DockerUtilError::ImageBatchTooLarge { requested, .. }) if requested == MAX_IMAGE_REMOVAL_BATCH + 1
    ));
}

/// Build a single-tag image with its own ID, as a per-deployment build is.
async fn build_unique_image(docker: &Docker, tag: &str) -> Result<(), Box<dyn std::error::Error>> {
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
        crate::test_support::docker_or_skip("removes_only_unused_images_on_a_real_daemon").await
    else {
        return;
    };
    if let Err(error) = pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await {
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
    if let Err(error) = pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await {
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
                &[("sh.temps.test.owner", owner.as_str())],
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
    if let Err(error) = pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await {
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
    let request = ImageRemovalRequest::new(unlabelled.clone(), None).with_owner_guard(OwnerGuard {
        label: label.clone(),
        owner: Some("this-install".to_string()),
        keep_unlabelled_when_shared: true,
    });

    let ours = format!("{}:latest", crate::test_support::unique_name("shared-ours"));
    let their_container = crate::test_support::unique_name("shared-their-container");
    let our_container = crate::test_support::unique_name("shared-our-container");
    let create = |name: String, image: String| {
        let docker = docker.clone();
        async move {
            docker
                .create_container(
                    Some(
                        bollard::query_parameters::CreateContainerOptionsBuilder::new()
                            .name(&name)
                            .build(),
                    ),
                    bollard::models::ContainerCreateBody {
                        image: Some(image),
                        cmd: Some(vec!["true".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
        }
    };
    let remove_container = |name: String| {
        let docker = docker.clone();
        async move {
            let _ = docker
                .remove_container(
                    &name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    };

    // Only this install's own container: the daemon is not shared.
    let prepared = async {
        build_labelled_unique_image(&docker, &ours, &label, "this-install").await?;
        create(our_container.clone(), ours.clone()).await?;
        build_labelled_unique_image(&docker, &theirs, &label, "another-install").await?;
        build_unique_image(&docker, &unlabelled).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let alone_with_ours = if prepared.is_ok() {
        Some(another_owner_present(&docker, &label, Some("this-install")).await)
    } else {
        None
    };
    // Another install runs a container from its image: shared.
    let prepared = match prepared {
        Ok(()) => create(their_container.clone(), theirs.clone())
            .await
            .map_err(Into::into),
        Err(error) => Err(error),
    };
    let while_shared = if prepared.is_ok() {
        Some(remove_unused_images(&docker, std::slice::from_ref(&request)).await)
    } else {
        None
    };
    let kept = docker.inspect_image(&unlabelled).await.is_ok();
    remove_container(their_container).await;
    remove_container(our_container).await;
    force_remove_image(&docker, &theirs).await;
    force_remove_image(&docker, &ours).await;
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
    assert_eq!(
        alone_with_ours,
        Some(false),
        "this install's own containers do not make the daemon shared"
    );
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

/// The owner probe pages past this owner's own containers, newest first,
/// to an older container of another owner, and answers yes when it runs
/// out of pages before it could rule one out.
#[tokio::test]
async fn the_owner_probe_pages_past_this_owners_containers() {
    use crate::pull::{pull_image, PullSpec};

    let Some(docker) =
        crate::test_support::docker_or_skip("the_owner_probe_pages_past_this_owners_containers")
            .await
    else {
        return;
    };
    if let Err(error) = pull_image(&docker, PullSpec::new(crate::test_support::TEST_IMAGE)).await {
        println!("Could not pull the test image, skipping: {error}");
        return;
    }
    let label = format!("sh.temps.test.owner.{}", uuid::Uuid::new_v4().simple());
    let names: Vec<String> = ["theirs", "ours-1", "ours-2"]
        .iter()
        .map(|name| crate::test_support::unique_name(&format!("probe-{name}")))
        .collect();
    let mut prepared: Result<(), String> = Ok(());
    // Created oldest first: another owner's, then two of ours.
    for (name, owner) in names
        .iter()
        .zip(["another-install", "this-install", "this-install"])
    {
        let created = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(crate::test_support::TEST_IMAGE.to_string()),
                    cmd: Some(vec!["true".to_string()]),
                    labels: Some(HashMap::from([(label.clone(), owner.to_string())])),
                    ..Default::default()
                },
            )
            .await;
        if let Err(error) = created {
            prepared = Err(format!("create {name}: {error}"));
            break;
        }
    }
    let paged = probe_other_owner(&docker, &label, Some("this-install"), 1, 8).await;
    let gave_up = probe_other_owner(&docker, &label, Some("this-install"), 1, 2).await;
    let theirs_only = probe_other_owner(&docker, &label, Some("another-install"), 1, 8).await;
    // With only ours left, the answer needs every page read through:
    // a `before` filter the daemon rejected would answer yes.
    let _ = docker
        .remove_container(
            &names[0],
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    let ours_only = probe_other_owner(&docker, &label, Some("this-install"), 1, 8).await;
    for name in &names {
        let _ = docker
            .remove_container(
                name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }
    let none_left = probe_other_owner(&docker, &label, Some("this-install"), 1, 8).await;

    if let Err(error) = prepared {
        panic!("preparing the owner probe test failed: {error}");
    }
    assert!(
        paged,
        "another owner's older container is found a page at a time"
    );
    assert!(gave_up, "running out of pages keeps unlabelled images");
    assert!(
        theirs_only,
        "for the other install, ours are the foreign ones"
    );
    assert!(
        !ours_only,
        "paging through only our own containers finds no other owner"
    );
    assert!(
        !none_left,
        "with no labelled container the daemon is not shared"
    );
}
