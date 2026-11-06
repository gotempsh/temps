// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::docker::static_archive::unpack_static_archive_stream;
use crate::docker::test_support::{
    create_test_docker_runtime, runtime_for_diagnosis, test_runtime,
};
use crate::docker::DockerRuntime;
use crate::BuildRequest;
use serial_test::serial;
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::fs;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn local_context_stream_filters_ignored_files_and_preserves_exceptions() {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in [
        ("Dockerfile", "FROM scratch"),
        (
            ".dockerignore",
            "Dockerfile\nnode_modules\ndocs\n!docs/keep.txt\n.git\n",
        ),
        ("node_modules/huge.bin", "excluded"),
        ("docs/keep.txt", "keep"),
        ("docs/drop.txt", "drop"),
        (".git/config", "private"),
        ("src/main.ts", "source"),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let rt = runtime_for_diagnosis(true);
    let mut stream = rt
        .create_tar_context_body(dir.path().to_path_buf(), None)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    let mut archive = tar::Archive::new(bytes.as_slice());
    let paths: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect();
    assert!(paths.contains(&PathBuf::from("Dockerfile")));
    assert!(paths.contains(&PathBuf::from("docs/keep.txt")));
    assert!(paths.contains(&PathBuf::from("src/main.ts")));
    assert!(!paths.contains(&PathBuf::from("node_modules/huge.bin")));
    assert!(!paths.contains(&PathBuf::from("docs/drop.txt")));
    assert!(!paths.contains(&PathBuf::from(".git/config")));
}

#[cfg(unix)]
#[tokio::test]
async fn local_context_preserves_symlinks_and_dockerfile_specific_ignore() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("docker")).unwrap();
    std::fs::write(dir.path().join("docker/Buildfile"), "FROM scratch").unwrap();
    std::fs::write(dir.path().join(".dockerignore"), "keep.txt").unwrap();
    std::fs::write(dir.path().join("docker/Buildfile.dockerignore"), "drop.txt").unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep").unwrap();
    std::fs::write(dir.path().join("drop.txt"), "drop").unwrap();
    std::os::unix::fs::symlink("keep.txt", dir.path().join("link")).unwrap();
    let rt = runtime_for_diagnosis(true);
    let mut stream = rt
        .create_tar_context_body(
            dir.path().to_path_buf(),
            Some(dir.path().join("docker/Buildfile")),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    let mut archive = tar::Archive::new(bytes.as_slice());
    let mut paths = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        if path == Path::new("link") {
            assert!(entry.header().entry_type().is_symlink());
        }
        paths.push(path);
    }
    assert!(paths.contains(&PathBuf::from("keep.txt")));
    assert!(paths.contains(&PathBuf::from("link")));
    assert!(!paths.contains(&PathBuf::from("drop.txt")));
}

