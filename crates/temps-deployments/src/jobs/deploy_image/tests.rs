// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[test]
fn worker_image_platform_must_match_before_transfer_including_same_node() {
    use super::verify_worker_image_platform;
    assert!(
        verify_worker_image_platform("app:1", "linux/arm64", Some("linux/arm64"), "worker").is_ok()
    );
    assert!(
        verify_worker_image_platform("app:1", "linux/arm64", Some("linux/amd64"), "worker")
            .is_err()
    );
    assert!(verify_worker_image_platform("app:1", "linux/arm64", None, "worker").is_err());
}
use super::*;

#[test]
fn readiness_timeout_says_app_never_listened_when_only_connects_failed() {
    let probe = ReadinessProbeState {
        last_failure: Some("connection refused or unreachable".to_string()),
        last_was_connect_error: true,
        ..Default::default()
    };
    let message = probe.timeout_message(3000, "/", 300, 303);
    assert!(message.starts_with("Application never accepted connections on port 3000"));
    assert!(message.contains("readiness limit of 300s"));
    assert!(message.contains("waited 303s"));

    // No probe ever completed (e.g. the budget ran out in phase 1).
    let empty = ReadinessProbeState::default().timeout_message(8080, "/health", 60, 61);
    assert!(empty.contains("never accepted connections on port 8080"));
}

#[test]
fn readiness_timeout_reports_closed_connections_without_claiming_tcp_was_refused() {
    let probe = ReadinessProbeState {
        last_was_request_error: true,
        last_failure: Some("connection closed before HTTP response".to_string()),
        ..Default::default()
    };
    let message = probe.timeout_message(8081, "/", 30, 31);
    assert!(message.contains("never returned an HTTP response on port 8081"));
    assert!(!message.contains("never accepted connections"));
}

#[test]
fn readiness_timeout_after_responses_is_a_slow_app_not_a_wrong_port() {
    let probe = ReadinessProbeState {
        responded: true,
        last_failure: Some("no response within 5s".to_string()),
        ..Default::default()
    };
    let message = probe.timeout_message(8080, "/ready", 120, 125);
    assert!(message.starts_with("Application readiness timed out on port 8080"));
    assert!(message.contains("health checks did not pass"));
    assert!(message.contains("no response within 5s for /ready"));
}

#[test]
fn exit_message_excludes_log_tail_and_excerpt_stays_bounded() {
    let info = temps_deployer::ContainerInfo {
        exit_code: Some(1),
        exit_reason: Some("Exit code 1".to_string()),
        ..Default::default()
    };
    let logs = (0..20)
        .map(|i| format!("line {i}"))
        .chain(std::iter::once(
            "Error: Cannot find module '/app/server.js'".to_string(),
        ))
        .collect::<Vec<_>>()
        .join("\n");
    let message = container_exit_failure_message(&info, 2);
    assert!(message.starts_with("Container exited during startup (Exit code 1) after 2s"));
    assert!(!message.contains("Cannot find module"));
    assert!(log_excerpt(&logs).unwrap().contains("Cannot find module"));
    assert!(
        !message.contains("line 10"),
        "only the last lines are kept: {message}"
    );

    let huge = "x".repeat(5_000);
    let excerpt = log_excerpt(&huge).unwrap();
    assert!(excerpt.chars().count() <= FAILURE_LOG_EXCERPT_CHARS + 1);
    assert!(log_excerpt("\n  \n").is_none());
}

#[test]
fn exit_description_preserves_code_alongside_reason() {
    let oom = temps_deployer::ContainerInfo {
        oom_killed: Some(true),
        exit_code: Some(137),
        ..Default::default()
    };
    assert_eq!(describe_container_exit(&oom), "OOMKilled (exit code 137)");
    let with_reason = temps_deployer::ContainerInfo {
        exit_code: Some(126),
        exit_reason: Some("entrypoint could not execute".to_string()),
        ..Default::default()
    };
    assert_eq!(
        describe_container_exit(&with_reason),
        "exit code 126: entrypoint could not execute"
    );
    let code_only = temps_deployer::ContainerInfo {
        exit_code: Some(3),
        ..Default::default()
    };
    assert_eq!(describe_container_exit(&code_only), "exit code 3");
    assert_eq!(
        describe_container_exit(&temps_deployer::ContainerInfo::default()),
        "no exit code reported"
    );
    let looped = container_crash_loop_message(&code_only, 6);
    assert_eq!(
        looped,
        "Container keeps restarting during startup (restarted 6 times; last exit: exit code 3)"
    );
}

use async_trait::async_trait;

struct TestLogWriter;

#[async_trait]
impl crate::workflow::LogWriter for TestLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

#[test]
fn poisoned_deployment_state_is_recovered_for_cleanup() {
    let state = Arc::new(Mutex::new(vec!["candidate".to_string()]));
    let poison_target = Arc::clone(&state);
    let _ = std::panic::catch_unwind(move || {
        let _guard = poison_target.lock().expect("initial lock");
        panic!("poison deployment state for regression coverage");
    });

    let mut recovered = lock_deployment_state(&state, "test_state");
    recovered.push("cleanup".to_string());

    assert_eq!(recovered.as_slice(), ["candidate", "cleanup"]);
}

/// Each replica's image comes from the worker that built that tag's
/// architecture; a single-builder build keeps its one builder for every
/// tag, and an unknown tag falls back to it rather than to another
/// architecture's builder.
#[test]
fn each_tag_is_fetched_from_the_worker_that_built_it() {
    let mut output = build_output_with_tags(&[
        ("linux/amd64", "myapp:latest"),
        ("linux/arm64", "myapp:latest-arm64"),
    ]);
    output.builder_node_id = Some(4);
    output.builder_node_ids_by_platform = HashMap::from([
        ("linux/amd64".to_string(), 4),
        ("linux/arm64".to_string(), 5),
    ]);

    let arm_tag = output.tag_for_platform(Some("linux/arm64")).to_string();
    let amd_tag = output.tag_for_platform(Some("linux/amd64")).to_string();
    assert_eq!(arm_tag, "myapp:latest-arm64");
    assert_eq!(output.builder_node_for_tag(&arm_tag), Some(5));
    assert_eq!(output.builder_node_for_tag(&amd_tag), Some(4));
    assert_eq!(output.builder_node_for_tag("myapp:unknown"), Some(4));
    output.image_ids_by_platform = HashMap::from([
        ("linux/amd64".to_string(), "sha256:amd".to_string()),
        ("linux/arm64".to_string(), "sha256:arm".to_string()),
    ]);
    assert_eq!(
        output.expected_image_id_for_tag(&arm_tag),
        Some("sha256:arm")
    );
    assert_eq!(output.expected_image_id_for_tag("myapp:unknown"), None);

    let mut single = build_output_with_tags(&[("linux/arm64", "myapp:latest")]);
    single.builder_node_id = Some(7);
    assert_eq!(single.builder_node_for_tag("myapp:latest"), Some(7));
    assert_eq!(
        build_output_with_tags(&[]).builder_node_for_tag("myapp:latest"),
        None
    );
}

#[test]
fn reused_worker_build_reads_only_a_matching_worker_build() {
    let outputs = serde_json::json!({
        "image_tag": "app:latest",
        "image_id": "sha256:amd",
        "builder_node_id": 4,
        "image_tags_by_platform": {
            "linux/amd64": "app:latest",
            "linux/arm64": "app:latest-arm64"
        },
        "builder_node_ids_by_platform": { "linux/amd64": 4, "linux/arm64": 5 },
        "image_ids_by_platform": { "linux/amd64": "sha256:amd", "linux/arm64": "sha256:arm" }
    });
    let reused = ReusedWorkerBuild::from_build_outputs(&outputs, "app:latest").unwrap();
    assert_eq!(
        reused
            .image_ids_by_platform
            .get("linux/arm64")
            .map(String::as_str),
        Some("sha256:arm")
    );
    // Per-architecture builders without the id of each image (a build
    // recorded before ids were) are never trusted as image sources.
    let mut without_ids = outputs.clone();
    without_ids["image_ids_by_platform"] = serde_json::json!({ "linux/amd64": "sha256:amd" });
    assert!(ReusedWorkerBuild::from_build_outputs(&without_ids, "app:latest").is_none());
    assert!(reused.is_per_architecture());
    assert_eq!(
        reused.builder_node_ids_by_platform.get("linux/arm64"),
        Some(&5)
    );
    assert_eq!(reused.image_id.as_deref(), Some("sha256:amd"));

    // Another tag, a control-plane build (no builder), or a worker build
    // without verified platforms is not reused.
    assert!(ReusedWorkerBuild::from_build_outputs(&outputs, "app:v2").is_none());
    let mut local = outputs.clone();
    local["builder_node_id"] = serde_json::Value::Null;
    assert!(ReusedWorkerBuild::from_build_outputs(&local, "app:latest").is_none());
    let mut unverified = outputs.clone();
    unverified["image_tags_by_platform"] = serde_json::json!({});
    assert!(ReusedWorkerBuild::from_build_outputs(&unverified, "app:latest").is_none());

    // A single-worker build is reusable but not per-architecture.
    let single = serde_json::json!({
        "image_tag": "app:latest",
        "builder_node_id": 7,
        "image_tags_by_platform": { "linux/arm64": "app:latest" }
    });
    let single = ReusedWorkerBuild::from_build_outputs(&single, "app:latest").unwrap();
    assert!(!single.is_per_architecture());
    assert_eq!(single.builder_node_id, Some(7));
}

fn inspect_body(id: &str, platform: &str) -> String {
    serde_json::json!({
        "id": id, "architecture": temps_deployer::platform::platform_arch(platform),
        "os": "linux", "platform": platform, "size_bytes": 1, "tags": ["app:latest-arm64"],
        "created": null, "working_dir": null
    })
    .to_string()
}

/// A single-architecture worker build records no per-platform ids, so its
/// primary tag is bound to the build's own `image_id` -- on a rollback
/// too. A control-plane build and an external image name no id to verify.
#[test]
fn a_single_worker_build_binds_its_primary_tag_to_the_built_image_id() {
    let mut worker = build_output_with_tags(&[("linux/arm64", "myapp:latest")]);
    worker.builder_node_id = Some(7);
    assert_eq!(
        worker.expected_image_id_for_tag("myapp:latest"),
        Some("sha256:abc")
    );
    assert_eq!(worker.expected_image_id_for_tag("myapp:other"), None);

    let control_plane = build_output_with_tags(&[]);
    assert_eq!(
        control_plane.expected_image_id_for_tag("myapp:latest"),
        None
    );

    let mut external = build_output_with_tags(&[("linux/arm64", "myapp:latest")]);
    external.builder_node_id = Some(7);
    external.image_id = format!("{EXTERNAL_IMAGE_ID_PREFIX}myapp-latest");
    assert_eq!(external.expected_image_id_for_tag("myapp:latest"), None);

    // A rollback of a single worker build keeps that binding.
    let reused = ReusedWorkerBuild::from_build_outputs(
        &serde_json::json!({
            "image_tag": "myapp:latest",
            "image_id": "sha256:built",
            "builder_node_id": 7,
            "image_tags_by_platform": { "linux/arm64": "myapp:latest" }
        }),
        "myapp:latest",
    )
    .unwrap();
    let rolled_back = BuildImageOutput {
        image_id: reused.image_id.clone().unwrap(),
        image_tags_by_platform: reused.image_tags_by_platform.clone(),
        builder_node_id: reused.builder_node_id,
        builder_node_ids_by_platform: reused.builder_node_ids_by_platform.clone(),
        image_ids_by_platform: reused.image_ids_by_platform.clone(),
        ..build_output_with_tags(&[])
    };
    assert_eq!(
        rolled_back.expected_image_id_for_tag("myapp:latest"),
        Some("sha256:built")
    );
}

/// A target that already holds exactly the image recorded at build time
/// runs it without its builder being contacted at all -- this job has no
/// scheduler, so reaching for the builder would fail.
#[tokio::test]
async fn a_target_holding_the_recorded_image_needs_nothing_from_its_builder() {
    let url = spawn_sequenced_agent(1, |request| {
        assert!(request.contains("/agent/images/inspect?"), "{request}");
        ("200 OK", inspect_body("sha256:arm", "linux/arm64"))
    })
    .await;
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(url, "token".into(), "w-arm".into())
            .unwrap()
            .with_platform(Some("linux/arm64".into())),
    );
    let job = job_with_target(Arc::new(TrackingMockContainerDeployer::new()));
    let context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
    job.ensure_worker_image_on_remote(
        "app:latest-arm64",
        Some("sha256:arm"),
        &remote,
        "w-arm",
        &context,
        Some(5),
        Some(9),
    )
    .await
    .expect("the recorded image is already on the target");
}

/// A builder that now holds a different image under the tag -- rebuilt,
/// or a node row re-registered by another machine -- is refused instead
/// of being trusted as the image's source.
#[tokio::test]
async fn a_builder_serving_another_image_under_the_tag_is_refused() {
    let url = spawn_sequenced_agent(2, |request| {
        assert!(request.contains("/agent/images/inspect?"), "{request}");
        ("200 OK", inspect_body("sha256:tampered", "linux/arm64"))
    })
    .await;
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(url, "token".into(), "w-arm".into())
            .unwrap()
            .with_platform(Some("linux/arm64".into())),
    );
    let job = job_with_target(Arc::new(TrackingMockContainerDeployer::new()));
    let context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
    // Same node builds and runs it: the owner is the target itself.
    let error = job
        .ensure_worker_image_on_remote(
            "app:latest-arm64",
            Some("sha256:arm"),
            &remote,
            "w-arm",
            &context,
            Some(5),
            Some(5),
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("sha256:tampered"), "{message}");
    assert!(message.contains("refusing to run it"), "{message}");
}

/// A rollback of a mixed-architecture build schedules on exactly the
/// architectures the original build covered, with no control-plane
/// Docker to inspect.
#[tokio::test]
async fn reused_worker_build_offers_every_architecture_to_the_scheduler() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder {
        platform: "linux/amd64".into(),
        discovered: None,
        image_platform: None,
        discoverable: None,
        local_platforms: Vec::new(),
        registry_platforms: Vec::new(),
        registry_reads: Default::default(),
    });
    let mut output = build_output_with_tags(&[
        ("linux/amd64", "myapp:latest"),
        ("linux/arm64", "myapp:latest-arm64"),
    ]);
    output.builder_node_id = Some(4);
    let mut platforms = job.available_image_platforms(&output).await;
    platforms.sort();
    assert_eq!(platforms, vec!["linux/amd64", "linux/arm64"]);
}

/// #1789: an image Temps did not build (`deploy:local-image`) that has
/// several architectures present locally is schedulable on each.
#[tokio::test]
async fn local_multi_platform_image_offers_every_architecture_to_the_scheduler() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder {
        platform: "linux/amd64".into(),
        discovered: Some("linux/amd64".into()),
        image_platform: Some("linux/amd64".into()),
        discoverable: None,
        local_platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        registry_platforms: Vec::new(),
        registry_reads: Default::default(),
    });
    let output = build_output_with_tags(&[]);
    let mut platforms = job.available_image_platforms(&output).await;
    platforms.sort();
    assert_eq!(platforms, vec!["linux/amd64", "linux/arm64"]);

    // The same image may run on a control plane of either architecture.
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
    assert!(job
        .verify_image_platform_for_local("myapp:latest", Some("linux/arm64"), &context)
        .await
        .is_ok());
    assert!(job
        .verify_image_platform_for_local("myapp:latest", Some("linux/riscv64"), &context)
        .await
        .is_err());
}

/// A control plane that pulled a multi-platform registry tag for its own
/// architecture: its store holds `linux/amd64`, the tag's index offers
/// `linux/amd64` and `linux/arm64`.
fn amd64_control_plane_with_multi_arch_index() -> PlatformOnlyImageBuilder {
    PlatformOnlyImageBuilder {
        platform: "linux/amd64".into(),
        discovered: Some("linux/amd64".into()),
        image_platform: Some("linux/amd64".into()),
        discoverable: None,
        local_platforms: Vec::new(),
        registry_platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        registry_reads: Default::default(),
    }
}

/// #1892: each worker pulls a registry image itself and gets its own
/// architecture, so an arm64 node is eligible for a multi-platform tag
/// even though the control plane's pull only downloaded amd64.
#[tokio::test]
async fn registry_image_offers_every_architecture_of_its_index() {
    let job = job_with_image_builder(amd64_control_plane_with_multi_arch_index())
        .with_image_source(DeployImageSource::Registry);
    let output = build_output_with_tags(&[]);

    let mut platforms = job.available_image_platforms(&output).await;
    platforms.sort();
    assert_eq!(platforms, vec!["linux/amd64", "linux/arm64"]);
}

