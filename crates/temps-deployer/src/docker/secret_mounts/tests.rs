// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::docker::DockerRuntime;
use crate::{BuildRequest, ImageBuilder};
use futures::TryStreamExt;
use std::sync::Arc;
use tempfile::TempDir;

/// A throwaway Docker build for the #1725 tests, or `None` to skip them
/// when no daemon is reachable.
struct SecretBuildFixture {
    docker: bollard::Docker,
    runtime: DockerRuntime,
    network: String,
    image: String,
    context: TempDir,
}

impl SecretBuildFixture {
    async fn new(dockerfile: &str) -> Option<Self> {
        let docker = bollard::Docker::connect_with_defaults().ok()?;
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let network = format!("temps-test-secrets-{suffix}");
        let runtime = DockerRuntime::new(Arc::new(docker.clone()), true, network.clone());
        let context = TempDir::new().unwrap();
        std::fs::write(context.path().join("Dockerfile"), dockerfile).unwrap();
        Some(Self {
            docker,
            runtime,
            network,
            image: format!("temps-test-secrets:{suffix}"),
            context,
        })
    }

    fn request(
        &self,
        build_args: &[(&str, &str)],
        secrets: &[(&str, &str)],
        cache_from: Vec<String>,
    ) -> BuildRequest {
        BuildRequest {
            build_secrets: crate::BuildSecrets::new(
                secrets
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
            ),
            cache_from,
            image_name: self.image.clone(),
            context_path: self.context.path().to_path_buf(),
            dockerfile_path: None,
            build_args: build_args
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            build_args_buildkit: HashMap::new(),
            platform: None,
            log_path: self.context.path().join("build.log"),
        }
    }

    /// Build, returning the result and every line the log callback saw.
    async fn build(&self, request: BuildRequest) -> (Result<BuildResult, BuilderError>, String) {
        let lines = Arc::new(std::sync::Mutex::new(String::new()));
        let collected = lines.clone();
        let log_callback: crate::LogCallback = Arc::new(move |line: String| {
            let collected = collected.clone();
            Box::pin(async move {
                if let Ok(mut lines) = collected.lock() {
                    lines.push_str(&line);
                }
            })
        });
        let result = self
            .runtime
            .build_image_with_callback(crate::BuildRequestWithCallback {
                request,
                log_callback: Some(log_callback),
            })
            .await;
        let logged = lines.lock().map(|lines| lines.clone()).unwrap_or_default();
        (result, logged)
    }