#[tokio::test]
async fn local_workspace_archive_intersects_root_and_specific_rules() {
    let source = tempfile::tempdir().unwrap();
    let root = source.path();
    for directory in ["apps/web", "private-data", "app-private", "src", "docs"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for file in [
        "apps/web/Dockerfile",
        "private-data/secret",
        "app-private/secret",
        "src/keep.ts",
        "src/drop.ts",
        "docs/readme",
    ] {
        std::fs::write(root.join(file), "fixture").unwrap();
    }
    std::fs::write(
        root.join(".dockerignore"),
        "private-data\nsrc\n!src/keep.ts\n!app-private\n",
    )
    .unwrap();
    std::fs::write(
        root.join("apps/web/Dockerfile.dockerignore"),
        format!(
            "{}\napp-private\n!private-data\n!src\n",
            crate::build_protocol::WORKSPACE_ROOT_IGNORE_MARKER
        ),
    )
    .unwrap();
    let rt = runtime_for_diagnosis(true);
    let mut stream = rt
        .create_tar_context_body(root.to_path_buf(), Some(root.join("apps/web/Dockerfile")))
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    let mut archive = tar::Archive::new(bytes.as_slice());
    let paths: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect();
    for kept in [
        "apps/web/Dockerfile",
        "apps/web/Dockerfile.dockerignore",
        ".dockerignore",
        "src/keep.ts",
        "docs/readme",
    ] {
        assert!(
            paths.contains(&PathBuf::from(kept)),
            "missing {kept}: {paths:?}"
        );
    }
    for excluded in ["private-data/secret", "app-private/secret", "src/drop.ts"] {
        assert!(
            !paths.contains(&PathBuf::from(excluded)),
            "exposed {excluded}"
        );
    }
}

#[test]
fn buildkit_arguments_extend_user_arguments() {
    let request = BuildRequest {
        build_secrets: Default::default(),
        cache_from: Vec::new(),
        image_name: "test".into(),
        context_path: PathBuf::new(),
        dockerfile_path: None,
        platform: None,
        log_path: PathBuf::new(),
        build_args: HashMap::from([
            ("NEXT_PUBLIC_URL".into(), "https://example.test".into()),
            ("OVERRIDE".into(), "user".into()),
        ]),
        build_args_buildkit: HashMap::from([
            ("BUILDKIT_CACHE_MOUNT_NS".into(), "isolated".into()),
            ("OVERRIDE".into(), "builder".into()),
        ]),
    };
    let args = docker_build_args(&request, true);
    assert_eq!(args["NEXT_PUBLIC_URL"], "https://example.test");
    assert_eq!(args["BUILDKIT_CACHE_MOUNT_NS"], "isolated");
    assert_eq!(args["OVERRIDE"], "builder");
    assert_eq!(args["BUILDKIT_INLINE_CACHE"], "1");
    assert_eq!(docker_build_args(&request, false), request.build_args);
}

#[test]
fn image_tag_split_preserves_registry_ports() {
    assert_eq!(
        split_repository_and_tag("registry.example:5000/team/app:v2"),
        ("registry.example:5000/team/app", "v2")
    );
    assert_eq!(
        split_repository_and_tag("registry.example:5000/team/app"),
        ("registry.example:5000/team/app", "latest")
    );
}

/// End-to-end reproduction of the production incident this change
/// fixes: the per-node Hickory resolver becomes unreachable, and a
/// container that only had it as `nameserver` hung on every DNS query
/// (`wget api.stripe.com` never returning). Uses a real container
/// against the local Docker daemon — no mocks — because the bug lived
/// in what Docker actually writes to `/etc/resolv.conf`, not in our
/// in-memory list-building logic (already covered by the pure
/// `dns_for_container` tests above and `temps_node_dns::host_dns`).
/// The worker side of static extraction: a path is streamed out of a
/// local image as a container-archive tar the control plane can unpack.
#[tokio::test]
async fn export_path_stream_round_trips_through_the_static_unpacker() {
    let runtime = match create_test_docker_runtime().await {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("🔧 Docker not available, skipping: {error}");
            return;
        }
    };
    let reachable = match runtime.require_docker_for_build() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    };
    if !reachable {
        println!("🔧 Docker not available, skipping");
        return;
    }

    let stream = runtime
        .export_path_stream("alpine:latest", "/etc/apk")
        .await
        .expect("export /etc/apk from alpine:latest");
    let workspace = TempDir::new().expect("workspace");
    let destination = workspace.path().join("apk");
    unpack_static_archive_stream(stream, "alpine:latest", "/etc/apk", &destination)
        .await
        .expect("exported archive unpacks under the static limits");
    assert!(destination.join("repositories").is_file());

    let missing = runtime
        .export_path_stream("temps-definitely-missing:never", "/app/dist")
        .await;
    assert!(
        matches!(missing, Err(BuilderError::ImageNotFound(_))),
        "a missing image must not be pulled or reported as a generic failure"
    );
}