/// An uploaded image reaches a worker as an export of the control plane's
/// store, which holds amd64 only: the registry index of a same-named tag
/// says nothing about what that export carries.
#[tokio::test]
async fn control_plane_local_image_offers_only_what_the_store_holds() {
    let job = job_with_image_builder(amd64_control_plane_with_multi_arch_index())
        .with_image_source(DeployImageSource::ControlPlaneLocal);
    let output = build_output_with_tags(&[]);

    assert_eq!(
        job.available_image_platforms(&output).await,
        vec!["linux/amd64"]
    );
}

/// An unreadable registry index (registry down, no daemon to ask) falls
/// back to the local store, as before #1892, instead of disabling the
/// architecture filter or failing the deployment.
#[tokio::test]
async fn registry_image_falls_back_to_the_local_store_without_an_index() {
    let mut builder = amd64_control_plane_with_multi_arch_index();
    builder.registry_platforms = Vec::new();
    let job = job_with_image_builder(builder).with_image_source(DeployImageSource::Registry);
    let output = build_output_with_tags(&[]);

    assert_eq!(
        job.available_image_platforms(&output).await,
        vec!["linux/amd64"]
    );
}

fn build_output_with_tags(tags: &[(&str, &str)]) -> BuildImageOutput {
    BuildImageOutput {
        image_tag: "myapp:latest".to_string(),
        image_id: "sha256:abc".to_string(),
        size_bytes: 1,
        build_context: PathBuf::from("/tmp"),
        dockerfile_path: PathBuf::from("/tmp/Dockerfile"),
        image_tags_by_platform: tags
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect(),
        builder_node_id: None,
        builder_node_ids_by_platform: HashMap::new(),
        image_ids_by_platform: HashMap::new(),
    }
}

fn blocker(
    reason: temps_providers::CrossNodeBlockReason,
) -> crate::services::CrossNodeServiceBlocker {
    crate::services::CrossNodeServiceBlocker {
        service_id: 7,
        service_name: "orders-db".to_string(),
        fqdn: Some("orders-db.temps.local".to_string()),
        detail: reason.detail("orders-db"),
        remedy: reason.remedy().to_string(),
        setup_path: reason.setup_path(7),
    }
}

/// Default (and overwhelmingly common) case: nothing blocks the replica,
/// so a remote deployment proceeds exactly as before.
#[test]
fn no_blockers_never_fails_a_remote_replica() {
    assert!(cross_node_unreachable_error("worker-1", &[]).is_none());
    assert!(DeploymentJobConfig::default()
        .cross_node_service_blockers
        .is_empty());
}

/// The whole point of the guard: the operator gets the node, the
/// service, the reason, and the fix — not a container that silently
/// cannot reach its database.
#[test]
fn blocked_remote_replica_fails_with_an_actionable_message() {
    let error = cross_node_unreachable_error(
        "worker-1",
        &[blocker(
            temps_providers::CrossNodeBlockReason::ClusterDnsDisabled,
        )],
    )
    .expect("a blocker must produce an error");

    assert!(matches!(
        error,
        WorkflowError::CrossNodeServiceUnreachable {
            blocker_count: 1,
            ..
        }
    ));

    let message = error.to_string();
    assert!(message.contains("worker-1"), "{message}");
    assert!(message.contains("orders-db"), "{message}");
    assert!(message.contains("Cluster DNS is disabled"), "{message}");
    assert!(message.contains("Enable cluster DNS"), "{message}");
}

#[test]
fn every_blocked_service_is_named_in_the_failure() {
    let error = cross_node_unreachable_error(
        "worker-2",
        &[
            blocker(temps_providers::CrossNodeBlockReason::ClusterDnsDisabled),
            crate::services::CrossNodeServiceBlocker {
                service_id: 9,
                service_name: "cache".to_string(),
                ..blocker(temps_providers::CrossNodeBlockReason::DnsRecordMissing)
            },
        ],
    )
    .expect("blockers must produce an error");

    let message = error.to_string();
    assert!(message.contains("2 linked service(s)"), "{message}");
    assert!(message.contains("orders-db"), "{message}");
    assert!(message.contains("cache"), "{message}");
}

#[test]
fn remote_app_ports_only_bind_private_interfaces() {
    for address in ["10.0.0.8", "172.20.0.5", "192.168.1.10", "fd00::5"] {
        assert_eq!(
            private_remote_bind_address(address).expect("private address should be accepted"),
            address
        );
    }

    let public = private_remote_bind_address("203.0.113.10")
        .expect_err("public worker address must not expose an app port");
    assert!(public.to_string().contains("outside the Temps proxy"));
    assert!(private_remote_bind_address("worker.example.com").is_err());

    let expected = PortMapping {
        host_port: 18080,
        container_port: 3000,
        protocol: Protocol::Tcp,
        host_ip: Some("10.0.0.8".to_string()),
    };
    assert!(confirms_private_port_binding(
        std::slice::from_ref(&expected),
        3000,
        18080,
        "10.0.0.8"
    ));

    let legacy_agent = PortMapping {
        host_ip: None,
        ..expected.clone()
    };
    assert!(!confirms_private_port_binding(
        &[legacy_agent],
        3000,
        18080,
        "10.0.0.8"
    ));
    let public_binding = PortMapping {
        host_ip: Some("0.0.0.0".to_string()),
        ..expected
    };
    assert!(!confirms_private_port_binding(
        &[public_binding],
        3000,
        18080,
        "10.0.0.8"
    ));
}

/// Single-arch build: every node gets the one tag, whatever it reports.
#[test]
fn test_tag_for_platform_without_multi_arch_build() {
    let output = build_output_with_tags(&[]);
    assert_eq!(output.tag_for_platform(None), "myapp:latest");
    assert_eq!(output.tag_for_platform(Some("linux/arm64")), "myapp:latest");
}

/// Multi-arch build: each node must receive the image built for it.
#[test]
fn test_tag_for_platform_selects_the_matching_image() {
    let output = build_output_with_tags(&[
        ("linux/amd64", "myapp:latest"),
        ("linux/arm64", "myapp:latest-arm64"),
    ]);

    assert_eq!(output.tag_for_platform(Some("linux/amd64")), "myapp:latest");
    assert_eq!(
        output.tag_for_platform(Some("linux/arm64")),
        "myapp:latest-arm64"
    );
    // Equivalent spellings resolve to the same image.
    assert_eq!(
        output.tag_for_platform(Some("linux/aarch64")),
        "myapp:latest-arm64"
    );
}

/// A node whose platform we don't know, or one we didn't build for, falls
/// back to the primary tag — the deploy path then runs the explicit
/// architecture check before transferring anything.
#[test]
fn test_tag_for_platform_falls_back_to_primary_tag() {
    let output = build_output_with_tags(&[
        ("linux/amd64", "myapp:latest"),
        ("linux/arm64", "myapp:latest-arm64"),
    ]);

    assert_eq!(output.tag_for_platform(None), "myapp:latest");
    assert_eq!(
        output.tag_for_platform(Some("linux/riscv64")),
        "myapp:latest"
    );
}

/// Minimal `ImageBuilder` that only answers "what platform do I run".
struct PlatformOnlyImageBuilder {
    platform: String,
    /// Platform the daemon confirmed, if any. `None` models a control
    /// plane whose `docker info` hasn't answered yet.
    discovered: Option<String>,
    /// Platform `inspect_image` reports for any tag.
    image_platform: Option<String>,
    /// What a discovery attempt would return. `None` models a daemon that
    /// still doesn't answer.
    discoverable: Option<String>,
    /// Every platform a multi-platform image has locally; empty answers
    /// with the inspected platform alone, as a single-platform image does.
    local_platforms: Vec<String>,
    /// What the image's registry index offers; empty models an index
    /// that cannot be read.
    registry_platforms: Vec<String>,
    /// How many times the registry index was asked for.
    registry_reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl PlatformOnlyImageBuilder {
    fn confirmed(platform: &str, image_platform: &str) -> Self {
        Self {
            platform: platform.to_string(),
            discovered: Some(platform.to_string()),
            image_platform: Some(image_platform.to_string()),
            discoverable: None,
            local_platforms: Vec::new(),
            registry_platforms: Vec::new(),
            registry_reads: Default::default(),
        }
    }

    /// A daemon whose platform isn't cached yet but answers when asked —
    /// the state an upload/external-image deploy starts in, since nothing
    /// on that path runs a build.
    fn discoverable_on_demand(fallback: &str, daemon: &str, image_platform: &str) -> Self {
        Self {
            platform: fallback.to_string(),
            discovered: None,
            image_platform: Some(image_platform.to_string()),
            discoverable: Some(daemon.to_string()),
            local_platforms: Vec::new(),
            registry_platforms: Vec::new(),
            registry_reads: Default::default(),
        }
    }
}

#[async_trait]
impl temps_deployer::ImageBuilder for PlatformOnlyImageBuilder {
    async fn build_image(
        &self,
        _request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn build_image_with_callback(
        &self,
        _request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn import_image(
        &self,
        _image_path: PathBuf,
        _tag: &str,
    ) -> Result<String, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn save_image(
        &self,
        _image_name: &str,
        _output_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn inspect_image(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
        let Some(platform) = self.image_platform.clone() else {
            return Err(temps_deployer::BuilderError::ImageNotFound(
                image_name.to_string(),
            ));
        };
        Ok(temps_deployer::ImageInfo {
            id: format!("sha256:{image_name}"),
            architecture: temps_deployer::platform::platform_arch(&platform),
            os: "linux".to_string(),
            platform,
            size_bytes: 1,
            tags: vec![image_name.to_string()],
            created: None,
            working_dir: None,
            config: None,
        })
    }

    async fn image_platforms(
        &self,
        image_name: &str,
    ) -> Result<Vec<String>, temps_deployer::BuilderError> {
        if !self.local_platforms.is_empty() {
            return Ok(self.local_platforms.clone());
        }
        Ok(vec![self.inspect_image(image_name).await?.platform])
    }

    async fn registry_image_platforms(
        &self,
        _image_name: &str,
        _credentials: Option<temps_deployer::remote::RemotePullCredentials>,
    ) -> Result<Vec<String>, temps_deployer::BuilderError> {
        self.registry_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.registry_platforms.clone())
    }

    fn get_native_platform(&self) -> String {
        self.platform.clone()
    }

    fn discovered_platform(&self) -> Option<String> {
        self.discovered.clone()
    }

    async fn ensure_platform_discovered(&self) -> Option<String> {
        self.discovered
            .clone()
            .or_else(|| self.discoverable.clone())
    }
}

fn job_with_image_builder(builder: PlatformOnlyImageBuilder) -> DeployImageJob {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .namespace("default".to_string())
    .image_builder(Arc::new(builder))
    .build(container_deployer)
    .unwrap()
}

fn job_with_local_platform(platform: &str) -> DeployImageJob {
    job_with_image_builder(PlatformOnlyImageBuilder {
        platform: platform.to_string(),
        discovered: Some(platform.to_string()),
        image_platform: None,
        discoverable: None,
        local_platforms: Vec::new(),
        registry_platforms: Vec::new(),
        registry_reads: Default::default(),
    })
}

/// Each replica's record must name the image that replica actually runs.
/// `MarkDeploymentCompleteJob` reads the `image_names` output when writing
/// `deployment_containers`; without it every row falls back to the
/// deployment's primary tag, so on a mixed fleet the node and deployment
/// APIs would report ARM replicas as running the amd64 image.
#[test]
fn test_deployment_output_carries_a_tag_per_replica() {
    let output = DeploymentOutput {
        status: DeploymentStatus::Running,
        replicas: 2,
        resources: ResourceUsage::default(),
        container_ids: vec!["c1".to_string(), "c2".to_string()],
        host_ports: vec![30001, 30002],
        container_port: Some(3000),
        node_ids: vec![None, Some(7)],
        image_names: vec!["app:latest".to_string(), "app:latest-arm64".to_string()],
    };

    // Parallel to container_ids, which is how the consumer indexes them.
    assert_eq!(output.image_names.len(), output.container_ids.len());
    assert_eq!(output.image_names[1], "app:latest-arm64");

    // And it survives the workflow context round-trip the job performs.
    let encoded = serde_json::to_value(&output.image_names).unwrap();
    let decoded: Vec<String> = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, output.image_names);
}

/// Older workflow contexts have no `image_names`; deserialization must not
/// break for a deployment that was in flight across an upgrade.
#[test]
fn test_deployment_output_without_image_names_still_parses() {
    let legacy = serde_json::json!({
        "status": "Running",
        "replicas": 1,
        "resources": {},
        "container_ids": ["c1"],
        "host_ports": [30001],
        "container_port": 3000
    });

    let output: DeploymentOutput = serde_json::from_value(legacy).unwrap();
    assert!(output.image_names.is_empty());
    assert!(output.node_ids.is_empty());
}

/// Remote replicas are checked before the image is transferred; local ones
/// had no equivalent guard, so an image built for another architecture
/// reached the control plane's Docker and failed as a container that won't
/// start — with nothing in the log about architecture.
#[tokio::test]
async fn test_local_deploy_refuses_an_image_for_another_architecture() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder::confirmed(
        "linux/amd64",
        "linux/arm64",
    ));
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    let err = job
        .verify_image_platform_for_local("app:latest-arm64", Some("linux/amd64"), &context)
        .await
        .expect_err("an arm64 image must not be deployed on an amd64 control plane");

    let message = err.to_string();
    assert!(message.contains("linux/arm64"), "got: {message}");
    assert!(message.contains("linux/amd64"), "got: {message}");
    assert!(message.contains("exec format error"), "got: {message}");
}

#[tokio::test]
async fn test_local_deploy_accepts_a_matching_image() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder::confirmed(
        "linux/amd64",
        "linux/amd64",
    ));
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    assert!(job
        .verify_image_platform_for_local("app:latest", Some("linux/amd64"), &context)
        .await
        .is_ok());
}

/// While the control plane's own platform is unconfirmed, comparing
/// against the compiled-in fallback could reject a perfectly good image.
/// Unknown means "proceed", as it did before multi-arch support.
#[tokio::test]
async fn test_local_deploy_skips_the_check_when_the_platform_is_unknown() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder {
        platform: "linux/amd64".to_string(),
        discovered: None,
        image_platform: Some("linux/arm64".to_string()),
        discoverable: None,
        local_platforms: Vec::new(),
        registry_platforms: Vec::new(),
        registry_reads: Default::default(),
    });
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    assert!(job
        .verify_image_platform_for_local("app:latest-arm64", None, &context)
        .await
        .is_ok());
}

/// The "scheduling failed, deploy locally" fallback is a degradation, not
/// a licence to run an image the control plane cannot execute. An uploaded
/// image is accepted when *any* node matches its architecture, so a
/// remote-only image can reach this path — and starting it here would
/// reproduce the `exec format error` this feature exists to prevent.
#[tokio::test]
async fn test_local_fallback_refuses_an_image_the_control_plane_cannot_run() {
    let job = job_with_local_platform("linux/amd64");
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    let err = job
        .ensure_local_can_run(&["linux/arm64".to_string()], &context, "database timeout")
        .await
        .expect_err("an arm64-only image must not fall back onto an amd64 control plane");

    let message = err.to_string();
    assert!(message.contains("linux/arm64"), "got: {message}");
    assert!(message.contains("linux/amd64"), "got: {message}");
    // The operator needs to know why the fallback happened at all.
    assert!(message.contains("database timeout"), "got: {message}");
}

#[tokio::test]
async fn test_local_fallback_allowed_when_the_image_matches() {
    let job = job_with_local_platform("linux/amd64");
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    // Same architecture, and the multi-arch case where one of the built
    // platforms is the control plane's.
    assert!(job
        .ensure_local_can_run(&["linux/amd64".to_string()], &context, "whatever")
        .await
        .is_ok());
    assert!(job
        .ensure_local_can_run(
            &["linux/arm64".to_string(), "linux/x86_64".to_string()],
            &context,
            "whatever"
        )
        .await
        .is_ok());
}

/// An upload or external-image deploy never runs a build, so the daemon's
/// platform may still be undiscovered when the local fallback is
/// considered. Judging that on the binary's architecture would approve the
/// fallback — and the local verification stays quiet while the platform is
/// unknown, so the container would reach `exec format error` unchallenged.
/// Discovery has to happen here.
#[tokio::test]
async fn test_local_fallback_discovers_the_platform_before_authorising() {
    // Binary says amd64; the daemon behind DOCKER_HOST is arm64 and will
    // say so when asked. The image is amd64-only.
    let job = job_with_image_builder(PlatformOnlyImageBuilder::discoverable_on_demand(
        "linux/amd64",
        "linux/arm64",
        "linux/amd64",
    ));
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    let err = job
        .ensure_local_can_run(&["linux/amd64".to_string()], &context, "database timeout")
        .await
        .expect_err("the daemon is arm64, so an amd64-only image cannot run here");

    let message = err.to_string();
    assert!(message.contains("linux/arm64"), "got: {message}");
}