    /// Whether `needle` is anywhere in the image's history or in the
    /// plain-text parts of `docker save` (the image config among them).
    async fn image_contains(&self, needle: &str) -> bool {
        let history = self.docker.image_history(&self.image).await.unwrap();
        if history
            .iter()
            .any(|layer| layer.created_by.contains(needle))
        {
            return true;
        }
        let saved: Vec<bytes::Bytes> = self
            .docker
            .export_image(&self.image)
            .try_collect()
            .await
            .unwrap();
        let saved = saved.concat();
        saved
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    async fn cleanup(&self) {
        let _ = self
            .docker
            .remove_image(
                &self.image,
                None::<bollard::query_parameters::RemoveImageOptions>,
                None,
            )
            .await;
        let _ = self.docker.remove_network(&self.network).await;
    }
}

/// Why #1725 exists: a value passed for a final-stage `ARG` is readable in
/// the image. If Docker ever stops doing this, the filtering can go.
#[tokio::test]
async fn final_stage_build_arg_value_is_kept_in_the_image() {
    let dockerfile = "FROM alpine:3.20\nARG API_KEY\nRUN echo building\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };
    let value = "final-stage-value-1725";
    let (result, logged) = fixture
        .build(fixture.request(&[("API_KEY", value)], &[], Vec::new()))
        .await;
    assert!(result.is_ok(), "{result:?}\n{logged}");
    assert!(fixture.image_contains(value).await);
    fixture.cleanup().await;
}

/// The other half of the issue's premise: a value used only in an earlier
/// stage stays out, so it is safe to keep passing it.
#[tokio::test]
async fn earlier_stage_build_arg_value_is_not_kept_in_the_image() {
    let dockerfile = "FROM alpine:3.20 AS build\nARG API_KEY\n\
                      RUN echo \"$API_KEY\" | wc -c > /length\n\
                      FROM alpine:3.20\nCOPY --from=build /length /length\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };
    let value = "earlier-stage-value-1725";
    let (result, logged) = fixture
        .build(fixture.request(&[("API_KEY", value)], &[], Vec::new()))
        .await;
    assert!(result.is_ok(), "{result:?}\n{logged}");
    assert!(!fixture.image_contains(value).await);
    fixture.cleanup().await;
}

/// A secret mount gets the value, build arguments and step output still
/// work on this path, and no trace of the value is left in the final stage.
#[tokio::test]
async fn secret_mount_serves_the_value_and_leaves_no_trace() {
    let value = "mounted-secret-value-1725";
    // Checks the value's length and ending: comparing it whole would write
    // it into the Dockerfile, and so into the history being inspected.
    let dockerfile = "FROM alpine:3.20\nARG PLAIN\n\
         RUN --mount=type=secret,id=API_KEY,env=API_KEY \
         sh -c 'test \"$(printf %s \"$API_KEY\" | wc -c)\" -eq 25 \
         && test \"$(printf %s \"$API_KEY\" | tail -c 4)\" = 1725 \
         && test \"$PLAIN\" = plain-1725 && echo secret-reached-the-step'\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };

    let (result, logged) = fixture
        .build(fixture.request(
            &[("PLAIN", "plain-1725")],
            &[("API_KEY", value)],
            Vec::new(),
        ))
        .await;
    let built = result.unwrap_or_else(|error| panic!("{error:?}\n{logged}"));
    assert!(built.size_bytes > 0);
    assert!(
        logged.contains("secret-reached-the-step"),
        "step output should stream to the log callback:\n{logged}"
    );
    assert!(!logged.contains(value), "value in the build log:\n{logged}");
    assert!(!fixture.image_contains(value).await);

    // A rebuild seeded from the image takes the cache path too.
    let (result, logged) = fixture
        .build(fixture.request(
            &[("PLAIN", "plain-1725")],
            &[("API_KEY", value)],
            vec![fixture.image.clone()],
        ))
        .await;
    assert!(result.is_ok(), "{result:?}\n{logged}");
    fixture.cleanup().await;
}

/// bollard's solve reports only an exit code, so a failing step's output
/// has to arrive through the status stream.
#[tokio::test]
async fn secret_mount_build_failure_reports_the_step_output() {
    let dockerfile = "FROM alpine:3.20\n\
         RUN --mount=type=secret,id=API_KEY sh -c 'echo why-the-step-failed; exit 7'\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };
    let (result, logged) = fixture
        .build(fixture.request(&[], &[("API_KEY", "unused-1725")], Vec::new()))
        .await;
    assert!(
        matches!(result, Err(BuilderError::BuildFailed(_))),
        "{result:?}"
    );
    assert!(logged.contains("why-the-step-failed"), "{logged}");
    assert!(logged.contains("ERROR: Build failed:"), "{logged}");
    assert!(logged.contains("exit code: 7"), "{logged}");
    // The gRPC status carries a Go stack trace that must not reach users.
    assert!(!logged.contains("github.com/moby"), "{logged}");
    fixture.cleanup().await;
}

/// Cancelling a deployment drops the build future. The solve runs on its own
/// thread, so it has to be told: a build left running would still export
/// the image.
#[tokio::test]
async fn dropping_a_secret_mount_build_cancels_it() {
    let dockerfile = "FROM alpine:3.20\n\
         RUN --mount=type=secret,id=API_KEY sleep 6\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };
    // Long enough for the step to start, far shorter than the step.
    let build = fixture.build(fixture.request(&[], &[("API_KEY", "unused-1725")], Vec::new()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), build)
            .await
            .is_err(),
        "the build should still be running"
    );
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    assert!(
        fixture.docker.inspect_image(&fixture.image).await.is_err(),
        "the cancelled build finished and exported its image"
    );
    fixture.cleanup().await;
}