#[test]
fn test_native_platform_detection() {
    // Before `refresh_daemon_platform` runs, detection falls back to the
    // architecture this binary was compiled for.
    let platform = test_runtime().detect_native_platform();

    // Verify platform format
    assert!(platform.starts_with("linux/"));

    // Verify it matches the current architecture
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(platform, "linux/amd64");
        println!("✅ Detected x86_64 platform: {}", platform);
    }

    #[cfg(target_arch = "aarch64")]
    {
        assert_eq!(platform, "linux/arm64");
        println!("✅ Detected ARM64 platform: {}", platform);
    }

    // Verify platform is one of the supported architectures
    assert!(
        platform == "linux/amd64" || platform == "linux/arm64",
        "Platform should be either linux/amd64 or linux/arm64, got: {}",
        platform
    );
}

/// A failed lookup must NOT be cached. It used to store the compiled-in
/// fallback in the `OnceLock`, so one transient `docker info` error at
/// boot froze the wrong architecture for the process lifetime — with a
/// cross-architecture `DOCKER_HOST`, every later build and scheduling
/// decision then used the binary's architecture even after the daemon
/// recovered, defeating the very checks this is here to feed.
#[tokio::test]
async fn test_failed_platform_lookup_is_not_cached() {
    // Port 1 is reserved and refuses connections: a client that can never
    // reach a daemon, which is what a boot-time failure looks like.
    let unreachable =
        Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
            .expect("client construction makes no connection");
    let runtime = DockerRuntime::new(Arc::new(unreachable), false, "test-network".to_string());

    assert_eq!(runtime.refresh_daemon_platform().await, None);
    // Still uncached, so a later call retries instead of returning a
    // stale guess.
    assert_eq!(runtime.refresh_daemon_platform().await, None);
    // The synchronous accessor keeps answering with the binary's platform
    // meanwhile — a fallback, not a recorded fact.
    assert_eq!(
        runtime.get_native_platform(),
        crate::platform::native_platform()
    );
}

/// `remove_image` used to build its request future and drop it, so it
/// removed nothing and still returned `Ok(())`. Tests that clean up after
/// themselves depended on a call that did nothing.
#[tokio::test]
async fn test_remove_image_actually_removes_and_reports_failures() {
    let runtime = test_runtime();
    let Some(raw_docker) = runtime.docker.get() else {
        println!("Docker handle not available, skipping");
        return;
    };
    if raw_docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }
    let raw_docker = raw_docker.clone();

    // Removing something that isn't there must be an error, not a silent
    // success — that silent success is exactly the bug.
    let missing = format!("temps-remove-test-{}:latest", uuid::Uuid::new_v4());
    assert!(
        runtime.remove_image(&missing).await.is_err(),
        "removing a non-existent image must fail"
    );

    // And a real image must be gone afterwards.
    let Ok(info) = runtime.inspect_image("alpine:3.20").await else {
        println!("alpine:3.20 not present locally, skipping the positive case");
        return;
    };
    let tag = format!("temps-remove-test-{}:latest", uuid::Uuid::new_v4());
    if raw_docker
        .tag_image(
            &info.id,
            Some(bollard::query_parameters::TagImageOptions {
                repo: tag.split(':').next().map(|r| r.to_string()),
                tag: Some("latest".to_string()),
            }),
        )
        .await
        .is_err()
    {
        println!("Could not tag a test image, skipping the positive case");
        return;
    }

    assert!(runtime.inspect_image(&tag).await.is_ok());
    runtime
        .remove_image(&tag)
        .await
        .expect("remove should work");
    assert!(
        runtime.inspect_image(&tag).await.is_err(),
        "the tag must be gone after remove_image"
    );
}