/// Unknown platforms must not start blocking deployments that worked
/// before this feature existed.
#[tokio::test]
async fn test_local_fallback_allowed_when_platforms_are_unknown() {
    let job = job_with_local_platform("linux/amd64");
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    assert!(job
        .ensure_local_can_run(&[], &context, "whatever")
        .await
        .is_ok());
}

#[test]
fn resource_usage_default_is_uncapped() {
    // Regression guard: the default MUST be all-None so any deploy path that
    // builds a job without calling `.resources(...)` (rollback/promote)
    // leaves the container uncapped rather than silently applying a phantom
    // CPU/memory limit. `None` here → `parse_cpu_cores`/`parse_memory_mb`
    // return None → Docker `HostConfig` nano_cpus/memory stay unset.
    let d = ResourceUsage::default();
    assert_eq!(
        d.cpu_limit, None,
        "default cpu_limit must be None (uncapped)"
    );
    assert_eq!(
        d.memory_limit, None,
        "default memory_limit must be None (uncapped)"
    );
    assert_eq!(d.cpu_request, None);
    assert_eq!(d.memory_request, None);
    // And it must parse to no limit at the deployer boundary.
    assert_eq!(d.cpu_limit.as_deref().and_then(parse_cpu_cores), None);
    assert_eq!(d.memory_limit.as_deref().and_then(parse_memory_mb), None);
}

#[test]
fn parse_cpu_cores_handles_millicores_and_whole_cores() {
    assert_eq!(parse_cpu_cores("1000m"), Some(1.0));
    assert_eq!(parse_cpu_cores("500m"), Some(0.5));
    assert_eq!(parse_cpu_cores("2"), Some(2.0));
    assert_eq!(parse_cpu_cores("0.25"), Some(0.25));
    assert_eq!(parse_cpu_cores("  1500m  "), Some(1.5));
    assert_eq!(parse_cpu_cores(""), None);
    assert_eq!(parse_cpu_cores("garbage"), None);
}

#[test]
fn parse_cpu_cores_handles_microcores() {
    // 1_000_000u = 1 core (the storage convention used by temps DB).
    assert_eq!(parse_cpu_cores("1000000u"), Some(1.0));
    assert_eq!(parse_cpu_cores("500000u"), Some(0.5));
    assert_eq!(parse_cpu_cores("2000000u"), Some(2.0));
    assert_eq!(parse_cpu_cores("100000u"), Some(0.1));
    assert_eq!(parse_cpu_cores("  2000000u  "), Some(2.0));
}

#[test]
fn parse_memory_mb_handles_binary_and_decimal_suffixes() {
    assert_eq!(parse_memory_mb("512Mi"), Some(512));
    assert_eq!(parse_memory_mb("1Gi"), Some(1024));
    assert_eq!(parse_memory_mb("2048Ki"), Some(2));
    assert_eq!(parse_memory_mb("1G"), Some(954)); // 1e9 / (1024*1024) ≈ 953.67 → ceil
    assert_eq!(parse_memory_mb("128"), Some(1)); // 128 bytes → ceil to 1 MB
    assert_eq!(parse_memory_mb(""), None);
    assert_eq!(parse_memory_mb("garbage"), None);
}

use temps_deployer::{
    ContainerDeployer, ContainerInfo, ContainerStats, ContainerStatus as DeployerContainerStatus,
    DeployRequest, DeployResult, DeployerError,
};

// Mock ContainerDeployer for testing multi-replica deployments
use std::sync::Mutex as StdMutex;

pub(super) struct TrackingMockContainerDeployer {
    deployed_containers: Arc<StdMutex<Vec<String>>>,
    stopped_containers: Arc<StdMutex<Vec<String>>>,
    removed_containers: Arc<StdMutex<Vec<String>>>,
    /// Containers whose removal the test wants to fail.
    unremovable_containers: Arc<StdMutex<Vec<String>>>,
    requests: Arc<StdMutex<Vec<DeployRequest>>>,
}

impl TrackingMockContainerDeployer {
    pub(super) fn new() -> Self {
        Self {
            deployed_containers: Arc::new(StdMutex::new(Vec::new())),
            stopped_containers: Arc::new(StdMutex::new(Vec::new())),
            removed_containers: Arc::new(StdMutex::new(Vec::new())),
            unremovable_containers: Arc::new(StdMutex::new(Vec::new())),
            requests: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

/// A log store that is broken for the whole test, as a full disk would be:
/// every write fails, including the ones made during cleanup.
struct BrokenLogWriter;

#[async_trait]
impl crate::workflow::LogWriter for BrokenLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        Err(WorkflowError::Other("log storage is full".to_string()))
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

fn job_for_cleanup_test(deployer: Arc<TrackingMockContainerDeployer>) -> DeployImageJob {
    DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("checkout".to_string())
    .build(deployer)
    .expect("valid deploy job")
}

/// Give the job a live log stream, so stopping it has a line to write.
fn start_idle_log_stream(job: &DeployImageJob) {
    *job.log_stream_task.lock().expect("log stream lock") =
        Some(tokio::spawn(std::future::pending()));
}

/// With log storage down, every cleanup log write fails. Each of them used
/// to return early with `?`, before the container it was announcing had
/// been removed, so every failed deployment left its container behind.
#[tokio::test]
async fn cleanup_removes_every_container_while_the_log_writer_stays_broken() {
    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    let job = job_for_cleanup_test(deployer.clone());
    job.track_container("first".to_string(), deployer.clone());
    job.track_container("second".to_string(), deployer.clone());
    start_idle_log_stream(&job);
    let context = WorkflowContext::new("run-1".to_string(), 1, 1, 1, Arc::new(BrokenLogWriter));

    job.cleanup(&context)
        .await
        .expect("a broken log writer must not fail cleanup");

    assert_eq!(
        deployer.removed_containers.lock().unwrap().as_slice(),
        ["first", "second"]
    );
    assert!(job.log_stream_task.lock().unwrap().is_none());
    assert!(
        job.container_ids.lock().unwrap().is_empty(),
        "removed containers must no longer be tracked"
    );
}

/// Logging is what may fail quietly. A real removal failure is still
/// returned, it does not stop the remaining removals, and the container
/// stays tracked so a later cleanup can retry it.
#[tokio::test]
async fn cleanup_with_a_broken_log_writer_still_reports_a_failed_removal() {
    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    deployer
        .unremovable_containers
        .lock()
        .unwrap()
        .push("first".to_string());
    let job = job_for_cleanup_test(deployer.clone());
    job.track_container("first".to_string(), deployer.clone());
    job.track_container("second".to_string(), deployer.clone());
    let context = WorkflowContext::new("run-1".to_string(), 1, 1, 1, Arc::new(BrokenLogWriter));

    let error = job
        .cleanup_container(&context)
        .await
        .expect_err("a failed removal must be reported");

    let message = error.to_string();
    assert!(message.contains("container first"), "{message}");
    assert!(!message.contains("log storage"), "{message}");
    assert_eq!(
        deployer.removed_containers.lock().unwrap().as_slice(),
        ["second"]
    );
    assert_eq!(job.container_ids.lock().unwrap().as_slice(), ["first"]);
}

/// Retaining a failed candidate stops the log stream first. That line going
/// unwritten must not discard the candidate kept for log inspection.
#[tokio::test]
async fn retaining_a_failed_container_survives_a_broken_log_writer() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection(),
    );
    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("checkout".to_string())
    .failed_container_retention(db, 42)
    .build(deployer.clone())
    .expect("valid deploy job");
    job.failed_candidates
        .lock()
        .expect("candidate lock")
        .push(FailedContainerCandidate {
            container_id: "failed-app-id".to_string(),
            container_name: "checkout-production".to_string(),
            container_port: Some(3000),
            host_port: 18080,
            image_name: "checkout:broken".to_string(),
            node_id: None,
        });
    start_idle_log_stream(&job);
    let context = WorkflowContext::new("run-42".to_string(), 42, 2, 3, Arc::new(BrokenLogWriter));

    job.retain_failed_containers(&context)
        .await
        .expect("a broken log writer must not abort retention");

    assert!(job.retained_failure.load(Ordering::Acquire));
    assert_eq!(
        deployer.stopped_containers.lock().unwrap().as_slice(),
        ["failed-app-id"]
    );
    assert!(deployer.removed_containers.lock().unwrap().is_empty());

    // With the failure retained, the executor's later cleanup only stops
    // the stream, and a failed log write there must not surface either.
    start_idle_log_stream(&job);
    job.cleanup(&context)
        .await
        .expect("a broken log writer must not fail cleanup of a retained candidate");
    assert!(job.log_stream_task.lock().unwrap().is_none());
    assert!(deployer.removed_containers.lock().unwrap().is_empty());
}

#[async_trait]
impl ContainerDeployer for TrackingMockContainerDeployer {
    async fn exec_in_container(
        &self,
        container_id: &str,
        _command: Vec<String>,
        _timeout: std::time::Duration,
    ) -> Result<temps_deployer::ExecResult, temps_deployer::DeployerError> {
        Err(temps_deployer::DeployerError::ExecFailed {
            container_id: container_id.to_string(),
            reason: "exec is not scripted by this test deployer".to_string(),
        })
    }

    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        self.requests.lock().unwrap().push(request.clone());

        // Generate unique container ID based on container name
        let container_id = format!("container_{}", request.container_name);

        // Track this deployment
        self.deployed_containers
            .lock()
            .unwrap()
            .push(container_id.clone());

        // Use the port from request
        let host_port = request
            .port_mappings
            .first()
            .map(|p| p.host_port)
            .unwrap_or(8080);
        let container_port = request
            .port_mappings
            .first()
            .map(|p| p.container_port)
            .unwrap_or(8080);

        Ok(DeployResult {
            container_id,
            container_name: request.container_name,
            container_port,
            host_port,
            status: DeployerContainerStatus::Running,
            docker_socket_mounted: false,
        })
    }

    async fn start_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.stopped_containers
            .lock()
            .unwrap()
            .push(container_id.to_string());
        Ok(())
    }

    async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Ok(())
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        if self
            .unremovable_containers
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == container_id)
        {
            return Err(DeployerError::Other(format!(
                "removal of {container_id} refused by test"
            )));
        }
        self.removed_containers
            .lock()
            .unwrap()
            .push(container_id.to_string());
        Ok(())
    }

    async fn get_container_info(
        &self,
        _container_id: &str,
    ) -> Result<ContainerInfo, DeployerError> {
        Ok(ContainerInfo {
            container_id: "test_container_123".to_string(),
            container_name: "test_container".to_string(),
            image_name: "test:latest".to_string(),
            status: DeployerContainerStatus::Running,
            created_at: chrono::Utc::now(),
            ports: vec![],
            environment_vars: HashMap::new(),
            restart_count: Some(0),
            labels: HashMap::new(),
            ..Default::default()
        })
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<ContainerStats, DeployerError> {
        Ok(ContainerStats {
            container_id: container_id.to_string(),
            container_name: "test_container".to_string(),
            cpu_percent: 25.0,
            memory_bytes: 268435456,
            memory_limit_bytes: Some(2147483648),
            memory_percent: Some(12.5),
            network_rx_bytes: 2048000,
            network_tx_bytes: 1024000,
            block_read_bytes: None,
            block_write_bytes: None,
            timestamp: chrono::Utc::now(),
            ..Default::default()
        })
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        Ok(vec![])
    }

    async fn get_container_logs(&self, _container_id: &str) -> Result<String, DeployerError> {
        Ok("test logs".to_string())
    }

    async fn stream_container_logs(
        &self,
        _container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        Err(DeployerError::Other("Not implemented".to_string()))
    }
}

#[tokio::test]
async fn deploying_a_replica_count_saved_above_the_maximum_fails_before_allocating() {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test_deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "registry.test.com".to_string(),
        network: Some("test-network".to_string()),
    })
    .service_name("myapp".to_string())
    .namespace("production".to_string())
    .replicas(temps_entities::deployment_config::MAX_REPLICAS.unsigned_abs() + 1)
    .build(container_deployer)
    .unwrap();
    let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

    let error = job.validate_deployment_config(&context).await.unwrap_err();

    assert!(
        matches!(&error, WorkflowError::JobValidationFailed(m)
                if m.contains("myapp") && m.contains("maximum")),
        "{error}"
    );
}

#[test]
fn test_deploy_image_job_builder() {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let target = DeploymentTarget::Docker {
        registry_url: "registry.test.com".to_string(),
        network: Some("test-network".to_string()),
    };

    let mut env_vars = HashMap::new();
    env_vars.insert("ENV".to_string(), "production".to_string());
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());

    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test_deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(target)
    .service_name("myapp".to_string())
    .namespace("production".to_string())
    .replicas(3)
    .environment_variables(env_vars)
    .failed_container_retention(db, 42)
    .build(container_deployer)
    .unwrap();

    assert_eq!(job.job_id(), "test_deploy");
    assert_eq!(job.build_job_id, "build_image");
    assert_eq!(job.config.service_name, "myapp");
    assert_eq!(job.config.namespace, "production");
    assert_eq!(job.deployment_id, Some(42));
    assert!(job.failed_container_db.is_some());
    assert_eq!(job.config.replicas, 3);
    assert_eq!(job.depends_on(), vec!["build_image".to_string()]);
}