#[test]
fn build_secret_files_are_private_and_removed_with_their_directory() {
    let secrets = crate::BuildSecrets::new([("API_KEY".to_string(), "value".to_string())].into());
    let (dir, sources) = write_build_secret_files(&secrets).unwrap();
    assert_eq!(sources.len(), 1);
    let (id, path) = &sources[0];
    assert_eq!(id, "API_KEY");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "value");
    assert!(
        !path.to_string_lossy().contains("API_KEY"),
        "file name should not reveal the secret: {path:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path), 0o600);
        assert_eq!(mode(dir.path()), 0o700);
    }
    let path = path.clone();
    drop(dir);
    assert!(!path.exists());
}

#[test]
fn oversized_build_secret_is_refused_by_name_without_its_value() {
    let secrets = crate::BuildSecrets::new(
        [("HUGE".to_string(), "x".repeat(MAX_BUILD_SECRET_BYTES + 1))].into(),
    );
    let Err(error) = write_build_secret_files(&secrets) else {
        panic!("an oversized secret should be refused");
    };
    let message = error.to_string();
    assert!(message.contains("HUGE"), "{message}");
    assert!(!message.contains("xxx"), "{message}");
}

#[test]
fn frontend_options_keep_the_dockerfile_path_arguments_and_platform() {
    let request = BuildRequest {
        build_secrets: Default::default(),
        cache_from: vec!["seed:latest".to_string()],
        image_name: "test".into(),
        context_path: PathBuf::from("/ctx"),
        dockerfile_path: Some(PathBuf::from("/ctx/docker/Dockerfile")),
        build_args: HashMap::from([("PLAIN".into(), "value".into())]),
        build_args_buildkit: HashMap::new(),
        platform: Some("linux/arm64/v8".into()),
        log_path: PathBuf::new(),
    };
    let options = bollard::grpc::build::ImageBuildFrontendOptions::builder()
        .dockerfile(Path::new("docker/Dockerfile"))
        .label("built-by", "temps")
        .buildarg("PLAIN", "value")
        .buildarg("BUILDKIT_INLINE_CACHE", "1")
        .cachefrom(
            &bollard::grpc::build::moby::buildkit::v1::CacheOptionsEntry {
                r#type: "registry".to_string(),
                attrs: HashMap::from([("ref".to_string(), "seed:latest".to_string())]),
            },
        )
        .platforms(&bollard::grpc::build::ImageBuildPlatform {
            os: "linux".to_string(),
            architecture: "arm64".to_string(),
            variant: Some("v8".to_string()),
        })
        .build();
    let actual = secret_mount_frontend_options(&request, "linux/amd64", &[]);
    // Argument order comes from a HashMap, so compare with a canonical form.
    let canonical = |options: &bollard::grpc::build::ImageBuildFrontendOptions| {
        let mut rendered: Vec<char> = format!("{options:?}").chars().collect();
        rendered.sort_unstable();
        rendered.into_iter().collect::<String>()
    };
    assert_eq!(canonical(&actual), canonical(&options));
}

#[test]
fn status_connection_uses_the_socket_bollard_connects_to() {
    assert_eq!(
        docker_unix_socket_from(None),
        Some(PathBuf::from("/var/run/docker.sock"))
    );
    assert_eq!(
        docker_unix_socket_from(Some("unix:///tmp/docker.sock")),
        Some(PathBuf::from("/tmp/docker.sock"))
    );
    assert_eq!(docker_unix_socket_from(Some("tcp://10.0.0.1:2375")), None);
}