#[tokio::test]
#[serial]
async fn test_docker_build_with_dockerfile() {
    let temp_dir = TempDir::new().unwrap();
    let context_path = temp_dir.path().to_path_buf();

    // Create a simple Dockerfile
    let dockerfile = r#"FROM alpine:latest
RUN echo "Hello from Docker test" > /hello.txt
CMD ["cat", "/hello.txt"]
"#;
    fs::write(context_path.join("Dockerfile"), dockerfile)
        .await
        .unwrap();

    match create_test_docker_runtime().await {
        Ok(runtime) => {
            let request = BuildRequest {
                build_secrets: Default::default(),
                cache_from: Vec::new(),
                image_name: "docker-test:latest".to_string(),
                context_path,
                dockerfile_path: None,
                build_args: HashMap::new(),
                build_args_buildkit: HashMap::new(),
                platform: None,
                log_path: temp_dir.path().join("build.log"),
            };

            let result = timeout(Duration::from_secs(60), runtime.build_image(request)).await;

            match result {
                Ok(Ok(build_result)) => {
                    println!("✅ Docker build succeeded: {}", build_result.image_name);
                    assert_eq!(build_result.image_name, "docker-test:latest");
                    assert!(build_result.build_duration_ms > 0);
                }
                Ok(Err(e)) => {
                    println!("🔧 Docker build failed (may be expected): {}", e);
                }
                Err(_) => {
                    println!("⏰ Docker build timed out");
                }
            }
        }
        Err(e) => {
            println!("🔧 Docker not available: {}", e);
        }
    }
}

#[tokio::test]
async fn test_image_operations() {
    match create_test_docker_runtime().await {
        Ok(runtime) => {
            // Test list images
            match runtime.list_images().await {
                Ok(images) => {
                    println!("✅ Found {} images", images.len());
                    // Don't assert specific count as it depends on system state
                }
                Err(e) => {
                    println!("🔧 Failed to list images: {}", e);
                }
            }

            // Test remove non-existent image
            match runtime
                .remove_image("definitely-does-not-exist:latest")
                .await
            {
                Ok(()) => println!("⚠️ Unexpectedly succeeded removing non-existent image"),
                Err(e) => println!("✅ Correctly failed to remove non-existent image: {}", e),
            }
        }
        Err(e) => {
            println!("🔧 Docker not available: {}", e);
        }
    }
}

/// Archive `root` with [`append_local_build_context`] and return every
/// entry's path, header and contents.
fn archive_build_context(root: &std::path::Path) -> Vec<(String, tar::Header, Vec<u8>)> {
    use std::io::Read;
    let mut archive = tar::Builder::new(Vec::new());
    let filter = crate::remote::load_context_filter(root, std::path::Path::new("Dockerfile"))
        .expect("load the context filter");
    append_local_build_context(&mut archive, root, std::path::Path::new(""), &filter)
        .expect("archive the build context");
    let bytes = archive.into_inner().expect("finish the archive");
    let mut reader = tar::Archive::new(bytes.as_slice());
    reader
        .entries()
        .expect("read the archive")
        .map(|entry| {
            let mut entry = entry.expect("archive entry");
            let path = entry.path().expect("entry path").display().to_string();
            let header = entry.header().clone();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).expect("entry contents");
            (path, header, contents)
        })
        .collect()
}