#[tokio::test]
async fn failed_app_container_is_registered_without_becoming_ready() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection(),
    );
    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("checkout".to_string())
    .failed_container_retention(db.clone(), 42)
    .build(deployer.clone())
    .expect("valid deploy job");
    job.failed_candidates
        .lock()
        .expect("candidate lock")
        .push(FailedContainerCandidate {
            container_id: "failed-app-id".to_string(),
            container_name: "checkout-production".to_string(),
            container_port: Some(3000),
            host_port: 18080,
            image_name: "checkout:broken".to_string(),
            node_id: Some(7),
        });
    let context = WorkflowContext::new("run-42".to_string(), 42, 2, 3, Arc::new(TestLogWriter));

    job.retain_failed_containers(&context)
        .await
        .expect("failed candidate should remain available for logs");
    assert!(job.retained_failure.load(Ordering::Acquire));
    assert_eq!(
        deployer.stopped_containers.lock().unwrap().as_slice(),
        ["failed-app-id"],
        "retained failures must be stopped before their ownership rows are committed"
    );

    drop(job);
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
    let transaction_log = db.into_transaction_log();
    let rendered = transaction_log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .filter(|statement| {
            statement
                .sql
                .contains("INSERT INTO \"deployment_containers\"")
        })
        .map(|statement| format!("{statement:?}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("failed-app-id"));
    assert!(rendered.contains("checkout-production"));
    assert!(rendered.contains("retained:stopped-after-failed-readiness"));
    assert!(rendered.contains("checkout:broken"));
    assert!(rendered.contains("18080"));
    assert!(rendered.contains("3000"));
    assert!(
        rendered.contains("Some(7)"),
        "node ownership missing: {rendered}"
    );
    assert!(
        !rendered.contains("ready_at\", Some"),
        "failed candidates must never be routable: {rendered}"
    );
}

/// ADR 045: a gated deployment whose executing host reports it did not
/// mount the socket must fail, not be silently accepted.
#[tokio::test]
async fn a_gated_replica_whose_host_did_not_mount_the_socket_fails_the_deployment() {
    let deployer: Arc<dyn ContainerDeployer> = Arc::new(TrackingMockContainerDeployer::new());
    let job = DeployImageJobBuilder::new(
        "node-daemon",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("node-daemon".to_string())
    .build(deployer.clone())
    .expect("valid deploy job");
    let context = WorkflowContext::new("run-45".to_string(), 45, 2, 3, Arc::new(TestLogWriter));

    // The mock reports `docker_socket_mounted: false`, which is exactly
    // what a host whose agent was never restarted would report.
    let error = job
        .deploy_single_replica(
            "node-daemon:latest",
            &context,
            0,
            None,
            &deployer,
            ReplicaTarget {
                assignment: &crate::services::NodeAssignment::Local,
                docker_socket_required: true,
            },
        )
        .await
        .expect_err("a gated replica without the socket must not be accepted");

    match error {
        WorkflowError::DockerSocketNotMounted {
            ref node,
            ref project_slug,
            env,
        } => {
            assert_eq!(node, "control-plane");
            assert_eq!(project_slug, "node-daemon");
            assert_eq!(
                env,
                temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV
            );
            // The operator reading the deploy log is alone: the message
            // must name the host and the exact variable to set on it.
            let rendered = error.to_string();
            assert!(rendered.contains("control-plane"), "{rendered}");
            assert!(
                rendered.contains("TEMPS_DOCKER_SOCKET_PROJECTS=node-daemon"),
                "{rendered}"
            );
            assert!(rendered.contains("temps agent"), "{rendered}");
        }
        other => panic!("expected DockerSocketNotMounted, got {other:?}"),
    }
}

/// ADR 045: once a deployment's container has ever mounted the host
/// Docker socket, that fact must land on the `deployments` row so exec
/// authorization can check *this deployment's* history instead of the
/// project's current slug -- a project renamed away from a granted slug
/// keeps its already-running containers, and only the persisted flag
/// keeps exec on them admin-only.
#[tokio::test]
async fn persist_docker_socket_mounted_updates_the_deployment_row() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection(),
    );
    let job = DeployImageJobBuilder::new(
        "node-daemon",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("node-daemon".to_string())
    .failed_container_retention(db.clone(), 77)
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .expect("valid deploy job");
    let context = WorkflowContext::new("run-77".to_string(), 77, 3, 4, Arc::new(TestLogWriter));

    job.persist_docker_socket_mounted(&context).await;

    drop(job);
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
    let transaction_log = db.into_transaction_log();
    let rendered = transaction_log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .filter(|statement| statement.sql.contains("UPDATE \"deployments\""))
        .map(|statement| format!("{statement:?}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("docker_socket_mounted"),
        "expected an UPDATE on docker_socket_mounted, got: {rendered}"
    );
    assert!(
        rendered.contains('7'),
        "expected deployment id 77: {rendered}"
    );
}

/// A job built with no database handle (never happens in production,
/// since `failed_container_retention` is called unconditionally on every
/// builder chain that reaches a real deploy) must not panic -- the mount
/// is still logged and audited, just not persisted, per the doc comment
/// on `persist_docker_socket_mounted`.
#[tokio::test]
async fn persist_docker_socket_mounted_is_a_no_op_without_a_wired_database() {
    let job = DeployImageJobBuilder::new(
        "node-daemon",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("node-daemon".to_string())
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .expect("valid deploy job");
    let context = WorkflowContext::new("run-1".to_string(), 1, 1, 1, Arc::new(TestLogWriter));

    job.persist_docker_socket_mounted(&context).await;
}

/// ADR 045, the structural half of the deploy rule: this builder is the
/// one point every image deployment is forced through, so a route that
/// never learned the rule — the three remote-deployment handlers that
/// originally bypassed the gated service methods, or any future fourth —
/// still cannot produce a job that would start a host-root container.
#[test]
fn a_project_writer_cannot_build_a_deploy_job_for_a_declared_project() {
    let grant = temps_core::docker_socket_grant::DockerSocketGrant::parse(Some("node-daemon"));
    let error = refuse_granted_project_deploy(
        &grant,
        Some("node-daemon"),
        temps_core::docker_socket_grant::DeployCaller::ProjectWriter,
    )
    .expect_err("a project writer must not be able to build this job");
    match error {
        WorkflowError::DockerSocketDeployRequiresAdmin { ref project_slug } => {
            assert_eq!(project_slug, "node-daemon");
            let rendered = error.to_string();
            assert!(rendered.contains("ADR 045"), "{rendered}");
            assert!(
                rendered.contains(temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV),
                "{rendered}"
            );
        }
        other => panic!("expected DockerSocketDeployRequiresAdmin, got {other:?}"),
    }
}

/// The three callers that are allowed, and the one project shape that is
/// never affected. `Platform` covers node drain and failover, which
/// redeploy the workload that is already there and would otherwise leave a
/// granted infrastructure service down after its host died.
#[test]
fn admins_the_platform_and_undeclared_projects_build_normally() {
    use temps_core::docker_socket_grant::{DeployCaller, DockerSocketGrant};
    let grant = DockerSocketGrant::parse(Some("node-daemon"));
    for caller in [DeployCaller::InstanceAdmin, DeployCaller::Platform] {
        assert!(
            refuse_granted_project_deploy(&grant, Some("node-daemon"), caller).is_ok(),
            "{caller:?} may deploy a declared project"
        );
    }
    for caller in [
        DeployCaller::ProjectWriter,
        DeployCaller::InstanceAdmin,
        DeployCaller::Platform,
    ] {
        assert!(
            refuse_granted_project_deploy(&grant, Some("my-app"), caller).is_ok(),
            "{caller:?} deploying an undeclared project is untouched by ADR 045"
        );
        assert!(
            refuse_granted_project_deploy(&DockerSocketGrant::default(), Some("my-app"), caller)
                .is_ok(),
            "an install that never set the variable is untouched by ADR 045"
        );
    }
}

#[test]
fn test_health_check_path_override_default_is_none() {
    // By default there is no deploy-time override; only the standard
    // health_check_path ("/") is set.
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("d".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap();

    assert_eq!(job.config.health_check_path_override, None);
    assert_eq!(job.config.health_check_path, Some("/".to_string()));
}

#[test]
fn test_health_check_path_override_flows_to_config() {
    // An explicit deploy-time override is captured separately so it can win
    // over .temps.yaml at execution time.
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("d".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .health_check_path_override(Some("/api/healthz".to_string()))
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap();

    assert_eq!(
        job.config.health_check_path_override,
        Some("/api/healthz".to_string())
    );
    // The default standard path is left untouched; precedence is resolved at
    // execution time (override > .temps.yaml > default).
    assert_eq!(job.config.health_check_path, Some("/".to_string()));
}

/// Regression test for https://github.com/gotempsh/temps/issues/879:
/// an explicit environment/project port override must win over image
/// EXPOSE auto-detection, not the other way around. The image tag here
/// doesn't exist, so any attempt to consult it would fail; the override
/// must be returned without ever needing a successful inspection.
#[tokio::test]
async fn test_resolve_container_port_prefers_explicit_override_over_image_detection() {
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .port(3000)
    .configured_port(Some(9090))
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap();

    let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);

    let port = job
        .resolve_container_port("temps-test-nonexistent-image:latest", &context)
        .await;

    assert_eq!(port, Some(9090));
}

/// When neither environment nor project configures a port, image
/// inspection is attempted; if it fails (as it always will here, since
/// the image tag doesn't exist), resolution falls back to the
/// configured/default port.
#[tokio::test]
async fn test_resolve_container_port_falls_back_to_default_without_override() {
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .port(4000)
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap();

    assert_eq!(job.config.configured_port, None);

    let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);

    let port = job
        .resolve_container_port("temps-test-nonexistent-image:latest", &context)
        .await;

    assert_eq!(port, Some(4000));
}

fn port_job(configured_port: Option<u16>) -> DeployImageJob {
    DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .port(4000)
    .configured_port(configured_port)
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap()
}

fn context_with_built_config(exposed_ports: Vec<u16>) -> crate::workflow::WorkflowContext {
    let mut context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);
    context
        .set_output(
            "build_image",
            "image_config",
            super::super::build_image::BuiltImageConfig {
                exposed_ports,
                working_dir: Some("/app".to_string()),
                user: None,
                entrypoint: Vec::new(),
            },
        )
        .unwrap();
    context
}

/// A worker build records the image's `EXPOSE` ports from the node that
/// owns the image, so the port resolves without any local copy of it.
#[tokio::test]
async fn test_resolve_container_port_uses_the_config_recorded_by_the_build_node() {
    let job = port_job(None);
    let context = context_with_built_config(vec![8080, 9000]);

    let port = job
        .resolve_container_port("temps-test-nonexistent-image:latest", &context)
        .await;

    assert_eq!(port, Some(8080));
}

/// A node that reports no EXPOSE declares a portless worker, without
/// needing a local copy of the image to inspect.
#[tokio::test]
async fn test_resolve_container_port_is_portless_when_node_reports_no_expose() {
    let job = port_job(None);
    let context = context_with_built_config(Vec::new());

    let port = job
        .resolve_container_port("temps-test-nonexistent-image:latest", &context)
        .await;

    assert_eq!(port, None);
}

/// The environment/project override still outranks what the image says.
#[tokio::test]
async fn test_resolve_container_port_override_wins_over_recorded_config() {
    let job = port_job(Some(9090));
    let context = context_with_built_config(vec![8080]);

    let port = job
        .resolve_container_port("temps-test-nonexistent-image:latest", &context)
        .await;

    assert_eq!(port, Some(9090));
}

/// A rollback or promotion has no workflow context from the original
/// build; the ports its node reported travel with the reused worker
/// build, so the container port is still the image's own.
#[tokio::test]
async fn test_resolve_container_port_uses_the_config_of_a_reused_worker_build() {
    let reused = ReusedWorkerBuild {
        image_config: Some(super::super::build_image::BuiltImageConfig {
            exposed_ports: vec![8080],
            working_dir: None,
            user: None,
            entrypoint: Vec::new(),
        }),
        ..Default::default()
    };
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("external-image".to_string())
    .external_image_tag("app:1".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .port(3000)
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .unwrap()
    .with_reused_worker_build(reused);
    let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);

    let port = job.resolve_container_port("app:1", &context).await;

    assert_eq!(port, Some(8080));
}

#[test]
fn port_env_follows_detected_container_port_without_configured_port() {
    let mut env = HashMap::from([("PORT".to_string(), "3000".to_string())]);

    let previous = align_port_env_with_container_port(&mut env, None, 8080);

    assert_eq!(previous.as_deref(), Some("3000"));
    assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));
}

#[test]
fn port_env_is_left_alone_when_a_port_is_configured() {
    let mut env = HashMap::from([("PORT".to_string(), "9090".to_string())]);

    let previous = align_port_env_with_container_port(&mut env, Some(9090), 9090);

    assert_eq!(previous, None);
    assert_eq!(env.get("PORT").map(String::as_str), Some("9090"));
}

#[test]
fn port_env_is_not_added_when_the_planner_set_none() {
    let mut env = HashMap::new();

    let previous = align_port_env_with_container_port(&mut env, None, 8080);

    assert_eq!(previous, None);
    assert!(!env.contains_key("PORT"));
}

#[test]
fn port_env_already_matching_reports_no_change() {
    let mut env = HashMap::from([("PORT".to_string(), "3000".to_string())]);

    let previous = align_port_env_with_container_port(&mut env, None, 3000);

    assert_eq!(previous, None);
    assert_eq!(env.get("PORT").map(String::as_str), Some("3000"));
}

/// End to end through the real replica deploy: with no configured port,
/// the planner hands the job `PORT=3000`, the image declares `EXPOSE
/// 8080`, and the container must be started with `PORT` matching the
/// port it is routed to. The deployer is a recording mock, so nothing is
/// started; Docker is only used to build and inspect a throwaway image.
#[tokio::test]
async fn deployed_container_port_env_matches_image_expose_port() {
    use bollard::query_parameters::{BuildImageOptionsBuilder, RemoveImageOptions};
    use futures_util::StreamExt as _;

    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available, skipping");
            return;
        }
    };

    // `FROM scratch` needs no pull, and nothing ever runs this image.
    let image = "temps-port-env-test:expose-8080";
    let dockerfile = "FROM scratch\nEXPOSE 8080\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    let mut builder = tar::Builder::new(Vec::new());
    let context = match builder
        .append_data(&mut header, "Dockerfile", dockerfile.as_bytes())
        .and_then(|()| builder.into_inner())
    {
        Ok(context) => context,
        Err(e) => {
            println!("Could not build the image context ({e}), skipping");
            return;
        }
    };
    let mut build = docker.build_image(
        BuildImageOptionsBuilder::default().t(image).build(),
        None,
        Some(http_body_util::Either::Left(http_body_util::Full::new(
            bytes::Bytes::from(context),
        ))),
    );
    while let Some(step) = build.next().await {
        if let Err(e) = step {
            println!("Could not build the test image ({e}), skipping");
            return;
        }
    }

    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .port(3000)
    .configured_port(None)
    .environment_variables(HashMap::from([("PORT".to_string(), "3000".to_string())]))
    .health_check_path(None)
    .build(deployer.clone())
    .unwrap();
    let context = crate::test_utils::create_test_context("run-1".to_string(), 1, 1, 1);
    let dyn_deployer: Arc<dyn ContainerDeployer> = deployer.clone();

    let result = job
        .deploy_single_replica(
            image,
            &context,
            0,
            None,
            &dyn_deployer,
            ReplicaTarget {
                assignment: &crate::services::NodeAssignment::Local,
                docker_socket_required: false,
            },
        )
        .await;

    let request = deployer.requests.lock().unwrap().first().cloned();
    let _ = docker
        .remove_image(
            image,
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await;

    assert!(result.is_ok(), "replica deploy failed: {result:?}");
    let request = request.expect("the deployer should have been called");
    assert_eq!(
        request.port_mappings.first().map(|p| p.container_port),
        Some(8080)
    );
    assert_eq!(
        request.environment_vars.get("PORT").map(String::as_str),
        Some("8080"),
        "PORT must match the container port traffic is routed to"
    );
}

#[tokio::test]
async fn dockerfile_runtime_overrides_reach_each_container_request() {
    let deployer = Arc::new(TrackingMockContainerDeployer::new());
    let command = Some(vec![
        "python".to_string(),
        "-m".to_string(),
        "worker".to_string(),
        "argument with spaces".to_string(),
    ]);
    for entrypoint in [None, Some(vec![]), Some(vec!["/app/start".to_string()])] {
        let job = DeployImageJobBuilder::new(
            "test-project",
            temps_core::docker_socket_grant::DeployCaller::Platform,
        )
        .job_id("deploy".into())
        .build_job_id("build".into())
        .target(DeploymentTarget::Docker {
            registry_url: "local".into(),
            network: None,
        })
        .service_name("worker".into())
        .configured_port(Some(3000))
        .health_check_path(None)
        .command(command.clone())
        .entrypoint(entrypoint.clone())
        .build(deployer.clone())
        .unwrap();
        let context = crate::test_utils::create_test_context("runtime-overrides".into(), 1, 1, 1);
        let target: Arc<dyn ContainerDeployer> = deployer.clone();
        job.deploy_single_replica(
            "example/worker:test",
            &context,
            0,
            None,
            &target,
            ReplicaTarget {
                assignment: &crate::services::NodeAssignment::Local,
                docker_socket_required: false,
            },
        )
        .await
        .unwrap();
        let requests = deployer.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert_eq!(request.command, command);
        assert_eq!(request.entrypoint, entrypoint);
        // The same request is serialized directly to the worker agent.
        let roundtrip: DeployRequest =
            serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
        assert_eq!(roundtrip.entrypoint, entrypoint);
        assert_eq!(roundtrip.command, command);
    }
}

#[tokio::test]
async fn test_multi_replica_deployment() {
    // This test verifies that DeployImageJob is configured to deploy multiple replicas
    // and that the configuration flows correctly through the system.
    //
    // Note: Full end-to-end execution is tested in integration tests since it requires
    // actual containers and health checks.

    let mock_deployer = Arc::new(TrackingMockContainerDeployer::new());
    let container_deployer: Arc<dyn ContainerDeployer> = mock_deployer.clone();

    let target = DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: Some(temps_core::NETWORK_NAME.to_string()),
    };

    // Create job with 2 replicas
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test_deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(target)
    .service_name("myapp".to_string())
    .namespace("production".to_string())
    .replicas(2) // Deploy 2 replicas
    .port(3000)
    .build(container_deployer)
    .unwrap();

    // Verify job configuration
    assert_eq!(
        job.config.replicas, 2,
        "Job should be configured for 2 replicas"
    );
    assert_eq!(job.config.service_name, "myapp");
    assert_eq!(job.config.port, 3000);

    // Verify container naming for multi-replica deployment
    // Replica 1 should be named "myapp-1", replica 2 should be "myapp-2"
    // This is tested implicitly through the container deployment flow
}

#[test]
fn test_image_output_from_context() {
    let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

    // Set up outputs as the build job would
    context
        .set_output("build_image", "image_tag", "myapp:latest")
        .unwrap();
    context
        .set_output("build_image", "image_id", "sha256:abc123")
        .unwrap();
    context
        .set_output("build_image", "size_bytes", 104857600u64)
        .unwrap(); // 100MB
    context
        .set_output("build_image", "build_context", "/tmp/repo")
        .unwrap();
    context
        .set_output("build_image", "dockerfile_path", "/tmp/repo/Dockerfile")
        .unwrap();

    let image_output = BuildImageOutput::from_context(&context, "build_image").unwrap();
    assert_eq!(image_output.image_tag, "myapp:latest");
    assert_eq!(image_output.image_id, "sha256:abc123");
    assert_eq!(image_output.size_bytes, 104857600);
    assert_eq!(image_output.build_context, PathBuf::from("/tmp/repo"));
    assert_eq!(
        image_output.dockerfile_path,
        PathBuf::from("/tmp/repo/Dockerfile")
    );
}

