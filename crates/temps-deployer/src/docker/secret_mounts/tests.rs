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
            labels: Default::default(),
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

/// Dockerfiles Docker reads differently from how they look, each one keeping
/// the value in the image. The analysis that decides which secrets to withhold
/// has to name the argument in every one of them, or the value leaks.
#[tokio::test]
async fn final_image_analysis_names_every_argument_buildkit_keeps() {
    let value = "kept-value-1725";
    for (name, dockerfile) in [
        (
            "standalone continuation",
            "FROM alpine:3.20\nARG \\\n\\\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "standalone continuation with blanks",
            "FROM alpine:3.20\nARG \\\n\\  \nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "standalone continuation with CRLF",
            "FROM alpine:3.20\r\nARG \\\r\n\\\r\nAPI_KEY\r\nRUN echo UNIQUE\r\n",
        ),
        (
            "continuation before two carriage returns",
            "FROM alpine:3.20\nARG \\\r\r\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "standalone continuation with a backtick escape",
            "# escape=`\nFROM alpine:3.20\nARG `\n`\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "escape directive after an unknown directive",
            "# foo=bar\n# escape=`\nFROM alpine:3.20\nARG \\\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "escape directive after a malformed key",
            "# a b=c\n# escape=`\nFROM alpine:3.20\nARG \\\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "escape directive after an empty value",
            "# foo=\n# escape=`\nFROM alpine:3.20\nARG \\\nAPI_KEY\nRUN echo UNIQUE\n",
        ),
        (
            "double-quoted base stage",
            "FROM alpine:3.20 AS build\nARG API_KEY\nRUN echo UNIQUE\nFROM \"build\"\nRUN echo done\n",
        ),
        (
            "single-quoted base stage",
            "FROM alpine:3.20 AS build\nARG API_KEY\nRUN echo UNIQUE\nFROM 'build'\nRUN echo done\n",
        ),
        (
            "escaped base stage",
            "FROM alpine:3.20 AS build\nARG API_KEY\nRUN echo UNIQUE\nFROM bu\\ild\nRUN echo done\n",
        ),
    ] {
        // Unique, so a step found in BuildKit's cache cannot hide the value.
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let dockerfile = dockerfile.replace("UNIQUE", &unique);
        let Some(fixture) = SecretBuildFixture::new(&dockerfile).await else {
            return;
        };
        let (result, logged) = fixture
            .build(fixture.request(&[("API_KEY", value)], &[], Vec::new()))
            .await;
        assert!(result.is_ok(), "{name}: {result:?}\n{logged}");
        assert!(
            fixture.image_contains(value).await,
            "{name}: BuildKit no longer keeps the value"
        );
        let analysed = crate::build_protocol::dockerfile_final_image_arg_names(&dockerfile);
        assert!(
            analysed
                .as_ref()
                .is_none_or(|names| names.contains("API_KEY")),
            "{name}: the value is in the image but the analysis found {analysed:?}"
        );
        fixture.cleanup().await;
    }
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

/// Every way of naming a secret, with the mount required so that a name read
/// differently from BuildKit fails the build. What is served is exactly what
/// the Dockerfile parser found, as a real deployment does.
#[tokio::test]
async fn secret_mount_names_are_read_the_way_buildkit_reads_them() {
    // The first step is unique so every later one runs: BuildKit does not ask for
    // the secret of a step it finds in its cache.
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let dockerfile = format!(
        "FROM alpine:3.20\nRUN echo {unique}\n\
         RUN --mount=type=secret,source=BY_SOURCE,required=true test -s /run/secrets/BY_SOURCE\n\
         RUN --mount=type=secret,src=BY_SRC,required=true test -s /run/secrets/BY_SRC\n\
         RUN --mount=type=secret,source=RENAMED,target=/run/secrets/other,required=true \
         test -s /run/secrets/other\n\
         RUN --mount=type=secret,id=BY_ID,target=/run/secrets/unused,required=true \
         test -s /run/secrets/unused\n\
         RUN --mount=type=secret,target=/run/secrets/BY_TARGET,required=true \
         test -s /run/secrets/BY_TARGET\n\
         RUN --mount=type=secret,id=\"QUOTED_VALUE\",required=true \
         test -s /run/secrets/QUOTED_VALUE\n\
         RUN --mount=\"type=secret,id=QUOTED_FLAG,required=true\" \
         test -s /run/secrets/QUOTED_FLAG\n\
         RUN --mount='type=secret,source=SINGLE_QUOTED,required=true' \
         test -s /run/secrets/SINGLE_QUOTED\n\
         RUN --mount='type=secret,\"id=CSV_QUOTED\",required=true' \
         test -s /run/secrets/CSV_QUOTED\n\
         RUN --mount=\"type=secret,target=/run/secrets/with space/SPACED,required=true\" \
         test -s '/run/secrets/with space/SPACED'\n"
    );
    let Some(fixture) = SecretBuildFixture::new(&dockerfile).await else {
        return;
    };
    let ids = crate::build_protocol::dockerfile_secret_mount_ids(&dockerfile);
    let secrets: Vec<(&str, &str)> = ids.iter().map(|id| (id.as_str(), "value")).collect();
    let (result, logged) = fixture
        .build(fixture.request(&[], &secrets, Vec::new()))
        .await;
    assert!(result.is_ok(), "served {ids:?}: {result:?}\n{logged}");
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
        labels: Default::default(),
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

/// A build with secret mounts goes through BuildKit's solve, not `/build`;
/// its image must carry the same ownership labels.
#[test]
fn secret_mount_builds_carry_the_ownership_labels() {
    let request = BuildRequest {
        labels: HashMap::from([
            ("sh.temps.image.project_id".to_string(), "7".to_string()),
            ("built-by".to_string(), "someone-else".to_string()),
        ]),
        build_secrets: Default::default(),
        cache_from: Vec::new(),
        image_name: "app:latest".into(),
        context_path: PathBuf::from("/tmp/context"),
        dockerfile_path: None,
        build_args: HashMap::new(),
        build_args_buildkit: HashMap::new(),
        platform: Some("linux/amd64".into()),
        log_path: PathBuf::new(),
    };
    let rendered = format!(
        "{:?}",
        secret_mount_frontend_options(&request, "linux/amd64", &[])
    );
    assert!(
        rendered.contains(r#""sh.temps.image.project_id": "7""#),
        "{rendered}"
    );
    assert!(rendered.contains(r#""built-by": "temps""#), "{rendered}");
    assert!(!rendered.contains("someone-else"), "{rendered}");
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

/// Read from a fake daemon's end of the connection until `marker` has arrived.
async fn read_until(daemon: &mut tokio::net::UnixStream, marker: &[u8]) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    while !seen.windows(marker.len()).any(|window| window == marker) {
        let read = daemon.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "the client closed the connection early");
        seen.extend_from_slice(&chunk[..read]);
    }
    seen
}

/// A cancelled build drops its future and, with it, the follower. A daemon that
/// has stalled never ends the status call, so only the drop can stop the task.
#[tokio::test]
async fn dropping_the_follower_closes_a_stalled_status_connection() {
    use tokio::io::AsyncReadExt;
    const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("docker.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (events, _receiver_stays_open) = tokio::sync::mpsc::channel(1);

    let follower = spawn_status_follower(Some(socket), "build-ref".to_string(), events);

    let (mut daemon, _) = tokio::time::timeout(WAIT, listener.accept())
        .await
        .expect("the follower should connect")
        .unwrap();
    // Accept the upgrade, then say nothing more: the status call now waits for
    // an answer that never comes.
    let upgraded = async {
        read_until(&mut daemon, b"\r\n\r\n").await;
        daemon
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .await
            .unwrap();
        read_until(&mut daemon, b"PRI * HTTP/2.0").await;
    };
    tokio::time::timeout(WAIT, upgraded)
        .await
        .expect("the follower should speak HTTP/2 after the upgrade");

    drop(follower);

    let mut rest = Vec::new();
    tokio::time::timeout(WAIT, daemon.read_to_end(&mut rest))
        .await
        .expect("dropping the follower should close its connection")
        .expect("the closed connection should read to its end");
}

/// A solve talking to a fake daemon that upgrades both of its connections and
/// then goes quiet, as a stalled daemon does.
struct StalledSolve {
    cancel: tokio::sync::oneshot::Sender<()>,
    solve: tokio::task::JoinHandle<Result<(), String>>,
    /// The daemon's end of the control connection the solve request goes over.
    control: tokio::net::UnixStream,
    /// The daemon's end of the session connection bollard's detached task serves.
    session: tokio::net::UnixStream,
    _socket_dir: TempDir,
}

const SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl StalledSolve {
    async fn start() -> Self {
        use tokio::io::AsyncReadExt;
        let socket_dir = TempDir::new().unwrap();
        let socket = socket_dir.path().join("docker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let docker = bollard::Docker::connect_with_unix(
            socket.to_str().unwrap(),
            120,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let options = bollard::grpc::build::ImageBuildFrontendOptions::builder().build();
        let (cancel, solve) = spawn_solve(
            Arc::new(docker),
            "stalled:test".to_string(),
            options,
            bytes::Bytes::from_static(b"context"),
            bollard::grpc::BuildRef::random(),
        );
        let upgrade = |listener: tokio::net::UnixListener, path: &'static [u8]| async move {
            let (mut daemon, _) = listener.accept().await.unwrap();
            let head = read_until(&mut daemon, b"\r\n\r\n").await;
            assert!(
                head.windows(path.len()).any(|window| window == path),
                "expected a request for {}: {}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(&head)
            );
            daemon
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\n\
                      Upgrade: h2c\r\n\r\n",
                )
                .await
                .unwrap();
            (listener, daemon)
        };
        let (control, session, listener) = tokio::time::timeout(SESSION_WAIT, async {
            let (listener, control) = upgrade(listener, b"/grpc").await;
            let (listener, mut session) = upgrade(listener, b"/session").await;
            // The session server speaks first, so it is running from here on.
            let mut first = [0u8; 1];
            session.read_exact(&mut first).await.unwrap();
            (control, session, listener)
        })
        .await
        .expect("the solve should open its control and session connections");
        drop(listener);
        Self {
            cancel,
            solve,
            control,
            session,
            _socket_dir: socket_dir,
        }
    }
}

/// Wait for the client to close the session connection. A session server left
/// running keeps it open, along with the build context it holds.
async fn session_closes(session: &mut tokio::net::UnixStream) {
    use tokio::io::AsyncReadExt;
    let mut rest = Vec::new();
    tokio::time::timeout(SESSION_WAIT, session.read_to_end(&mut rest))
        .await
        .expect("the session connection should close, releasing the build context")
        .expect("the closed connection should read to its end");
}

/// bollard serves the session from a task it detaches, so dropping the solve
/// future alone would leave that task and the build context behind.
#[tokio::test]
async fn cancelling_a_solve_closes_its_session_on_a_stalled_daemon() {
    let mut stalled = StalledSolve::start().await;
    drop(stalled.cancel);
    session_closes(&mut stalled.session).await;
}

/// The same when the solve ends by itself: nothing in bollard closes the
/// session afterwards, and a daemon need not close it either.
#[tokio::test]
async fn a_solve_that_ends_closes_its_session() {
    let mut stalled = StalledSolve::start().await;
    drop(stalled.control);
    let outcome = tokio::time::timeout(SESSION_WAIT, &mut stalled.solve)
        .await
        .expect("the solve should end when its control connection does")
        .unwrap();
    assert!(outcome.is_err(), "{outcome:?}");
    session_closes(&mut stalled.session).await;
}

/// A healthy daemon does not close the session after a build either, so without
/// this every build would leave a task behind, holding its whole build context.
#[tokio::test]
async fn finished_builds_leave_no_session_task_behind() {
    let dockerfile = "FROM alpine:3.20\n\
         RUN --mount=type=secret,id=API_KEY,required=true test -s /run/secrets/API_KEY\n";
    let Some(fixture) = SecretBuildFixture::new(dockerfile).await else {
        return;
    };
    let metrics = tokio::runtime::Handle::current().metrics();
    let mut alive = Vec::new();
    for _ in 0..3 {
        let (result, logged) = fixture
            .build(fixture.request(&[], &[("API_KEY", "value")], Vec::new()))
            .await;
        assert!(result.is_ok(), "{result:?}\n{logged}");
        alive.push(metrics.num_alive_tasks());
    }
    assert!(
        alive[2] <= alive[0],
        "tasks alive after each build: {alive:?}"
    );
    fixture.cleanup().await;
}