/// A git checkout in the shape Temps clones it: sources plus `.git`.
fn checkout_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(root.join("cmd/app")).unwrap();
    std::fs::write(root.join("cmd/app/main.go"), "package main\n").unwrap();
    std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let script = root.join("build.sh");
    std::fs::write(&script, "#!/bin/sh\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

#[test]
fn build_context_entries_are_owned_by_root() {
    let checkout = checkout_fixture();

    let entries = archive_build_context(checkout.path());

    assert!(!entries.is_empty());
    for (path, header, _) in &entries {
        assert_eq!(header.uid().unwrap(), 0, "{path} must be owned by uid 0");
        assert_eq!(header.gid().unwrap(), 0, "{path} must be owned by gid 0");
    }
}

#[test]
fn build_context_keeps_git_metadata_sources_and_modes() {
    let checkout = checkout_fixture();

    let entries = archive_build_context(checkout.path());
    let find = |wanted: &str| {
        entries
            .iter()
            .find(|(path, _, _)| path.trim_end_matches('/') == wanted)
            .unwrap_or_else(|| {
                panic!(
                    "{wanted} missing from {entries:?}",
                    entries = entries.iter().map(|e| &e.0).collect::<Vec<_>>()
                )
            })
    };

    let (_, head, contents) = find(".git/HEAD");
    assert_eq!(contents, b"ref: refs/heads/main\n");
    assert_eq!(head.entry_type(), tar::EntryType::Regular);
    assert_eq!(
        find(".git/refs/heads").1.entry_type(),
        tar::EntryType::Directory
    );
    assert_eq!(find("cmd/app/main.go").2, b"package main\n");
    assert_eq!(find("build.sh").1.mode().unwrap() & 0o777, 0o755);
    // Paths are relative to the context root, never absolute.
    assert!(entries.iter().all(|(path, _, _)| !path.starts_with('/')));
}

#[test]
fn build_context_archives_symlinks_as_root_owned_links() {
    let checkout = checkout_fixture();
    std::os::unix::fs::symlink("cmd/app/main.go", checkout.path().join("main.go")).unwrap();

    let entries = archive_build_context(checkout.path());
    let (_, header, _) = entries
        .iter()
        .find(|(path, _, _)| path == "main.go")
        .expect("symlink archived");

    assert_eq!(header.entry_type(), tar::EntryType::Symlink);
    assert_eq!(
        header.link_name().unwrap().unwrap(),
        std::path::Path::new("cmd/app/main.go")
    );
    assert_eq!(header.uid().unwrap(), 0);
    assert_eq!(header.gid().unwrap(), 0);
}

#[test]
fn build_context_order_is_independent_of_directory_iteration() {
    let checkout = checkout_fixture();

    let first: Vec<String> = archive_build_context(checkout.path())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    let second: Vec<String> = archive_build_context(checkout.path())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();

    assert_eq!(first, second);
}

#[test]
fn build_context_keeps_a_dangling_symlink_without_reading_through_it() {
    let checkout = checkout_fixture();
    std::os::unix::fs::symlink("does-not-exist", checkout.path().join("broken")).unwrap();

    let entries = archive_build_context(checkout.path());
    let (_, header, _) = entries
        .iter()
        .find(|(path, _, _)| path == "broken")
        .expect("dangling symlink archived as a link");

    assert_eq!(header.entry_type(), tar::EntryType::Symlink);
}

#[test]
fn build_context_omits_git_metadata_only_when_ignored() {
    let checkout = checkout_fixture();
    std::fs::write(checkout.path().join(".dockerignore"), ".git\n").unwrap();

    let entries = archive_build_context(checkout.path());

    assert!(entries
        .iter()
        .all(|(path, _, _)| !path.starts_with(".git/") && path.trim_end_matches('/') != ".git"));
    assert!(entries.iter().any(|(path, _, _)| path == "cmd/app/main.go"));
}

/// End to end against a real BuildKit daemon: a `COPY` of the context
/// must land root-owned in the build stage, which is what lets git (and
/// `go build`'s VCS stamping) use a copied `.git`. Before the fix the
/// files kept the checkout owner's uid and git exited 128.
#[tokio::test]
#[serial]
async fn buildkit_build_stage_sees_root_owned_context() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            println!("Docker not available, skipping: {}", e);
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker ping failed, skipping");
        return;
    }

    let checkout = checkout_fixture();
    std::fs::write(
        checkout.path().join("Dockerfile"),
        "FROM alpine:3.20\nCOPY . /src\n\
         RUN test \"$(stat -c %u:%g /src/.git/HEAD)\" = 0:0 \\\n \
         && test \"$(stat -c %u:%g /src/cmd/app/main.go)\" = 0:0\n",
    )
    .unwrap();

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let network = format!("temps-test-ctx-owner-{suffix}");
    let image = format!("temps-test-ctx-owner:{suffix}");
    let runtime = DockerRuntime::new(Arc::new(docker.clone()), true, network.clone());
    let log_dir = TempDir::new().unwrap();
    let request = BuildRequest {
        build_secrets: Default::default(),
        cache_from: Vec::new(),
        image_name: image.clone(),
        context_path: checkout.path().to_path_buf(),
        dockerfile_path: None,
        build_args: HashMap::new(),
        build_args_buildkit: HashMap::new(),
        platform: None,
        log_path: log_dir.path().join("build.log"),
    };

    let result = timeout(Duration::from_secs(300), runtime.build_image(request)).await;

    let _ = runtime.remove_image(&image).await;
    let _ = docker.remove_network(&network).await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            let log = std::fs::read_to_string(log_dir.path().join("build.log")).unwrap_or_default();
            panic!("build with a root-owned context failed: {e}\n{log}");
        }
        Err(_) => panic!("BuildKit build timed out"),
    }
}