#[tokio::test]
async fn test_deployment_config_validation() {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let target = DeploymentTarget::Docker {
        registry_url: "docker.io".to_string(),
        network: None,
    };

    let job = DeployImageJob::new(
        "test".to_string(),
        "build_job".to_string(),
        target,
        container_deployer,
    );

    let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
    assert!(job.validate_deployment_config(&context).await.is_ok());
}

#[test]
fn test_deploy_image_job_builder_with_node_scheduler() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<nodes::Model>::new()])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test_deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("myapp".to_string())
    .namespace("default".to_string())
    .replicas(2)
    .node_scheduler(scheduler)
    .target_nodes(vec![1, 3])
    .build(container_deployer)
    .unwrap();

    assert!(job.node_scheduler.is_some(), "Node scheduler should be set");
    assert_eq!(
        job.config.target_nodes,
        Some(vec![1, 3]),
        "Target nodes should be set"
    );
    assert_eq!(job.config.replicas, 2);
}

#[test]
fn test_deploy_image_job_builder_without_node_scheduler() {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test_deploy".to_string())
    .build_job_id("build_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("myapp".to_string())
    .namespace("default".to_string())
    .replicas(3)
    .build(container_deployer)
    .unwrap();

    assert!(
        job.node_scheduler.is_none(),
        "Node scheduler should not be set when not provided"
    );
    assert_eq!(
        job.config.target_nodes, None,
        "Target nodes should be None by default"
    );
}

#[test]
fn test_deployment_job_config_target_nodes_default() {
    let config = DeploymentJobConfig::default();
    assert_eq!(config.target_nodes, None);
}

#[test]
fn explicit_placement_constraints_are_fail_closed_for_every_scheduler_error() {
    assert!(!has_explicit_placement_constraints(None, None));
    assert!(!has_explicit_placement_constraints(
        None,
        Some(&serde_json::json!({}))
    ));
    // Empty selectors of either kind name nothing, so they constrain
    // nothing — the two must agree, and an empty label object has always
    // read that way.
    assert!(!has_explicit_placement_constraints(Some(&[]), None));
    assert!(has_explicit_placement_constraints(Some(&[1]), None));
    assert!(has_explicit_placement_constraints(
        None,
        Some(&serde_json::json!({"region": "eu"}))
    ));
    assert!(has_explicit_placement_constraints(
        None,
        Some(&serde_json::json!(["malformed"]))
    ));
}

/// Test that node scheduling produces correct assignments when integrated with DeployImageJob.
/// We test the scheduling logic directly (not the full deploy flow which needs real containers).
#[tokio::test]
async fn test_node_scheduling_no_scheduler_returns_local_assignments() {
    // Without a node_scheduler, the deploy_image method creates Local assignments
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("test".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("myapp".to_string())
    .namespace("default".to_string())
    .replicas(3)
    .build(container_deployer)
    .unwrap();

    // Verify no scheduler is set
    assert!(job.node_scheduler.is_none());
    // The deploy_image method will create vec![Local; 3] internally
}

/// Test that scheduler with no active nodes produces local assignments
#[tokio::test]
async fn test_node_scheduling_empty_nodes_returns_local() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<nodes::Model>::new()])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    // Schedule 3 replicas with no active nodes
    let assignments = scheduler
        .schedule_replicas(3, None, None, false)
        .await
        .unwrap();
    assert_eq!(assignments.len(), 3);
    for a in &assignments {
        assert!(
            a.is_local(),
            "All assignments should be Local when no active nodes"
        );
    }
}

/// Test that scheduler distributes replicas across active nodes via round-robin
#[tokio::test]
async fn test_node_scheduling_round_robin_across_nodes() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    fn make_node(id: i32, name: &str) -> nodes::Model {
        nodes::Model {
            architecture: None,
            id,
            name: name.to_string(),
            token_hash: format!("hash_{}", id),
            token_encrypted: None,
            address: format!("https://10.0.0.{}:3100", id),
            private_address: format!("10.0.0.{}", id),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            failover_at: None,
            drain_requested_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            overlay_vxlan_vni: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![
            make_node(1, "worker-a"),
            make_node(2, "worker-b"),
        ]])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    let assignments = scheduler
        .schedule_replicas(4, None, None, false)
        .await
        .unwrap();
    assert_eq!(assignments.len(), 4);

    // Pool is [Local, worker-a, worker-b] → round-robin: Local, A(1), B(2), Local
    assert!(assignments[0].is_local(), "First replica should be Local");
    assert_eq!(
        assignments[1].node_id(),
        Some(1),
        "Second should be worker-a"
    );
    assert_eq!(
        assignments[2].node_id(),
        Some(2),
        "Third should be worker-b"
    );
    assert!(
        assignments[3].is_local(),
        "Fourth should wrap back to Local"
    );
}

/// Test that target_nodes filters to only specified nodes
#[tokio::test]
async fn test_node_scheduling_with_target_nodes_filter() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    fn make_node(id: i32, name: &str) -> nodes::Model {
        nodes::Model {
            architecture: None,
            id,
            name: name.to_string(),
            token_hash: format!("hash_{}", id),
            token_encrypted: None,
            address: format!("https://10.0.0.{}:3100", id),
            private_address: format!("10.0.0.{}", id),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(chrono::Utc::now()),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            failover_at: None,
            drain_requested_at: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            overlay_vxlan_vni: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![
            make_node(1, "worker-a"),
            make_node(2, "worker-b"),
            make_node(3, "worker-c"),
        ]])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    // Target only nodes 1 and 3
    let target_ids = vec![1, 3];
    let assignments = scheduler
        .schedule_replicas(4, None, Some(&target_ids), false)
        .await
        .unwrap();
    assert_eq!(assignments.len(), 4);

    // Explicit targets restrict the pool to worker-a(1) and worker-c(3).
    for a in &assignments {
        match a {
            crate::services::NodeAssignment::Remote { node_id, .. } => {
                assert!(
                    *node_id == 1 || *node_id == 3,
                    "Should only schedule on target nodes, got {}",
                    node_id
                );
            }
            crate::services::NodeAssignment::Local => {
                panic!("explicit target nodes must exclude the control plane")
            }
        }
    }
}

/// Explicit target constraints must never silently fall back to local.
#[tokio::test]
async fn test_node_scheduling_target_nodes_no_match_fails_closed() {
    use crate::services::{NodeError, NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let node = nodes::Model {
        architecture: None,
        id: 1,
        name: "worker-1".to_string(),
        token_hash: "hash".to_string(),
        token_encrypted: None,
        address: "https://10.0.0.1:3100".to_string(),
        private_address: "10.0.0.1".to_string(),
        public_endpoint: None,
        wg_public_key: None,
        role: "worker".to_string(),
        status: "active".to_string(),
        labels: serde_json::json!({}),
        capacity: serde_json::json!({}),
        last_heartbeat: Some(chrono::Utc::now()),
        edge_public_key: None,
        compute_cidr: None,
        underlay_address: None,
        mesh_wg_public_key: None,
        mesh_wg_endpoint: None,
        mesh_wg_address: None,
        failover_at: None,
        drain_requested_at: None,
        dns_resolver_running: None,
        dns_resolver_tasks_alive: None,
        dns_resolver_last_sync_at: None,
        dns_resolver_consecutive_failures: 0,
        dns_resolver_last_error: None,
        dns_resolver_record_count: None,
        deployments_without_resolver: None,
        build_limits: None,
        public_ingress_enabled: false,
        public_ingress_running: None,
        public_ingress_last_error: None,
        public_ingress_certificate_count: None,
        public_ingress_route_count: None,
        public_ingress_unsupported_route_count: None,
        public_ingress_unsupported_reasons: serde_json::json!([]),
        occupied_ranges: None,
        overlay_status: None,
        overlay_last_error: None,
        overlay_ready_cidr: None,
        overlay_remediation: None,
        overlay_network_name: None,
        overlay_vxlan_vni: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    // `list_active`, then `list_all` to say why node 99 is not usable.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![node.clone()], vec![node]])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    // Target node 99 doesn't exist
    let target_ids = vec![99];
    let error = scheduler
        .schedule_replicas(2, None, Some(&target_ids), false)
        .await
        .expect_err("an unmatched explicit target must not run on the control plane");
    match error {
        NodeError::PlacementConstraintsUnsatisfied { excluded } => {
            assert!(
                excluded.contains("'node 99' is listed in target_nodes but does not exist"),
                "unexpected placement diagnostic: {excluded}"
            );
        }
        other => {
            panic!("expected PlacementConstraintsUnsatisfied, got {other:?}");
        }
    }
}

/// Test that DeployImageJob correctly passes target_nodes to scheduler
#[tokio::test]
async fn test_deploy_image_job_target_nodes_config_flows_to_scheduler() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<nodes::Model>::new()])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".to_string())
    .build_job_id("build".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .service_name("app".to_string())
    .namespace("default".to_string())
    .replicas(2)
    .node_scheduler(scheduler)
    .target_nodes(vec![5, 10])
    .build(container_deployer)
    .unwrap();

    // Verify the config was set correctly
    assert_eq!(job.config.target_nodes, Some(vec![5, 10]));
    assert!(job.node_scheduler.is_some());

    // The target_nodes will be passed to scheduler.schedule_replicas
    // via self.config.target_nodes.as_deref() in deploy_image()
}

/// Test NodeAssignment accessor methods
#[test]
fn test_node_assignment_private_address() {
    use crate::services::NodeAssignment;

    let local = NodeAssignment::Local;
    assert!(local.private_address().is_none());

    let remote = NodeAssignment::Remote {
        platform: None,
        node_id: 1,
        node_name: "w1".to_string(),
        address: "https://10.0.0.1:3100".to_string(),
        private_address: "10.0.0.1".to_string(),
    };
    assert_eq!(remote.private_address(), Some("10.0.0.1"));
    assert_eq!(remote.node_id(), Some(1));
    assert!(!remote.is_local());
}

/// Test get_node_token returns error for Local assignment
#[tokio::test]
async fn test_get_node_token_local_returns_error() {
    use crate::services::NodeAssignment;

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJob::new(
        "test".to_string(),
        "build".to_string(),
        DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        },
        container_deployer,
    );

    let result = job.get_node_token(&NodeAssignment::Local).await;
    assert!(result.is_err(), "Should error for local assignment");
}

/// Test get_node_token returns error when no scheduler is set
#[tokio::test]
async fn test_get_node_token_no_scheduler_returns_error() {
    use crate::services::NodeAssignment;

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let job = DeployImageJob::new(
        "test".to_string(),
        "build".to_string(),
        DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        },
        container_deployer,
    );

    let result = job
        .get_node_token(&NodeAssignment::Remote {
            platform: None,
            node_id: 1,
            node_name: "worker-1".to_string(),
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
        })
        .await;
    assert!(result.is_err(), "Should error when no scheduler available");
}

/// Test get_node_token decrypts the encrypted token from node service
#[tokio::test]
async fn test_get_node_token_success() {
    use crate::services::{NodeAssignment, NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let enc_service =
        Arc::new(temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap());
    let plaintext_token = "my-secret-agent-token";
    let encrypted = enc_service.encrypt(plaintext_token.as_bytes()).unwrap();

    let node = nodes::Model {
        architecture: None,
        id: 1,
        name: "worker-1".to_string(),
        token_hash: "hash".to_string(),
        token_encrypted: Some(encrypted),
        address: "https://10.0.0.1:3100".to_string(),
        private_address: "10.0.0.1".to_string(),
        public_endpoint: None,
        wg_public_key: None,
        role: "worker".to_string(),
        status: "active".to_string(),
        labels: serde_json::json!({}),
        capacity: serde_json::json!({}),
        last_heartbeat: Some(chrono::Utc::now()),
        edge_public_key: None,
        compute_cidr: None,
        underlay_address: None,
        mesh_wg_public_key: None,
        mesh_wg_endpoint: None,
        mesh_wg_address: None,
        failover_at: None,
        drain_requested_at: None,
        dns_resolver_running: None,
        dns_resolver_tasks_alive: None,
        dns_resolver_last_sync_at: None,
        dns_resolver_consecutive_failures: 0,
        dns_resolver_last_error: None,
        dns_resolver_record_count: None,
        deployments_without_resolver: None,
        build_limits: None,
        public_ingress_enabled: false,
        public_ingress_running: None,
        public_ingress_last_error: None,
        public_ingress_certificate_count: None,
        public_ingress_route_count: None,
        public_ingress_unsupported_route_count: None,
        public_ingress_unsupported_reasons: serde_json::json!([]),
        occupied_ranges: None,
        overlay_status: None,
        overlay_last_error: None,
        overlay_ready_cidr: None,
        overlay_remediation: None,
        overlay_network_name: None,
        overlay_vxlan_vni: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![node]])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let mut job = DeployImageJob::new(
        "test".to_string(),
        "build".to_string(),
        DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        },
        container_deployer,
    );
    job.node_scheduler = Some(scheduler);
    job.encryption_service = Some(enc_service);

    let result = job
        .get_node_token(&NodeAssignment::Remote {
            platform: None,
            node_id: 1,
            node_name: "worker-1".to_string(),
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
        })
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), plaintext_token);
}

/// Test get_node_token fails when no encrypted token is stored
#[tokio::test]
async fn test_get_node_token_no_encrypted_token() {
    use crate::services::{NodeAssignment, NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::nodes;

    let node = nodes::Model {
        architecture: None,
        id: 1,
        name: "worker-1".to_string(),
        token_hash: "hash".to_string(),
        token_encrypted: None,
        address: "https://10.0.0.1:3100".to_string(),
        private_address: "10.0.0.1".to_string(),
        public_endpoint: None,
        wg_public_key: None,
        role: "worker".to_string(),
        status: "active".to_string(),
        labels: serde_json::json!({}),
        capacity: serde_json::json!({}),
        last_heartbeat: Some(chrono::Utc::now()),
        edge_public_key: None,
        compute_cidr: None,
        underlay_address: None,
        mesh_wg_public_key: None,
        mesh_wg_endpoint: None,
        mesh_wg_address: None,
        failover_at: None,
        drain_requested_at: None,
        dns_resolver_running: None,
        dns_resolver_tasks_alive: None,
        dns_resolver_last_sync_at: None,
        dns_resolver_consecutive_failures: 0,
        dns_resolver_last_error: None,
        dns_resolver_record_count: None,
        deployments_without_resolver: None,
        build_limits: None,
        public_ingress_enabled: false,
        public_ingress_running: None,
        public_ingress_last_error: None,
        public_ingress_certificate_count: None,
        public_ingress_route_count: None,
        public_ingress_unsupported_route_count: None,
        public_ingress_unsupported_reasons: serde_json::json!([]),
        occupied_ranges: None,
        overlay_status: None,
        overlay_last_error: None,
        overlay_ready_cidr: None,
        overlay_remediation: None,
        overlay_network_name: None,
        overlay_vxlan_vni: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![node]])
        .into_connection();
    let node_service = Arc::new(NodeService::new(Arc::new(db)));
    let scheduler = Arc::new(NodeScheduler::new(node_service));

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());

    let enc_service =
        Arc::new(temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap());

    let mut job = DeployImageJob::new(
        "test".to_string(),
        "build".to_string(),
        DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        },
        container_deployer,
    );
    job.node_scheduler = Some(scheduler);
    job.encryption_service = Some(enc_service);

    let result = job
        .get_node_token(&NodeAssignment::Remote {
            platform: None,
            node_id: 1,
            node_name: "worker-1".to_string(),
            address: "https://10.0.0.1:3100".to_string(),
            private_address: "10.0.0.1".to_string(),
        })
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no encrypted token"),
        "Error should mention missing token: {}",
        err
    );
}

/// An [`ImageBuilder`] that records whether `save_image` was invoked and,
/// when it is, writes a real (tiny) file to the requested path so the
/// subsequent `RemoteNodeDeployer::import_image` file-open succeeds —
/// exercising the actual save+stream path end to end rather than
/// stubbing it out.
struct RecordingImageBuilder {
    save_image_called: Arc<AtomicBool>,
    /// Whether the control plane's Docker holds the image. When `true`,
    /// `inspect_image` reports it as `linux/amd64`; tests pair that with a
    /// `linux/amd64` remote so the platform check needs no agent call.
    has_image: bool,
}

