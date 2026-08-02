//! Real cross-architecture build test.
//!
//! Runs `BuildImageJob` against a live Docker daemon with two target
//! platforms and asserts that each produced tag really carries the requested
//! architecture. This is the CI guard for the multi-arch build path: the
//! unit tests around it use a mock `ImageBuilder`, so they can prove the job
//! *asks* for two platforms but not that the platform actually reaches the
//! daemon or that the resulting images differ.
//!
//! **No QEMU required.** The fixture Dockerfile has no `RUN` instruction, so
//! nothing is executed during the build — Docker only pulls the foreign-arch
//! base image and stacks layers. That keeps this test fast enough to run on
//! every PR while still exercising the real `platform` plumbing.
//!
//! Skips gracefully (never `#[ignore]`) when Docker is unavailable.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use temps_core::{LogWriter, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::{docker::DockerRuntime, ImageBuilder};
use temps_deployments::jobs::BuildImageJobBuilder;

struct NoopLogWriter;

#[async_trait]
impl LogWriter for NoopLogWriter {
    async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        1
    }
}

/// Build context whose Dockerfile executes nothing — see the module docs.
fn write_fixture(dir: &std::path::Path) {
    std::fs::write(
        dir.join("Dockerfile"),
        "FROM alpine:3.20\nLABEL sh.temps.test=\"multiarch\"\n",
    )
    .expect("write Dockerfile");
}

/// Feed the outputs `BuildImageJob` expects from the download job.
fn context_with_repo(repo_dir: &std::path::Path) -> WorkflowContext {
    let mut context = WorkflowContext::new(
        "multiarch-test".to_string(),
        1,
        1,
        1,
        Arc::new(NoopLogWriter),
    );
    context
        .set_output(
            "download_repo",
            "repo_dir",
            repo_dir.to_string_lossy().to_string(),
        )
        .unwrap();
    context
        .set_output("download_repo", "checkout_ref", "main")
        .unwrap();
    context
        .set_output("download_repo", "repo_owner", "temps")
        .unwrap();
    context
        .set_output("download_repo", "repo_name", "multiarch-fixture")
        .unwrap();
    context
}

#[tokio::test]
async fn test_cross_platform_build_produces_images_of_both_architectures() {
    use bollard::Docker;

    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker daemon not responding, skipping");
        return;
    }

    // BuildKit, like the control plane uses in production — and unlike the
    // legacy builder, which silently ignores the requested platform (see
    // `test_legacy_builder_platform_mismatch_is_caught` below).
    let runtime = Arc::new(DockerRuntime::new(
        docker.clone(),
        true,
        "temps-multiarch-test".to_string(),
    ));
    // The build path creates the network up front. An existing network is
    // fine — only a hard failure means this runner can't build.
    if let Err(e) = runtime.ensure_network_exists().await {
        if !e.to_string().contains("already exists") {
            println!("Could not create test network ({e}), skipping");
            return;
        }
    }

    // Ask for the daemon's own platform plus the other mainstream one, so the
    // test covers a genuine cross-architecture build wherever it runs (amd64
    // CI today, arm64 laptops just as well).
    let Some(native) = runtime.refresh_daemon_platform().await else {
        println!("Daemon did not report a platform, skipping");
        return;
    };
    let foreign = if temps_deployer::platform::platforms_match(&native, "linux/amd64") {
        "linux/arm64"
    } else {
        "linux/amd64"
    };

    let temp_dir = tempfile::tempdir().expect("temp dir");
    write_fixture(temp_dir.path());

    // Unique tag: this test may run alongside others on a shared Docker host,
    // and it must only ever touch images it created itself.
    let tag = format!("temps-multiarch-test-{}:latest", uuid::Uuid::new_v4());
    let foreign_tag = format!(
        "{}-{}",
        tag,
        temps_deployer::platform::platform_tag_suffix(foreign)
    );

    let image_builder: Arc<dyn ImageBuilder> = runtime.clone();
    let job = BuildImageJobBuilder::new()
        .job_id("build_multiarch".to_string())
        .download_job_id("download_repo".to_string())
        .image_tag(tag.clone())
        .dockerfile_path("Dockerfile".to_string())
        .target_platforms(vec![native.clone(), foreign.to_string()])
        .build(image_builder.clone())
        .expect("build job");

    let result = job.execute(context_with_repo(temp_dir.path())).await;

    // Clean up before asserting, so a failed assertion doesn't leave images
    // behind on a shared host.
    let cleanup = |image: String, builder: Arc<dyn ImageBuilder>| async move {
        let _ = builder.remove_image(&image).await;
    };

    let build_error = result.as_ref().err().map(|e| e.to_string());
    let native_info = image_builder.inspect_image(&tag).await;
    let foreign_info = image_builder.inspect_image(&foreign_tag).await;

    cleanup(tag.clone(), image_builder.clone()).await;
    cleanup(foreign_tag.clone(), image_builder.clone()).await;

    if let Some(error) = build_error {
        // A runner without network can't pull the foreign base image. That's
        // an environment limitation, not a regression — everything else is a
        // real failure.
        if error.contains("no matching manifest")
            || error.contains("failed to resolve")
            || error.contains("dial tcp")
        {
            println!("Could not pull the {foreign} base image ({error}), skipping");
            return;
        }
        panic!("multi-platform build failed: {error}");
    }

    let native_info = native_info.expect("native image should exist after the build");
    let foreign_info = foreign_info.expect("per-architecture tag should exist after the build");

    assert!(
        temps_deployer::platform::platforms_match(&native_info.platform, &native),
        "primary tag {tag} should be {native}, got {}",
        native_info.platform
    );
    assert!(
        temps_deployer::platform::platforms_match(&foreign_info.platform, foreign),
        "suffixed tag {foreign_tag} should be {foreign}, got {}",
        foreign_info.platform
    );
    // The two tags must be different images — a build that silently ignored
    // the platform would produce the same ID twice and pass every check above
    // except this one.
    assert_ne!(
        native_info.id, foreign_info.id,
        "the two platforms must produce distinct images"
    );
}