/// A daemon that sends no response headers until it has finished with the
/// request, as `POST /images/load` does while it unpacks layers. Answers each
/// request only after `header_delay`.
async fn slow_image_archive_daemon(header_delay: Duration) -> Docker {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 8192];
                let head_end = loop {
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end;
                    }
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                };
                let head = String::from_utf8_lossy(&request[..head_end]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body: &[u8] = if path.contains("/images/load") {
                    while !request.ends_with(b"0\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    tokio::time::sleep(header_delay).await;
                    b"{\"stream\":\"Loaded image: sha256:abc123\\n\"}\n"
                } else if path.contains("/get") {
                    tokio::time::sleep(header_delay).await;
                    b"image-archive-bytes"
                } else {
                    b"{}"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(body).await;
            });
        }
    });
    // A client timeout shorter than the daemon's delay stands in for bollard's
    // 120 second default, so the test does not have to wait two minutes.
    Docker::connect_with_http(
        &format!("http://{address}"),
        1,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap()
}

/// Regression test for issue #1781.
#[tokio::test]
async fn image_import_outlasts_the_default_client_timeout() {
    let docker = slow_image_archive_daemon(Duration::from_secs(2)).await;
    let archive: ImageImportStream = Box::pin(futures::stream::once(async {
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"image-archive-bytes"))
    }));

    let image_id = import_stream_into_docker(&docker, archive, "temps.internal/app:immutable")
        .await
        .expect("a slow import must not hit the short client timeout");

    assert_eq!(image_id, "sha256:abc123");
}

/// The same limit applies to `GET /images/{name}/get`.
#[tokio::test]
async fn image_save_outlasts_the_default_client_timeout() {
    let docker = slow_image_archive_daemon(Duration::from_secs(2)).await;
    let runtime = DockerRuntime::new(Arc::new(docker), false, "temps-test-network".to_string());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("image.tar");

    runtime
        .save_image("temps.internal/app:immutable", &output)
        .await
        .expect("a slow export must not hit the short client timeout");

    assert_eq!(std::fs::read(&output).unwrap(), b"image-archive-bytes");
}

/// A worker streams an export back to the control plane through this path.
#[tokio::test]
async fn image_export_stream_outlasts_the_default_client_timeout() {
    let docker = slow_image_archive_daemon(Duration::from_secs(2)).await;
    let runtime = DockerRuntime::new(Arc::new(docker), false, "temps-test-network".to_string());

    let mut stream = runtime
        .export_image_stream("temps.internal/app:immutable")
        .await
        .expect("the image exists");
    let mut archive = Vec::new();
    while let Some(chunk) = stream.next().await {
        archive.extend_from_slice(&chunk.expect("a slow export must not time out"));
    }

    assert_eq!(archive, b"image-archive-bytes");
}

/// Only the archive request gets the long limit; a hung daemon must still fail
/// every other call on the shared client within its normal timeout.
#[test]
fn image_archive_client_leaves_the_shared_client_timeout_alone() {
    let docker =
        Docker::connect_with_http("http://127.0.0.1:9", 120, bollard::API_DEFAULT_VERSION).unwrap();

    let archive_client = image_archive_client(&docker);

    assert_eq!(archive_client.timeout(), IMAGE_ARCHIVE_CLIENT_TIMEOUT);
    assert_eq!(docker.timeout(), Duration::from_secs(120));
}