#[async_trait]
impl temps_deployer::ImageBuilder for RecordingImageBuilder {
    async fn build_image(
        &self,
        _request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn build_image_with_callback(
        &self,
        _request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn import_image(
        &self,
        _image_path: PathBuf,
        _tag: &str,
    ) -> Result<String, temps_deployer::BuilderError> {
        unimplemented!("not used — the control plane never imports into itself")
    }

    async fn save_image(
        &self,
        image_name: &str,
        output_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        self.save_image_called.store(true, Ordering::SeqCst);
        // A real (tiny) `docker save` archive in which the saved tag names
        // the image this builder reports (`sha256:local`), OCI layout.
        write_saved_image_archive(output_path, image_name, "blobs/sha256/local")
            .map_err(temps_deployer::BuilderError::IoError)?;
        Ok(())
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn inspect_image(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
        if self.has_image {
            return Ok(temps_deployer::ImageInfo {
                id: "sha256:local".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 0,
                tags: vec![image_name.to_string()],
                created: None,
                working_dir: None,
                config: None,
            });
        }
        // Reported as not found so `verify_image_platform_for_node` takes
        // its graceful skip path — platform matching isn't what this test
        // is proving.
        Err(temps_deployer::BuilderError::ImageNotFound(
            image_name.to_string(),
        ))
    }

    fn get_native_platform(&self) -> String {
        "linux/amd64".to_string()
    }
}

/// Write a tar archive with the given `(path, contents)` entries.
fn write_archive(path: &std::path::Path, entries: &[(&str, Vec<u8>)]) -> std::io::Result<()> {
    let mut builder = tar::Builder::new(std::fs::File::create(path)?);
    for (name, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, contents.as_slice())?;
    }
    builder.finish()
}

/// `manifest.json` with one entry per `(Config, RepoTags)`.
fn manifest_json(entries: &[(&str, &[&str])]) -> Vec<u8> {
    serde_json::Value::Array(
        entries
            .iter()
            .map(|(config, repo_tags)| {
                serde_json::json!({
                    "Config": config,
                    "RepoTags": repo_tags,
                    "Layers": [],
                })
            })
            .collect(),
    )
    .to_string()
    .into_bytes()
}

/// OCI `index.json` with one descriptor per `(digest, annotations)`.
fn oci_index_json(descriptors: &[(&str, &[(&str, &str)])]) -> Vec<u8> {
    serde_json::json!({
        "schemaVersion": 2,
        "manifests": descriptors
            .iter()
            .map(|(digest, annotations)| serde_json::json!({
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "digest": digest,
                "size": 1,
                "annotations": annotations
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), serde_json::Value::from(*v)))
                    .collect::<serde_json::Map<_, _>>(),
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
    .into_bytes()
}

/// Write a minimal `docker save` archive whose `manifest.json` tags one
/// image (`config`) with `tag`.
fn write_saved_image_archive(
    path: &std::path::Path,
    tag: &str,
    config: &str,
) -> std::io::Result<()> {
    write_archive(
        path,
        &[("manifest.json", manifest_json(&[(config, &[tag])]))],
    )
}

fn job_with_target(container_deployer: Arc<dyn ContainerDeployer>) -> DeployImageJob {
    DeployImageJob::new(
        "deploy".to_string(),
        "build".to_string(),
        DeploymentTarget::Docker {
            registry_url: "local".to_string(),
            network: None,
        },
        container_deployer,
    )
}

/// Spawns a bare-bones HTTP/1.1 agent that answers exactly `expected_requests`
/// requests in sequence, routing each by a substring match against the
/// request line/headers. `route_body` receives the raw request text and
/// returns `(status_line, json_body)`; panicking inside it (e.g. on an
/// unexpected path) fails the test with a clear message instead of hanging.
async fn spawn_sequenced_agent(
    expected_requests: usize,
    route_body: impl Fn(&str) -> (&'static str, String) + Send + 'static,
) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock agent");
    let address = listener.local_addr().expect("mock agent address");

    tokio::spawn(async move {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.expect("read request");
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            let (status_line, body) = route_body(&request_text);
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });

    format!("http://{address}")
}

/// Registry-sourced deploys (`external_image_tag` set) must ask the
/// worker to pull the image itself via `POST /agent/images/pull`, and
/// must never touch a control-plane image builder — proving the
/// control-plane serve profile (no CP Docker daemon) can still ship a
/// registry image to a worker.
#[tokio::test]
async fn ensure_image_on_remote_uses_registry_pull_for_external_image() {
    let agent_url = spawn_sequenced_agent(2, |request_text| {
        if request_text.contains("/exists") {
            ("200 OK", r#"{"success":true,"data":false}"#.to_string())
        } else if request_text.contains("/agent/images/pull") {
            (
                "200 OK",
                r#"{"success":true,"data":{"image_id":"sha256:pulled","digest":null}}"#.to_string(),
            )
        } else if request_text.contains("/agent/images/import") {
            panic!(
                "registry-sourced deploy must not call /agent/images/import (save+stream), \
                     got request: {request_text}"
            );
        } else {
            panic!("unexpected request to mock agent: {request_text}");
        }
    })
    .await;

    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            agent_url,
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap(),
    );

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer);
    job.external_image_tag = Some("ghcr.io/acme/app:v1".to_string());
    // Deliberately no `image_builder` — the control-plane serve profile
    // constructs none. If the pull path accidentally fell back to
    // save+stream, `image_builder.as_ref()` would be `None` and the job
    // would fail with "no image builder configured" instead of reaching
    // the mock agent's import endpoint (which would also panic above).

    let context = crate::test_utils::create_test_context("wf-registry".to_string(), 1, 1, 1);

    job.ensure_image_on_remote(
        "ghcr.io/acme/app:v1",
        &remote,
        "worker-1",
        &context,
        None,
        Some(1),
    )
    .await
    .expect("registry pull path should succeed with zero control-plane Docker involvement");
}

#[tokio::test]
async fn worker_image_same_node_rejects_actual_architecture_mismatch() {
    let url = spawn_sequenced_agent(1, |request| {
        assert!(request.contains("/agent/images/inspect?"));
        (
            "200 OK",
            serde_json::json!({
                "id": "sha256:test", "architecture": "arm64", "os": "linux",
                "platform": "linux/arm64", "size_bytes": 1, "tags": ["app:1"],
                "created": null, "working_dir": null
            })
            .to_string(),
        )
    })
    .await;
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(url, "token".into(), "worker".into())
            .unwrap()
            .with_platform(Some("linux/amd64".into())),
    );
    let job = job_with_image_builder(PlatformOnlyImageBuilder::confirmed(
        "linux/amd64",
        "linux/amd64",
    ));
    let context = crate::test_utils::create_test_context("wf".into(), 1, 1, 1);
    let error = job
        .ensure_image_on_remote("app:1", &remote, "worker", &context, Some(7), Some(7))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("linux/arm64"));
    assert!(error.to_string().contains("linux/amd64"));
}

#[tokio::test]
async fn worker_recorded_platform_drives_placement_without_control_plane_inspection() {
    let job = job_with_image_builder(PlatformOnlyImageBuilder {
        platform: "linux/amd64".into(),
        discovered: None,
        image_platform: None,
        discoverable: None,
        local_platforms: Vec::new(),
        registry_platforms: Vec::new(),
        registry_reads: Default::default(),
    });
    let mut output = build_output_with_tags(&[("linux/arm64", "myapp:latest")]);
    output.builder_node_id = Some(7);
    assert_eq!(
        job.available_image_platforms(&output).await,
        vec!["linux/arm64"]
    );
}

/// A control-plane-local build (no `external_image_tag`) must still use
/// the save+stream path — the only path available in the full profile,
/// where a CP Docker daemon actually built the image and nothing else
/// on the cluster has it yet.
#[tokio::test]
async fn ensure_image_on_remote_uses_save_and_stream_for_local_build() {
    let agent_url = spawn_sequenced_agent(2, |request_text| {
        if request_text.contains("/exists") {
            ("200 OK", r#"{"success":true,"data":false}"#.to_string())
        } else if request_text.contains("/agent/images/import") {
            (
                "200 OK",
                r#"{"success":true,"data":"sha256:imported"}"#.to_string(),
            )
        } else if request_text.contains("/agent/images/pull") {
            panic!(
                "a control-plane-local build must not call /agent/images/pull, \
                     got request: {request_text}"
            );
        } else {
            panic!("unexpected request to mock agent: {request_text}");
        }
    })
    .await;

    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            agent_url,
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        // The builder reports the image as present (linux/amd64); a known
        // node platform keeps the architecture check off the mock agent.
        .with_platform(Some("linux/amd64".to_string())),
    );

    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer);
    let save_image_called = Arc::new(AtomicBool::new(false));
    job.image_builder = Some(Arc::new(RecordingImageBuilder {
        save_image_called: save_image_called.clone(),
        has_image: true,
    }));
    // No external_image_tag: this is the BuildImageJob-driven path.

    let context = crate::test_utils::create_test_context("wf-local-build".to_string(), 1, 1, 1);

    job.ensure_image_on_remote("myapp:latest", &remote, "worker-1", &context, None, Some(1))
        .await
        .expect("save+stream path should succeed for a control-plane-local build");

    assert!(
        save_image_called.load(Ordering::SeqCst),
        "expected the local image builder's save_image to be called for a local build"
    );
}

const UPLOADED_IMAGE: &str = "temps.internal/project-1/environment-2/upload-0f3c9a:immutable";
const REGISTRY_IMAGE: &str = "ghcr.io/example-org/app:v1";

/// Agent endpoints hit, in order: `exists`, `pull`, `import`, or the raw
/// request line for anything else.
type AgentRequestLog = Arc<Mutex<Vec<String>>>;

fn agent_request_kind(request_text: &str) -> String {
    let path = request_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    if path.starts_with("/agent/images/") && path.ends_with("/exists") {
        "exists".to_string()
    } else if path.starts_with("/agent/images/pull") {
        if request_text.contains(r#""replace_cached":true"#) {
            "pull-replacing".to_string()
        } else {
            "pull".to_string()
        }
    } else if path.starts_with("/agent/images/import") {
        "import".to_string()
    } else if path.starts_with("/agent/images/inspect") {
        "inspect".to_string()
    } else {
        request_text.lines().next().unwrap_or_default().to_string()
    }
}

/// Mock agent that records every request it receives. The image is never
/// already on the node; `/pull` answers with `pull_response`; `/import`
/// succeeds. Unexpected requests are answered with an error and recorded,
/// so the test's sequence assertion reports them instead of a hang.
async fn spawn_recording_agent(
    pull_response: (&'static str, &'static str),
) -> (
    Arc<temps_deployer::remote::RemoteNodeDeployer>,
    AgentRequestLog,
) {
    spawn_recording_agent_resolving(pull_response, "linux/amd64").await
}

/// [`spawn_recording_agent`] whose node, after an import, resolves the
/// image as `imported_platform`.
async fn spawn_recording_agent_resolving(
    pull_response: (&'static str, &'static str),
    imported_platform: &'static str,
) -> (
    Arc<temps_deployer::remote::RemoteNodeDeployer>,
    AgentRequestLog,
) {
    spawn_recording_agent_holding(pull_response, false, vec![imported_platform]).await
}

/// [`spawn_recording_agent`] whose node answers `/exists` with
/// `image_exists` and resolves the image, on its n-th inspect, as
/// `inspected_platforms[n]` (the last entry repeats).
async fn spawn_recording_agent_holding(
    pull_response: (&'static str, &'static str),
    image_exists: bool,
    inspected_platforms: Vec<&'static str>,
) -> (
    Arc<temps_deployer::remote::RemoteNodeDeployer>,
    AgentRequestLog,
) {
    spawn_recording_agent_on(
        pull_response,
        image_exists,
        inspected_platforms,
        "linux/amd64",
    )
    .await
}

/// [`spawn_recording_agent_holding`] for a node that runs `node_platform`.
async fn spawn_recording_agent_on(
    pull_response: (&'static str, &'static str),
    image_exists: bool,
    inspected_platforms: Vec<&'static str>,
    node_platform: &'static str,
) -> (
    Arc<temps_deployer::remote::RemoteNodeDeployer>,
    AgentRequestLog,
) {
    let requests: AgentRequestLog = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let inspections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // More slots than any expected sequence needs, so an unexpected extra
    // request is still recorded rather than refused.
    let agent_url = spawn_sequenced_agent(6, move |request_text| {
        let kind = agent_request_kind(request_text);
        recorded
            .lock()
            .expect("agent request log")
            .push(kind.clone());
        match kind.as_str() {
            "exists" => (
                "200 OK",
                format!(r#"{{"success":true,"data":{image_exists}}}"#),
            ),
            "pull" | "pull-replacing" => (pull_response.0, pull_response.1.to_string()),
            "import" => (
                "200 OK",
                r#"{"success":true,"data":"sha256:imported"}"#.to_string(),
            ),
            "inspect" => {
                let nth = inspections.fetch_add(1, Ordering::SeqCst);
                let imported_platform = inspected_platforms
                    .get(nth)
                    .or(inspected_platforms.last())
                    .copied()
                    .unwrap_or("linux/amd64");
                let (os, architecture) = imported_platform
                    .split_once('/')
                    .unwrap_or(("linux", "amd64"));
                (
                    "200 OK",
                    serde_json::json!({
                        "id": "sha256:imported",
                        "architecture": architecture,
                        "os": os,
                        "platform": imported_platform,
                        "size_bytes": 1,
                        "tags": [],
                        "created": null,
                        "working_dir": null,
                    })
                    .to_string(),
                )
            }
            _ => (
                "404 Not Found",
                r#"{"success":false,"data":null,"error":"unexpected request"}"#.to_string(),
            ),
        }
    })
    .await;

    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            agent_url,
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        // Known platform, so the pre-transfer architecture check never
        // needs to ask the mock agent's health endpoint.
        .with_platform(Some(node_platform.to_string())),
    );
    (remote, requests)
}

const PULL_OK: (&str, &str) = (
    "200 OK",
    r#"{"success":true,"data":{"image_id":"sha256:pulled","digest":null}}"#,
);
const PULL_FAILS: (&str, &str) = (
    "500 Internal Server Error",
    r#"{"success":false,"data":null,"error":"manifest unknown"}"#,
);

fn assert_agent_requests(requests: &AgentRequestLog, expected: &[&str], why: &str) {
    let seen = requests.lock().expect("agent request log").clone();
    assert_eq!(seen, expected, "{why}");
}

fn uploaded_image_job(
    image_source: Option<DeployImageSource>,
    builder: Option<RecordingImageBuilder>,
) -> DeployImageJob {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job =
        job_with_target(container_deployer).with_external_image_tag(UPLOADED_IMAGE.to_string());
    if let Some(image_source) = image_source {
        job = job.with_image_source(image_source);
    }
    if let Some(builder) = builder {
        job.image_builder = Some(Arc::new(builder));
    }
    job
}

/// Regression: an uploaded image (`temps deploy:local-image`) is deployed
/// with a directly-handed tag, exactly like a registry image, but lives only
/// in the control plane's Docker. A remote replica must receive it via
/// save+stream; asking the worker to pull `temps.internal/...` failed with
/// "lookup temps.internal: no such host".
#[tokio::test]
async fn ensure_image_on_remote_imports_uploaded_image_instead_of_pulling() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: true,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-upload".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "import", "inspect"],
        "an uploaded image must be imported, never pulled from a registry",
    );
    result.expect("an uploaded image must be transferred to the worker via import");
    assert!(
        save_image_called.load(Ordering::SeqCst),
        "expected the uploaded image to be exported from the control plane"
    );
}

/// #1789: a multi-platform image passes the pre-transfer check when it has
/// the node's architecture; if the archive the worker imported did not
/// carry it, the deploy fails here with the reason instead of at start
/// with `exec format error`.
#[tokio::test]
async fn ensure_image_on_remote_refuses_an_import_the_node_cannot_run() {
    let (remote, requests) = spawn_recording_agent_resolving(PULL_FAILS, "linux/arm64").await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: true,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-arch".to_string(), 1, 1, 1);

    let error = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect_err("an image the node resolves for another architecture is refused");

    assert_agent_requests(
        &requests,
        &["exists", "import", "inspect"],
        "the imported image is inspected on the node",
    );
    let message = error.to_string();
    assert!(message.contains("worker-1"), "{message}");
    assert!(message.contains("linux/amd64"), "{message}");
    assert!(message.contains("linux/arm64"), "{message}");
}

/// Job configs planned before `image_source` existed carry only
/// `use_external_image: true`. A `temps.internal/` ref must still be
/// treated as control-plane-local, so deployments already queued when the
/// fix ships don't hit the same failure.
#[tokio::test]
async fn ensure_image_on_remote_imports_reserved_ref_from_legacy_config() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    // No image source declared; even an explicit `Registry` would be
    // overridden for a reserved ref (see `DeployImageSource::resolve`).
    let job = uploaded_image_job(
        None,
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: true,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-legacy".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "import", "inspect"],
        "a reserved temps.internal ref from a legacy config must be imported, never pulled",
    );
    result.expect("a reserved temps.internal ref must be transferred via import");
    assert!(save_image_called.load(Ordering::SeqCst));
}