/// Single-platform builds must keep producing exactly one image with the
/// plain tag — the path every existing single-architecture cluster takes.
#[tokio::test]
async fn test_single_platform_build_produces_only_the_primary_tag() {
    use bollard::Docker;

    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker daemon not responding, skipping");
        return;
    }

    let runtime = Arc::new(DockerRuntime::new(
        docker.clone(),
        true,
        "temps-multiarch-test".to_string(),
    ));
    if let Err(e) = runtime.ensure_network_exists().await {
        if !e.to_string().contains("already exists") {
            println!("Could not create test network ({e}), skipping");
            return;
        }
    }

    let temp_dir = tempfile::tempdir().expect("temp dir");
    write_fixture(temp_dir.path());

    let tag = format!("temps-singlearch-test-{}:latest", uuid::Uuid::new_v4());
    let image_builder: Arc<dyn ImageBuilder> = runtime.clone();

    let job = BuildImageJobBuilder::new()
        .job_id("build_singlearch".to_string())
        .download_job_id("download_repo".to_string())
        .image_tag(tag.clone())
        .dockerfile_path("Dockerfile".to_string())
        .build(image_builder.clone())
        .expect("build job");

    let result = job.execute(context_with_repo(temp_dir.path())).await;

    let Some(native) = runtime.refresh_daemon_platform().await else {
        println!("Daemon did not report a platform, skipping");
        return;
    };
    let suffixed = format!(
        "{}-{}",
        tag,
        temps_deployer::platform::platform_tag_suffix(&native)
    );
    let primary_info = image_builder.inspect_image(&tag).await;
    let suffixed_info = image_builder.inspect_image(&suffixed).await;

    let _ = image_builder.remove_image(&tag).await;

    if let Err(e) = result {
        panic!("single-platform build failed: {e}");
    }

    assert!(
        primary_info.is_ok(),
        "the plain tag must exist after a single-platform build"
    );
    assert!(
        suffixed_info.is_err(),
        "a single-platform build must not create a per-architecture tag"
    );
}

/// Compile-time guard that the fixture path helper stays used even if the
/// tests above are skipped on a Docker-less machine.
#[test]
fn test_fixture_dockerfile_has_no_run_instruction() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_fixture(dir.path());
    let dockerfile = std::fs::read_to_string(dir.path().join("Dockerfile")).unwrap();
    // A `RUN` would execute foreign-architecture code and pull QEMU into the
    // requirements of this test — the whole point is that it doesn't.
    assert!(
        !dockerfile.contains("RUN "),
        "fixture must not execute anything during the build"
    );
    let _: PathBuf = dir.path().to_path_buf();
}