/// A daemon that answers an image inspect with `inspect` and an image list
/// with `list`, as JSON.
async fn image_store_daemon(inspect: serde_json::Value, list: serde_json::Value) -> Docker {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let inspect = inspect.to_string();
    let list = list.to_string();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (inspect, list) = (inspect.clone(), list.clone());
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&request).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = if path.contains("/images/json") {
                    list
                } else {
                    inspect
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    Docker::connect_with_http(
        &format!("http://{address}"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap()
}

fn manifest(
    os: &str,
    arch: &str,
    variant: Option<&str>,
    kind: &str,
    available: bool,
) -> serde_json::Value {
    let mut platform = serde_json::json!({"os": os, "architecture": arch});
    if let Some(variant) = variant {
        platform["variant"] = serde_json::json!(variant);
    }
    serde_json::json!({
        "ID": format!("sha256:{arch}{}", variant.unwrap_or_default()),
        "Descriptor": {"mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:00", "size": 1},
        "Available": available,
        "Size": {"Total": 1, "Content": 1},
        "Kind": kind,
        "ImageData": {"Platform": platform, "Containers": [], "Size": {"Unpacked": 1}},
    })
}

fn summary(id: &str, manifests: Option<Vec<serde_json::Value>>) -> serde_json::Value {
    let mut summary = serde_json::json!({
        "Id": id, "ParentId": "", "RepoTags": ["app:1"], "RepoDigests": [],
        "Created": 0, "Size": 1, "SharedSize": -1, "Labels": {}, "Containers": -1,
    });
    if let Some(manifests) = manifests {
        summary["Manifests"] = serde_json::json!(manifests);
    }
    summary
}

const INSPECTED_ID: &str = "sha256:1111";

fn inspected_amd64() -> serde_json::Value {
    serde_json::json!({"Id": INSPECTED_ID, "Architecture": "amd64", "Os": "linux", "RepoTags": ["app:1"]})
}

/// #1789: a multi-arch image with every architecture pulled locally is
/// schedulable on each of them, not only on the daemon's own.
#[tokio::test]
async fn image_platforms_lists_every_locally_available_architecture() {
    let docker = image_store_daemon(
        inspected_amd64(),
        serde_json::json!([summary(
            INSPECTED_ID,
            Some(vec![
                manifest("linux", "amd64", None, "image", true),
                manifest("linux", "arm64", Some("v8"), "image", true),
                // Pulled for amd64 and arm64 only: listed, not runnable here.
                manifest("linux", "arm", Some("v7"), "image", false),
                // Build provenance is not a platform.
                manifest("unknown", "unknown", None, "attestation", true),
            ]),
        )]),
    )
    .await;
    let runtime = DockerRuntime::new(Arc::new(docker), false, "temps-test-network".to_string());

    let platforms = runtime.image_platforms("app:1").await.unwrap();

    assert_eq!(platforms.len(), 2, "{platforms:?}");
    for node in ["linux/amd64", "linux/arm64"] {
        assert!(
            platforms
                .iter()
                .any(|platform| crate::platform::platforms_match(platform, node)),
            "a {node} node must be eligible: {platforms:?}"
        );
    }
    assert!(
        !platforms
            .iter()
            .any(|platform| crate::platform::platforms_match(platform, "linux/arm/v7")),
        "an architecture whose content is not local is not runnable: {platforms:?}"
    );
}

/// The classic image store lists no manifests: one platform, as before.
#[tokio::test]
async fn image_platforms_falls_back_to_the_inspected_platform() {
    for list in [
        serde_json::json!([summary(INSPECTED_ID, None)]),
        serde_json::json!([summary(INSPECTED_ID, Some(vec![]))]),
        // A summary for some other image under the same reference is not this one.
        serde_json::json!([summary(
            "sha256:other",
            Some(vec![manifest("linux", "arm64", None, "image", true)]),
        )]),
        serde_json::json!([]),
    ] {
        let docker = image_store_daemon(inspected_amd64(), list.clone()).await;
        let runtime = DockerRuntime::new(Arc::new(docker), false, "temps-test-network".to_string());
        assert_eq!(
            runtime.image_platforms("app:1").await.unwrap(),
            ["linux/amd64"],
            "{list}"
        );
    }
}
