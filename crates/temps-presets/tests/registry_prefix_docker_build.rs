// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Proves the registry-mirror-prefix rewrite is not just a text transform:
//! the rewritten `FROM` line has to survive real BuildKit parsing and the
//! image actually has to resolve and pull through the configured prefix.
//!
//! Everything else touching this feature is a unit test against a string
//! (`temps-core::registry_prefix`, `temps-presets::registry_prefix`). This is
//! the one place that runs `docker build` against the rewritten output, the
//! same way `tests/starters.rs` runs a real build for the unprefixed case.

use std::collections::HashSet;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use temps_presets::{apply_registry_prefix, AutopackPreset, DockerfileConfig, Preset};
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;

/// `mirror.gcr.io` is a public, unauthenticated pull-through cache for
/// Docker Hub -- exactly the kind of target this doc/feature exists to
/// support, and reachable without any operator-specific credentials, so this
/// test can run anywhere `docker` can reach the network.
const PUBLIC_MIRROR: &str = "mirror.gcr.io";

fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn write_node_fixture(dir: &Path) {
    std::fs::write(
        dir.join("package.json"),
        r#"{"scripts":{"start":"node server.js"}}"#,
    )
    .expect("write package.json");
    std::fs::write(
        dir.join("server.js"),
        "require('http').createServer((_, res) => res.end('ok')).listen(process.env.PORT || 3000);\n",
    )
    .expect("write server.js");
}

fn render_dockerfile(dir: &Path) -> String {
    let mut config = DockerfileConfig::new(dir, dir, "registry-prefix-e2e");
    config.use_buildkit = true;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    runtime
        .block_on(AutopackPreset::new().dockerfile(config))
        .content
}

/// Feed `dockerfile` to `docker build` via stdin (never touches the fixture
/// tree) and return the captured output. Mirrors `tests/starters.rs::verify`.
fn docker_build(context_dir: &Path, tag: &str, dockerfile: &str) -> Result<(), String> {
    let mut build = Command::new("docker");
    build
        .args(["build", "--progress", "plain", "-t", tag, "-f", "-", "."])
        .current_dir(context_dir)
        .env("DOCKER_BUILDKIT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = build
        .spawn()
        .map_err(|e| format!("could not start docker build: {e}"))?;
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(dockerfile.as_bytes())
        .map_err(|e| format!("could not write the Dockerfile: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("docker build failed to complete: {e}"))?;

    if !output.status.success() {
        let log = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = log.lines().rev().take(40).collect();
        return Err(format!(
            "docker build failed\n--- build log (last 40 lines) ---\n{}\n--- Dockerfile ---\n{dockerfile}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(())
}

#[test]
fn a_registry_mirror_prefixed_dockerfile_actually_builds() {
    if !docker_available() {
        println!("Docker not available, skipping");
        return;
    }

    let fixture = tempfile::tempdir().expect("a temp dir");
    write_node_fixture(fixture.path());

    let dockerfile = render_dockerfile(fixture.path());
    assert!(
        !dockerfile.contains("autopack could not plan"),
        "the preset produced no build plan:\n{dockerfile}"
    );

    let rewritten = apply_registry_prefix(&dockerfile, PUBLIC_MIRROR);

    // Sanity-check the rewrite actually happened before spending a real
    // `docker build` proving it works -- a no-op rewrite would make the rest
    // of this test meaningless (it would just be re-running starters.rs).
    assert!(
        rewritten.contains(&format!("FROM {PUBLIC_MIRROR}/")),
        "expected at least one FROM line rewritten through {PUBLIC_MIRROR}, got:\n{rewritten}"
    );
    assert_ne!(
        dockerfile, rewritten,
        "prefix rewrite must change the Dockerfile"
    );

    let tag = "temps-registry-prefix-e2e:test";
    let result = docker_build(fixture.path(), tag, &rewritten);

    // Best-effort cleanup regardless of outcome.
    let _ = Command::new("docker")
        .args(["rmi", "-f", tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Err(e) = result {
        panic!("building the registry-mirror-prefixed Dockerfile failed: {e}");
    }
}

/// Extracts the docker-hub image references a `FROM {prefix}/...` rewrite
/// produced, so the test can seed exactly those images into the local
/// registry before building -- rather than guessing what autopack chose.
fn images_rewritten_through(rewritten: &str, prefix: &str) -> HashSet<String> {
    let needle = format!("FROM {prefix}/");
    rewritten
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix(&needle)?;
            // A stage reference is "<image> AS <name>" -- keep only the image.
            rest.split_whitespace().next().map(str::to_string)
        })
        .collect()
}

fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn docker(args: &[&str]) -> Result<(), String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| format!("could not run docker {args:?}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Same proof as above, but against a real *self-hosted* registry rather
/// than a public mirror -- this is the scenario the feature actually exists
/// for (an operator's own path-prefixing registry proxy, not Docker's own
/// pull-through cache infrastructure), and it runs fully offline once the
/// base image is cached, with no dependency on a third-party host being up.
#[test]
fn a_registry_mirror_prefixed_dockerfile_builds_against_a_self_hosted_registry() {
    if !docker_available() {
        println!("Docker not available, skipping");
        return;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");

    let port = match runtime.block_on(async {
        let container = GenericImage::new("registry", "2").start().await?;
        let port = container.get_host_port_ipv4(5000).await?;
        // `ContainerAsync::drop` schedules async cleanup on the current Tokio
        // reactor; by the time this function returns, the `current_thread`
        // runtime above is no longer entered, so a normal drop panics with
        // "no reactor running". Leak it instead -- the test process exits
        // right after, same workaround already used by
        // `temps-providers/tests/role_reconciler_integration.rs`.
        Box::leak(Box::new(container));
        Ok::<_, testcontainers::TestcontainersError>(port)
    }) {
        Ok(port) => port,
        Err(e) => {
            println!("could not start a local registry container, skipping: {e}");
            return;
        }
    };

    if !wait_for_tcp(port, Duration::from_secs(15)) {
        println!("local registry never accepted connections, skipping");
        return;
    }
    let prefix = format!("localhost:{port}");

    let fixture = tempfile::tempdir().expect("a temp dir");
    write_node_fixture(fixture.path());
    let dockerfile = render_dockerfile(fixture.path());
    assert!(
        !dockerfile.contains("autopack could not plan"),
        "the preset produced no build plan:\n{dockerfile}"
    );

    let rewritten = apply_registry_prefix(&dockerfile, &prefix);
    assert_ne!(
        dockerfile, rewritten,
        "prefix rewrite must change the Dockerfile"
    );

    let images = images_rewritten_through(&rewritten, &prefix);
    assert!(
        !images.is_empty(),
        "expected at least one FROM line rewritten through {prefix}, got:\n{rewritten}"
    );

    // Seed the local registry with exactly the images the rewrite expects to
    // find there -- mirroring what an operator's own registry proxy would
    // already have cached/proxied.
    for image in &images {
        if let Err(e) = docker(&["pull", image]) {
            println!("could not pull {image} to seed the local registry, skipping: {e}");
            return;
        }
        let local_ref = format!("{prefix}/{image}");
        docker(&["tag", image, &local_ref]).expect("tag for local registry push");
        docker(&["push", &local_ref]).expect("push to local registry");
    }

    let tag = "temps-registry-prefix-e2e-self-hosted:test";
    let result = docker_build(fixture.path(), tag, &rewritten);

    let _ = Command::new("docker")
        .args(["rmi", "-f", tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Err(e) = result {
        panic!("building against the self-hosted registry mirror failed: {e}");
    }
}