/// Docker's **legacy builder accepts `platform` and ignores it**: it reports
/// success and returns an image of the host's architecture. Left unchecked,
/// a cross-build on a daemon without BuildKit would tag an amd64 image as
/// `-arm64`, ship it to an arm64 node, and fail there — with an error blaming
/// the node instead of the build.
///
/// The build job inspects every cross-built image and refuses the mislabelled
/// result. This test pins that behaviour by deliberately building through the
/// legacy builder.
#[tokio::test]
async fn test_legacy_builder_platform_mismatch_is_caught() {
    use bollard::Docker;

    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker daemon not responding, skipping");
        return;
    }

    // `false` = legacy builder, the whole point of this test.
    let runtime = Arc::new(DockerRuntime::new(
        docker.clone(),
        false,
        "temps-multiarch-test".to_string(),
    ));
    if let Err(e) = runtime.ensure_network_exists().await {
        if !e.to_string().contains("already exists") {
            println!("Could not create test network ({e}), skipping");
            return;
        }
    }

    let Some(native) = runtime.refresh_daemon_platform().await else {
        println!("Daemon did not report a platform, skipping");
        return;
    };
    let foreign = if temps_deployer::platform::platforms_match(&native, "linux/amd64") {
        "linux/arm64"
    } else {
        "linux/amd64"
    };

    let temp_dir = tempfile::tempdir().expect("temp dir");
    write_fixture(temp_dir.path());

    let tag = format!("temps-legacy-arch-test-{}:latest", uuid::Uuid::new_v4());
    let foreign_tag = format!(
        "{}-{}",
        tag,
        temps_deployer::platform::platform_tag_suffix(foreign)
    );

    let image_builder: Arc<dyn ImageBuilder> = runtime.clone();
    let job = BuildImageJobBuilder::new()
        .job_id("build_legacy".to_string())
        .download_job_id("download_repo".to_string())
        .image_tag(tag.clone())
        .dockerfile_path("Dockerfile".to_string())
        .target_platforms(vec![native.clone(), foreign.to_string()])
        .build(image_builder.clone())
        .expect("build job");

    let result = job.execute(context_with_repo(temp_dir.path())).await;

    let foreign_platform = image_builder
        .inspect_image(&foreign_tag)
        .await
        .map(|info| info.platform);
    let _ = image_builder.remove_image(&tag).await;

    // Either way the deployment proceeds: the native image built fine, and a
    // secondary platform failing must not take down every deployment in the
    // cluster (an operator without QEMU installed would otherwise break all of
    // them by joining one ARM worker).
    assert!(
        result.is_ok(),
        "a mislabelled secondary platform must degrade, not fail the job: {:?}",
        result.err()
    );

    match foreign_platform {
        // The daemon honoured the platform after all — a future Docker release
        // could fix the legacy builder. Nothing to catch, and the tag stays.
        Ok(platform) if temps_deployer::platform::platforms_match(&platform, foreign) => {}
        // The expected case today: the tag held the wrong architecture, so the
        // job dropped it rather than advertising a mislabelled image. It must
        // also not be left behind in the image store, where someone could pick
        // it up by hand.
        Ok(platform) => {
            assert!(
                image_builder.inspect_image(&foreign_tag).await.is_err(),
                "the mislabelled {platform} image must not survive as {foreign_tag}"
            );
        }
        Err(_) => {}
    }

    let _ = image_builder.remove_image(&foreign_tag).await;
}

/// ARMv6 is the one advertised platform where the variant matters end to end.
///
/// Docker reports an ARMv6 image as `architecture = "arm"` with
/// `variant = "v6"`; a bare `arm` normalizes to v7, so dropping the variant
/// makes a correctly built ARMv6 image look like the legacy builder mislabelled
/// it — and the build fails. ARMv7 hides this bug, since `arm` and `arm/v7`
/// happen to agree.
#[tokio::test]
async fn test_armv6_build_keeps_its_variant() {
    use bollard::Docker;

    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    let docker = Arc::new(docker);
    if docker.ping().await.is_err() {
        println!("Docker daemon not responding, skipping");
        return;
    }

    let runtime = Arc::new(DockerRuntime::new(
        docker.clone(),
        true,
        "temps-multiarch-test".to_string(),
    ));
    if let Err(e) = runtime.ensure_network_exists().await {
        if !e.to_string().contains("already exists") {
            println!("Could not create test network ({e}), skipping");
            return;
        }
    }

    let Some(native) = runtime.refresh_daemon_platform().await else {
        println!("Daemon did not report a platform, skipping");
        return;
    };

    let temp_dir = tempfile::tempdir().expect("temp dir");
    write_fixture(temp_dir.path());

    let tag = format!("temps-armv6-test-{}:latest", uuid::Uuid::new_v4());
    let v6_tag = format!("{}-arm-v6", tag);

    let image_builder: Arc<dyn ImageBuilder> = runtime.clone();
    let job = BuildImageJobBuilder::new()
        .job_id("build_armv6".to_string())
        .download_job_id("download_repo".to_string())
        .image_tag(tag.clone())
        .dockerfile_path("Dockerfile".to_string())
        .target_platforms(vec![native.clone(), "linux/arm/v6".to_string()])
        .build(image_builder.clone())
        .expect("build job");

    let result = job.execute(context_with_repo(temp_dir.path())).await;

    let v6_info = image_builder.inspect_image(&v6_tag).await;
    let _ = image_builder.remove_image(&tag).await;
    let _ = image_builder.remove_image(&v6_tag).await;

    if let Err(e) = &result {
        let error = e.to_string();
        if error.contains("no matching manifest")
            || error.contains("failed to resolve")
            || error.contains("dial tcp")
        {
            println!("Could not pull the arm/v6 base image ({error}), skipping");
            return;
        }
        panic!("armv6 build failed: {error}");
    }

    let v6_info = v6_info.expect("the arm/v6 tag should exist after the build");
    assert!(
        temps_deployer::platform::platforms_match(&v6_info.platform, "linux/arm/v6"),
        "the built image must report arm/v6, got {}",
        v6_info.platform
    );
    // The distinction that matters: v6 must not be mistaken for v7.
    assert!(
        !temps_deployer::platform::platforms_match(&v6_info.platform, "linux/arm/v7"),
        "arm/v6 must not compare equal to arm/v7, got {}",
        v6_info.platform
    );
}