/// A registry image is pulled by the worker itself; the control plane is
/// not involved.
#[tokio::test]
async fn ensure_image_on_remote_pulls_registry_image_on_node() {
    let (remote, requests) = spawn_recording_agent(PULL_OK).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer)
        .with_external_image_tag(REGISTRY_IMAGE.to_string())
        .with_image_source(DeployImageSource::Registry);
    job.image_builder = Some(Arc::new(RecordingImageBuilder {
        save_image_called: save_image_called.clone(),
        has_image: true,
    }));
    let context = crate::test_utils::create_test_context("wf-pull".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "pull"],
        "a registry image must be pulled by the node",
    );
    result.expect("registry pull should succeed");
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// An [`ImageBuilder`] whose image can be pruned and pulled back:
/// `inspect_image` answers from `present`, and `fetch_registry_image`
/// records the pull and (unless `pull_fails`) makes the image present
/// under `pulled_id`.
struct RepullImageBuilder {
    present: AtomicBool,
    pull_fails: bool,
    pulled_id: &'static str,
    fetches: std::sync::Mutex<Vec<String>>,
}

impl RepullImageBuilder {
    fn new(present: bool, pull_fails: bool, pulled_id: &'static str) -> Self {
        Self {
            present: AtomicBool::new(present),
            pull_fails,
            pulled_id,
            fetches: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn fetches(&self) -> Vec<String> {
        self.fetches
            .lock()
            .expect("repull builder mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl temps_deployer::ImageBuilder for RepullImageBuilder {
    async fn build_image(
        &self,
        _request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn build_image_with_callback(
        &self,
        _request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn fetch_registry_image(
        &self,
        reference: &str,
        _tag: &str,
        _credentials: Option<temps_deployer::remote::RemotePullCredentials>,
    ) -> Result<(), temps_deployer::BuilderError> {
        self.fetches
            .lock()
            .expect("repull builder mutex poisoned")
            .push(reference.to_string());
        if self.pull_fails {
            return Err(temps_deployer::BuilderError::Other(format!(
                "manifest for {reference} not found"
            )));
        }
        self.present.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn import_image(
        &self,
        _image_path: PathBuf,
        _tag: &str,
    ) -> Result<String, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn save_image(
        &self,
        _image_name: &str,
        _output_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &std::path::Path,
    ) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), temps_deployer::BuilderError> {
        unimplemented!("not used")
    }

    async fn inspect_image(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
        if !self.present.load(Ordering::SeqCst) {
            return Err(temps_deployer::BuilderError::ImageNotFound(
                image_name.to_string(),
            ));
        }
        Ok(temps_deployer::ImageInfo {
            id: self.pulled_id.to_string(),
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            platform: "linux/amd64".to_string(),
            size_bytes: 0,
            tags: vec![image_name.to_string()],
            created: None,
            working_dir: None,
            config: None,
        })
    }

    fn get_native_platform(&self) -> String {
        "linux/amd64".to_string()
    }
}

fn repull_job(
    builder: Arc<RepullImageBuilder>,
    source: DeployImageSource,
) -> (DeployImageJob, WorkflowContext) {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer)
        .with_external_image_tag(REGISTRY_IMAGE.to_string())
        .with_image_source(source);
    job.image_builder = Some(builder);
    let context = crate::test_utils::create_test_context("wf-repull".to_string(), 1, 1, 1);
    (job, context)
}

/// Image retention now removes expired registry images from the control
/// plane. A rollback onto one must pull it again rather than fail at
/// container creation with "No such image".
#[tokio::test]
async fn local_rollback_repulls_a_registry_image_that_retention_removed() {
    let builder = Arc::new(RepullImageBuilder::new(false, false, "sha256:pulled"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::Registry);

    job.ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect("a pruned registry image must be pulled again");

    assert_eq!(builder.fetches(), vec![REGISTRY_IMAGE.to_string()]);
}

/// An image that is still here is never pulled again: a pull would be a
/// needless registry round trip, and a mutable tag could move under it.
#[tokio::test]
async fn local_deploy_does_not_repull_a_registry_image_that_is_present() {
    let builder = Arc::new(RepullImageBuilder::new(true, false, "sha256:local"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::Registry);

    job.ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect("a present image needs nothing");

    assert!(builder.fetches().is_empty());
}

/// An uploaded or locally built image has no registry to come from, so
/// its absence is never "fixed" by pulling a same-named public image.
#[tokio::test]
async fn local_deploy_never_pulls_a_control_plane_local_image() {
    let builder = Arc::new(RepullImageBuilder::new(false, false, "sha256:pulled"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::ControlPlaneLocal);

    job.ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect("control-plane-local images are left to the deploy path");

    assert!(builder.fetches().is_empty());
}

/// A registry that no longer serves the tag fails the rollback with an
/// error that names the image, the deployment and the remedy.
#[tokio::test]
async fn failed_repull_explains_what_is_missing() {
    let builder = Arc::new(RepullImageBuilder::new(false, true, "sha256:pulled"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::Registry);

    let error = job
        .ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect_err("a failed re-pull must fail the deploy");

    let message = error.to_string();
    assert!(message.contains(REGISTRY_IMAGE), "{message}");
    assert!(message.contains("pulling it again"), "{message}");
    assert!(message.contains("manifest for"), "{message}");
}

/// A deployment bound to a recorded image must not be rolled back onto a
/// different image the mutable tag points at today.
#[tokio::test]
async fn repulled_image_must_match_the_recorded_identity() {
    let builder = Arc::new(RepullImageBuilder::new(false, false, "sha256:moved"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::Registry);
    let job = job.with_expected_image_identity(
        ExpectedImageIdentity::image_id("sha256:original").expect("non-empty id"),
    );

    let error = job
        .ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect_err("a re-pulled image with another identity must be refused");

    let message = error.to_string();
    assert!(message.contains("different image"), "{message}");
    assert!(message.contains("sha256:moved"), "{message}");

    // The refused pull left the moved tag here. A retry finds it present
    // and must refuse it again instead of starting it.
    let retry = job
        .ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect_err("a retry must not start the image the first attempt refused");
    let message = retry.to_string();
    assert!(message.contains("different image"), "{message}");
    assert!(message.contains("sha256:moved"), "{message}");
    assert_eq!(
        builder.fetches().len(),
        1,
        "the retry checks the image already here instead of pulling again"
    );
}

/// A rollback whose recorded image is still here starts it without a pull.
#[tokio::test]
async fn present_image_matching_the_recorded_identity_is_used_as_is() {
    let builder = Arc::new(RepullImageBuilder::new(true, false, "sha256:original"));
    let (job, context) = repull_job(builder.clone(), DeployImageSource::Registry);
    let job = job.with_expected_image_identity(
        ExpectedImageIdentity::image_id("sha256:original").expect("non-empty id"),
    );

    job.ensure_registry_image_local(REGISTRY_IMAGE, &context)
        .await
        .expect("the recorded image is here");
    assert!(builder.fetches().is_empty());
}

/// #1892: an arm64 worker pulls the arm64 variant of a multi-platform
/// registry tag itself. The control plane's own pull downloaded only
/// amd64, which must not make the node look unable to run the image.
#[tokio::test]
async fn ensure_image_on_remote_pulls_multi_arch_registry_image_on_arm64_node() {
    let (remote, requests) =
        spawn_recording_agent_on(PULL_OK, false, vec!["linux/arm64"], "linux/arm64").await;
    assert_eq!(remote.platform().as_deref(), Some("linux/arm64"));
    let job = job_with_image_builder(amd64_control_plane_with_multi_arch_index())
        .with_external_image_tag(REGISTRY_IMAGE.to_string())
        .with_image_source(DeployImageSource::Registry);
    let context = crate::test_utils::create_test_context("wf-arm64".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    result.expect("the registry index offers linux/arm64, so the node can pull and run it");
    assert_agent_requests(
        &requests,
        &["exists", "pull"],
        "the arm64 node pulls its own variant from the registry",
    );
}

/// Scheduling and every replica's node check ask for the same tag's
/// platforms: the registry index is read once per job, whether the read
/// succeeds or not. An unreachable registry would otherwise be waited on
/// again for every replica, each time to fall back to the local store.
#[tokio::test]
async fn registry_index_is_read_once_per_job_even_when_unreadable() {
    for (registry_platforms, node_platform) in [
        (
            vec!["linux/amd64".into(), "linux/arm64".into()],
            "linux/arm64",
        ),
        // Unreadable: the local store's amd64 answers, so amd64 nodes.
        (Vec::new(), "linux/amd64"),
    ] {
        let mut builder = amd64_control_plane_with_multi_arch_index();
        builder.registry_platforms = registry_platforms;
        let registry_reads = builder.registry_reads.clone();
        let job = job_with_image_builder(builder)
            .with_external_image_tag(REGISTRY_IMAGE.to_string())
            .with_image_source(DeployImageSource::Registry);
        let context = crate::test_utils::create_test_context("wf-once".to_string(), 1, 1, 1);
        let mut output = build_output_with_tags(&[]);
        output.image_tag = REGISTRY_IMAGE.to_string();

        assert!(job
            .available_image_platforms(&output)
            .await
            .iter()
            .any(|platform| temps_deployer::platform::platforms_match(platform, node_platform)));
        for _ in 0..3 {
            let (remote, _requests) =
                spawn_recording_agent_on(PULL_OK, false, vec![node_platform], node_platform).await;
            job.ensure_image_on_remote(
                REGISTRY_IMAGE,
                &remote,
                "worker-1",
                &context,
                None,
                Some(1),
            )
            .await
            .expect("the node runs a platform the job has an image for");
        }

        assert_eq!(
            registry_reads.load(Ordering::SeqCst),
            1,
            "scheduling plus three node checks for {node_platform}"
        );
    }
}

/// A registry-image job whose control plane holds a copy of the tag
/// (`RecordingImageBuilder` reports `sha256:local`), with the image ID the
/// dependency job resolved in this workflow recorded as `resolved_image_id`.
fn registry_fallback_job(
    save_image_called: &Arc<AtomicBool>,
    resolved_image_id: Option<&str>,
) -> (DeployImageJob, WorkflowContext) {
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer)
        .with_external_image_tag(REGISTRY_IMAGE.to_string())
        .with_image_source(DeployImageSource::Registry);
    job.image_builder = Some(Arc::new(RecordingImageBuilder {
        save_image_called: save_image_called.clone(),
        has_image: true,
    }));
    let mut context = crate::test_utils::create_test_context("wf-fallback".to_string(), 1, 1, 1);
    if let Some(image_id) = resolved_image_id {
        // What `PullExternalImageJob` records; `job_with_target` depends on
        // the job id "build".
        context
            .set_output("build", "image_id", image_id)
            .expect("record resolved image id");
    }
    (job, context)
}

/// A registry image whose pull fails on the worker (registry unreachable
/// from that node, missing credentials there, ...) is still deliverable
/// when the control plane's copy is provably the image this deployment
/// resolved — here, the ID `PullExternalImageJob` recorded.
#[tokio::test]
async fn ensure_image_on_remote_falls_back_to_verified_control_plane_copy() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, Some("sha256:local"));

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "pull", "import", "inspect"],
        "a failed registry pull must fall back to importing the verified control-plane copy, \
             then check the node can run it",
    );
    result.expect("a failed registry pull must fall back to the verified control-plane copy");
    assert!(
        save_image_called.load(Ordering::SeqCst),
        "expected the fallback to export the control plane's copy"
    );
}

/// The registry fallback imports the control plane's copy; a copy without
/// the node's architecture is refused like any other import, instead of
/// failing at start with `exec format error`.
#[tokio::test]
async fn ensure_image_on_remote_refuses_a_fallback_import_the_node_cannot_run() {
    let (remote, requests) = spawn_recording_agent_resolving(PULL_FAILS, "linux/arm64").await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, Some("sha256:local"));

    let message = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect_err("a fallback import the node resolves for another architecture is refused")
        .to_string();

    assert_agent_requests(
        &requests,
        &["exists", "pull", "import", "inspect"],
        "the fallback import is inspected on the node",
    );
    assert!(message.contains("worker-1"), "{message}");
    assert!(message.contains("linux/amd64"), "{message}");
    assert!(message.contains("linux/arm64"), "{message}");
}

/// A node that already holds the tag for its own architecture keeps it:
/// nothing is pulled or imported.
#[tokio::test]
async fn ensure_image_on_remote_reuses_a_cached_image_the_node_can_run() {
    let (remote, requests) =
        spawn_recording_agent_holding(PULL_OK, true, vec!["linux/amd64"]).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: true,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-cached".to_string(), 1, 1, 1);

    job.ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect("a cached image for the node's architecture is reused");

    assert_agent_requests(
        &requests,
        &["exists", "inspect"],
        "a cached image the node can run is only inspected",
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// A node that cached the tag for another architecture (an earlier
/// single-arch push) gets the image again rather than starting a
/// container that dies with `exec format error`.
#[tokio::test]
async fn ensure_image_on_remote_replaces_a_cached_image_for_another_architecture() {
    let (remote, requests) =
        spawn_recording_agent_holding(PULL_FAILS, true, vec!["linux/arm64", "linux/amd64"]).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: true,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-stale".to_string(), 1, 1, 1);

    job.ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect("a cached image for another architecture is replaced");

    assert_agent_requests(
        &requests,
        &["exists", "inspect", "import", "inspect"],
        "a cached image for another architecture is re-imported and checked again",
    );
    assert!(save_image_called.load(Ordering::SeqCst));
}

/// A cached registry image for another architecture is pulled again, past
/// the agent's cached-image shortcut, and the node is checked to resolve
/// the tag for its own platform afterwards.
#[tokio::test]
async fn ensure_image_on_remote_pulls_again_over_a_cached_image_for_another_architecture() {
    let (remote, requests) =
        spawn_recording_agent_holding(PULL_OK, true, vec!["linux/arm64", "linux/amd64"]).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, None);

    job.ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect("the registry pull replaces the cached image");

    assert_agent_requests(
        &requests,
        &["exists", "inspect", "pull-replacing", "inspect"],
        "a cached registry image for another architecture is pulled again and rechecked",
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// A replacing pull the registry refuses (the agent reports it failed
/// rather than handing back the cached copy) falls back to the verified
/// control-plane copy, which is checked like any import.
#[tokio::test]
async fn ensure_image_on_remote_falls_back_when_replacing_a_cached_image_fails() {
    let (remote, requests) =
        spawn_recording_agent_holding(PULL_FAILS, true, vec!["linux/arm64", "linux/amd64"]).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, Some("sha256:local"));

    job.ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect("the verified control-plane copy replaces the cached image");

    assert_agent_requests(
        &requests,
        &["exists", "inspect", "pull-replacing", "import", "inspect"],
        "a failed replacing pull falls back to importing the verified copy, then rechecks",
    );
    assert!(save_image_called.load(Ordering::SeqCst));
}

/// An agent that predates replacing a cached image hands the old copy back
/// from the pull: the deploy is refused with the reason instead of starting
/// a container that dies with `exec format error`.
#[tokio::test]
async fn ensure_image_on_remote_refuses_a_cached_image_the_pull_did_not_replace() {
    let (remote, requests) =
        spawn_recording_agent_holding(PULL_OK, true, vec!["linux/arm64"]).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, None);

    let message = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await
        .expect_err("a cached image still for another architecture is refused")
        .to_string();

    assert_agent_requests(
        &requests,
        &["exists", "inspect", "pull-replacing", "inspect"],
        "the image is rechecked after the replacing pull",
    );
    assert!(message.contains("worker-1"), "{message}");
    assert!(message.contains("linux/amd64"), "{message}");
    assert!(message.contains("linux/arm64"), "{message}");
    assert!(message.contains("upgrade the node agent"), "{message}");
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// The tag was re-pointed on the control plane after this deployment
/// resolved it: exporting it would run a different image on the worker,
/// so the pull failure stands.
#[tokio::test]
async fn ensure_image_on_remote_refuses_fallback_when_control_plane_copy_differs() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, Some("sha256:other"));

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "pull"],
        "a re-pointed tag on the control plane must never be imported",
    );
    let message = result
        .expect_err("a mismatched control-plane copy must not be used")
        .to_string();
    assert!(
        message.contains("Failed to pull image 'ghcr.io/example-org/app:v1'")
            && message.contains("manifest unknown")
            && message.contains("holds a different image under this tag"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// No identity was recorded (the pull was deferred to the worker, or an
/// older workflow): a tag match alone is not trusted.
#[tokio::test]
async fn ensure_image_on_remote_refuses_fallback_without_recorded_identity() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, None);

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "pull"],
        "without a recorded identity the control-plane copy must not be imported",
    );
    let message = result
        .expect_err("an unverifiable control-plane copy must not be used")
        .to_string();
    assert!(
        message.contains("manifest unknown") && message.contains("no recorded image identity"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// An explicitly bound identity (rollback/promotion: the origin's
/// registered digest) takes precedence over workflow outputs; a local copy
/// that doesn't carry that digest is refused.
#[tokio::test]
async fn ensure_image_on_remote_refuses_fallback_when_bound_digest_is_absent() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let (job, context) = registry_fallback_job(&save_image_called, Some("sha256:local"));
    let job = job.with_expected_image_identity(ExpectedImageIdentity::RepoDigest(
        "sha256:0d1e2f".to_string(),
    ));

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(&requests, &["exists", "pull"], "digest mismatch: no import");
    let message = result.expect_err("digest mismatch").to_string();
    assert!(
        message.contains("registry digest sha256:0d1e2f"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

fn temp_archive(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("temps-saved-{label}-{}.tar", uuid::Uuid::new_v4()))
}

const TAG: &str = "ghcr.io/example-org/app:v1";
const OTHER_TAG: &str = "ghcr.io/example-org/app:v2";

async fn verify_archive(
    entries: &[(&str, Vec<u8>)],
    tag: &str,
    expected: &str,
) -> Result<(), String> {
    let archive = temp_archive("verify");
    write_archive(&archive, entries).unwrap();
    let result = verify_saved_image_id(&archive, tag, expected).await;
    let _ = std::fs::remove_file(archive);
    result
}

#[tokio::test]
async fn verify_saved_image_id_accepts_classic_single_entry_in_either_layout() {
    for config in ["blobs/sha256/abc123", "abc123.json"] {
        assert_eq!(
            verify_archive(
                &[("manifest.json", manifest_json(&[(config, &[TAG])]))],
                TAG,
                "sha256:abc123"
            )
            .await,
            Ok(()),
            "{config}"
        );
    }
}

/// Several entries; only the one tagged with the exported tag matters.
#[tokio::test]
async fn verify_saved_image_id_accepts_tagged_entry_among_others() {
    let manifest = manifest_json(&[
        ("aaa111.json", &[OTHER_TAG]),
        ("abc123.json", &[TAG]),
        ("bbb222.json", &[]),
    ]);
    assert_eq!(
        verify_archive(&[("manifest.json", manifest)], TAG, "sha256:abc123").await,
        Ok(())
    );
}

/// The expected image is in the archive, but under another tag: the
/// exported tag names something else, which is what the worker would run.
#[tokio::test]
async fn verify_saved_image_id_rejects_expected_image_under_another_tag() {
    let manifest = manifest_json(&[("abc123.json", &[OTHER_TAG]), ("def456.json", &[TAG])]);
    let err = verify_archive(&[("manifest.json", manifest)], TAG, "sha256:abc123")
        .await
        .expect_err("the exported tag names a different image");
    assert!(err.contains("names sha256:def456"), "{err}");
}

/// containerd store: the tag-annotated index descriptor carries the image
/// (index) digest; the per-platform manifest.json configs differ from it.
#[tokio::test]
async fn verify_saved_image_id_accepts_containerd_index_named_by_tag() {
    let entries = [
        (
            "index.json",
            oci_index_json(&[(
                "sha256:idx999",
                &[
                    (CONTAINERD_IMAGE_NAME_ANNOTATION, TAG),
                    (OCI_REF_NAME_ANNOTATION, "v1"),
                ],
            )]),
        ),
        (
            "manifest.json",
            manifest_json(&[
                ("blobs/sha256/cfgamd", &[TAG]),
                ("blobs/sha256/cfgarm", &[TAG]),
            ]),
        ),
    ];
    assert_eq!(verify_archive(&entries, TAG, "sha256:idx999").await, Ok(()));
}

/// Docker's OCI layout on the classic store: index.json names the tag with
/// a manifest digest (a different kind), manifest.json with the config
/// digest that `inspect` reported.
#[tokio::test]
async fn verify_saved_image_id_accepts_classic_config_in_oci_layout() {
    let entries = [
        (
            "index.json",
            oci_index_json(&[("sha256:man555", &[(OCI_REF_NAME_ANNOTATION, "v1")])]),
        ),
        (
            "manifest.json",
            manifest_json(&[("blobs/sha256/abc123", &[TAG])]),
        ),
    ];
    assert_eq!(verify_archive(&entries, TAG, "sha256:abc123").await, Ok(()));
}

/// The reported race: the tag was re-pointed to a new index that still
/// references the verified image in a nested index. Only what the tag
/// names counts, so this is rejected.
#[tokio::test]
async fn verify_saved_image_id_rejects_expected_image_only_in_nested_index() {
    let entries = [
        (
            "blobs/sha256/new777",
            oci_index_json(&[("sha256:idx999", &[])]),
        ),
        (
            "index.json",
            oci_index_json(&[("sha256:new777", &[(CONTAINERD_IMAGE_NAME_ANNOTATION, TAG)])]),
        ),
        (
            "manifest.json",
            manifest_json(&[("blobs/sha256/cfgnew", &[TAG])]),
        ),
    ];
    let err = verify_archive(&entries, TAG, "sha256:idx999")
        .await
        .expect_err("the tag names a different index");
    assert!(err.contains("sha256:new777"), "{err}");
}

#[tokio::test]
async fn verify_saved_image_id_rejects_archive_not_naming_the_tag() {
    let entries = [
        (
            "index.json",
            oci_index_json(&[(
                "sha256:abc123",
                &[(CONTAINERD_IMAGE_NAME_ANNOTATION, OTHER_TAG)],
            )]),
        ),
        (
            "manifest.json",
            manifest_json(&[("abc123.json", &[OTHER_TAG])]),
        ),
    ];
    let err = verify_archive(&entries, TAG, "sha256:abc123")
        .await
        .expect_err("the archive never names the exported tag");
    assert!(
        err.contains("does not name ghcr.io/example-org/app:v1"),
        "{err}"
    );
}

/// Docker writes familiar names (`nginx:1.27`); the export may be asked
/// for the fully qualified form, and vice versa.
#[tokio::test]
async fn verify_saved_image_id_normalizes_docker_hub_references() {
    let manifest = manifest_json(&[("abc123.json", &["nginx:1.27"])]);
    assert_eq!(
        verify_archive(
            &[("manifest.json", manifest.clone())],
            "docker.io/library/nginx:1.27",
            "sha256:abc123"
        )
        .await,
        Ok(())
    );
    let index = oci_index_json(&[(
        "sha256:idx999",
        &[(
            CONTAINERD_IMAGE_NAME_ANNOTATION,
            "docker.io/library/nginx:1.27",
        )],
    )]);
    assert_eq!(
        verify_archive(&[("index.json", index)], "nginx:1.27", "sha256:idx999").await,
        Ok(())
    );
    // A different repository with the same tag is not the same reference.
    assert!(verify_archive(
        &[("manifest.json", manifest)],
        "ghcr.io/example-org/nginx:1.27",
        "sha256:abc123"
    )
    .await
    .is_err());
}

#[test]
fn normalize_image_ref_canonicalizes_references() {
    assert_eq!(
        normalize_image_ref("nginx"),
        "docker.io/library/nginx:latest"
    );
    assert_eq!(
        normalize_image_ref("nginx:1.27"),
        "docker.io/library/nginx:1.27"
    );
    assert_eq!(
        normalize_image_ref("index.docker.io/library/nginx:1.27"),
        "docker.io/library/nginx:1.27"
    );
    assert_eq!(normalize_image_ref("org/app:v1"), "docker.io/org/app:v1");
    assert_eq!(normalize_image_ref(TAG), TAG);
    assert_eq!(
        normalize_image_ref("localhost:5000/app"),
        "localhost:5000/app:latest"
    );
    assert_eq!(
        normalize_image_ref("temps.internal/project-1/environment-2/upload-0f3c9a:immutable"),
        "temps.internal/project-1/environment-2/upload-0f3c9a:immutable"
    );
}

#[tokio::test]
async fn verify_saved_image_id_errors_on_unreadable_archives() {
    let garbage = temp_archive("garbage");
    std::fs::write(&garbage, b"not a tar").unwrap();
    assert!(verify_saved_image_id(&garbage, TAG, "sha256:abc123")
        .await
        .is_err());
    let _ = std::fs::remove_file(garbage);

    let err = verify_archive(
        &[("blobs/sha256/abc123", b"{}".to_vec())],
        TAG,
        "sha256:abc123",
    )
    .await
    .expect_err("an archive without metadata proves nothing");
    assert!(
        err.contains("neither manifest.json nor index.json"),
        "{err}"
    );
}

/// Without a local copy there is nothing to fall back to: the pull error
/// is reported as before, and no export is attempted.
#[tokio::test]
async fn ensure_image_on_remote_reports_pull_failure_without_local_copy() {
    let (remote, requests) = spawn_recording_agent(PULL_FAILS).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let container_deployer: Arc<dyn ContainerDeployer> =
        Arc::new(TrackingMockContainerDeployer::new());
    let mut job = job_with_target(container_deployer)
        .with_external_image_tag(REGISTRY_IMAGE.to_string())
        .with_image_source(DeployImageSource::Registry);
    job.image_builder = Some(Arc::new(RecordingImageBuilder {
        save_image_called: save_image_called.clone(),
        has_image: false,
    }));
    let context = crate::test_utils::create_test_context("wf-pull-fail".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(REGISTRY_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists", "pull"],
        "with no local copy the deploy must stop after the failed pull",
    );
    let message = result
        .expect_err("a failed pull with no local copy must fail the deploy")
        .to_string();
    assert!(
        message.contains("Failed to pull image 'ghcr.io/example-org/app:v1'")
            && message.contains("manifest unknown"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// A control-plane-local image on a control plane with no image builder
/// (Docker disabled) fails with the registry remedy — and never tries a
/// pull that is guaranteed to fail with a confusing DNS error.
#[tokio::test]
async fn ensure_image_on_remote_refuses_local_image_without_image_builder() {
    let (remote, requests) = spawn_recording_agent(PULL_OK).await;
    let job = uploaded_image_job(Some(DeployImageSource::ControlPlaneLocal), None);
    let context = crate::test_utils::create_test_context("wf-no-builder".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists"],
        "the refusal must happen before any pull or import",
    );
    let message = result
        .expect_err("a control-plane-local image cannot be delivered without a builder")
        .to_string();
    assert!(
        message.contains("Failed to deliver image")
            && message.contains("exists only in the control plane's local image store")
            && message.contains("no image builder (Docker is disabled here)")
            && message.contains("temps deploy:image"),
        "error should explain the cause and the remedy: {message}"
    );
}

/// Control-plane serve profile (`local_workloads_enabled = false`): the
/// image builder is wired but its Docker does not hold the image, so the
/// deploy is refused explicitly instead of attempting a registry pull.
#[tokio::test]
async fn ensure_image_on_remote_refuses_local_image_on_control_plane_profile() {
    use crate::services::{NodeScheduler, NodeService};
    use sea_orm::{DatabaseBackend, MockDatabase};

    let (remote, requests) = spawn_recording_agent(PULL_OK).await;
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let scheduler = Arc::new(
        NodeScheduler::new(Arc::new(NodeService::new(Arc::new(db))))
            .with_local_workloads_enabled(false),
    );
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: false,
        }),
    )
    .with_node_scheduler(scheduler);
    let context = crate::test_utils::create_test_context("wf-cp-profile".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists"],
        "the refusal must happen before any pull or import",
    );
    let message = result
        .expect_err("the control-plane profile cannot export a local image")
        .to_string();
    assert!(
        message.contains("control-plane profile")
            && message.contains("no Docker daemon to export it from"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// Full profile, Docker available, but the image is gone from the control
/// plane (pruned): the error says so and points at re-uploading, instead
/// of blaming a missing Docker daemon.
#[tokio::test]
async fn ensure_image_on_remote_reports_pruned_local_image() {
    let (remote, requests) = spawn_recording_agent(PULL_OK).await;
    let save_image_called = Arc::new(AtomicBool::new(false));
    let job = uploaded_image_job(
        Some(DeployImageSource::ControlPlaneLocal),
        Some(RecordingImageBuilder {
            save_image_called: save_image_called.clone(),
            has_image: false,
        }),
    );
    let context = crate::test_utils::create_test_context("wf-pruned".to_string(), 1, 1, 1);

    let result = job
        .ensure_image_on_remote(UPLOADED_IMAGE, &remote, "worker-1", &context, None, Some(1))
        .await;

    assert_agent_requests(
        &requests,
        &["exists"],
        "the refusal must happen before any pull or import",
    );
    let message = result
        .expect_err("a pruned local image cannot be delivered")
        .to_string();
    assert!(
        message.contains("may have been pruned")
            && message.contains("temps deploy:local-image")
            && !message.contains("no Docker daemon"),
        "unexpected error: {message}"
    );
    assert!(!save_image_called.load(Ordering::SeqCst));
}

/// The refusal and fallback messages are logged at their real level; the
/// keyword-based classifier alone would file "ERROR: …" under info.
#[test]
fn delivery_messages_are_not_misclassified_by_keyword_detection() {
    assert_eq!(
        DeployImageJob::detect_log_level(
            "ERROR: Failed to deliver image 'x' to node 'y': it exists only in the \
                 control plane's local image store"
        ),
        LogLevel::Error
    );
}

fn external_image_job_from_config(config: &serde_json::Value) -> DeployImageJob {
    let image = config["image_name"]
        .as_str()
        .expect("image_name")
        .to_string();
    DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .build_job_id("verify_local_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .external_image_tag(image)
    .image_source_from_job_config(config)
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .expect("deploy job builds")
}

/// The execution service hands the planned job config to the builder via
/// `image_source_from_job_config`; the recorded source must reach the job.
#[test]
fn builder_applies_image_source_from_job_config() {
    // A non-reserved tag, so only the recorded source can make it local.
    let local = serde_json::json!({
        "image_name": "example-app-1666:latest",
        "use_external_image": true,
        "image_source": "control_plane_local",
    });
    let job = external_image_job_from_config(&local);
    assert_eq!(job.image_source, Some(DeployImageSource::ControlPlaneLocal));
    assert_eq!(
        job.resolved_image_source("example-app-1666:latest"),
        DeployImageSource::ControlPlaneLocal
    );

    let registry = serde_json::json!({
        "image_name": REGISTRY_IMAGE,
        "use_external_image": true,
        "image_source": "registry",
    });
    let job = external_image_job_from_config(&registry);
    assert_eq!(job.image_source, Some(DeployImageSource::Registry));
    assert_eq!(
        job.resolved_image_source(REGISTRY_IMAGE),
        DeployImageSource::Registry
    );
}

/// Configs planned before the field existed leave the source unset; the
/// job then derives it from the tag.
#[test]
fn builder_leaves_image_source_unset_for_legacy_job_config() {
    let legacy = serde_json::json!({
        "image_name": UPLOADED_IMAGE,
        "use_external_image": true,
    });
    let job = external_image_job_from_config(&legacy);
    assert_eq!(job.image_source, None);
    assert_eq!(
        job.resolved_image_source(UPLOADED_IMAGE),
        DeployImageSource::ControlPlaneLocal
    );

    let legacy_registry = serde_json::json!({
        "image_name": REGISTRY_IMAGE,
        "use_external_image": true,
    });
    let job = external_image_job_from_config(&legacy_registry);
    assert_eq!(
        job.resolved_image_source(REGISTRY_IMAGE),
        DeployImageSource::Registry
    );
}

/// Rollback/promotion bind the job through the builder; the bound
/// identity wins over any workflow output.
#[test]
fn expected_image_identity_prefers_bound_identity_over_workflow_output() {
    let job = DeployImageJobBuilder::new(
        "test-project",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .build_job_id("pull_external_image".to_string())
    .target(DeploymentTarget::Docker {
        registry_url: "local".to_string(),
        network: None,
    })
    .external_image_tag(REGISTRY_IMAGE.to_string())
    .expected_image_identity(ExpectedImageIdentity::RepoDigest("sha256:ddd".to_string()))
    .build(Arc::new(TrackingMockContainerDeployer::new()))
    .expect("deploy job builds");

    let mut context = crate::test_utils::create_test_context("wf-bound".to_string(), 1, 1, 1);
    context
        .set_output("pull_external_image", "image_id", "sha256:aaa")
        .unwrap();
    assert_eq!(
        job.expected_image_identity(&context),
        Some(ExpectedImageIdentity::RepoDigest("sha256:ddd".to_string()))
    );

    // Unbound: the dependency's resolved image ID; an empty one (pull
    // deferred to the worker) is no identity at all.
    let unbound = job_with_target(Arc::new(TrackingMockContainerDeployer::new()));
    let mut context = crate::test_utils::create_test_context("wf-out".to_string(), 1, 1, 1);
    assert_eq!(unbound.expected_image_identity(&context), None);
    context.set_output("build", "image_id", "").unwrap();
    assert_eq!(unbound.expected_image_identity(&context), None);
    context
        .set_output("build", "image_id", "sha256:aaa")
        .unwrap();
    assert_eq!(
        unbound.expected_image_identity(&context),
        Some(ExpectedImageIdentity::ImageId("sha256:aaa".to_string()))
    );
}
