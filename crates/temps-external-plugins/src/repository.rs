// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a plugin's repository address may look like, and the container build
//! a worker's agent still answers.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

pub(crate) const BUILDER_IMAGE: &str =
    "oven/bun@sha256:4f6e31d1a54d6a3dd312daef655fc998101b5043d52e12592ac293ef04b9bc73";
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const BUILD_TIMEOUT: Duration = Duration::from_secs(300);
/// The largest executable a build may produce.
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
// Source (up to 128 MiB), dependency cache, and a compiled binary (up to
// 256 MiB) share /work. Leave headroom for Bun and keep total tmpfs below
// the container's 1536 MiB memory+swap ceiling.
const BUILD_CONTAINER_ARGS: &[&str] = &[
    "create",
    "--network",
    "bridge",
    "--cpus",
    "1",
    "--memory",
    "1536m",
    "--memory-swap",
    "1536m",
    "--pids-limit",
    "128",
    "--read-only",
    "--log-driver",
    "none",
    "--user",
    "0:0",
    "--security-opt",
    "no-new-privileges",
    "--cap-drop",
    "ALL",
    "--tmpfs",
    "/tmp:rw,noexec,nosuid,size=128m",
    "--tmpfs",
    "/work:rw,nosuid,size=768m",
    "--env",
    "HOME=/work/home",
    "--env",
    "XDG_CACHE_HOME=/work/cache",
    "--env",
    "BUN_INSTALL_CACHE_DIR=/work/cache/bun",
    "--env",
    "TMPDIR=/tmp",
    "--workdir",
    "/work",
    "--entrypoint",
    "/bin/sh",
    BUILDER_IMAGE,
    "-c",
    "sleep 660",
];

/// The container engine that compiles a plugin. Docker is preferred so a host
/// that has always built with Docker keeps doing exactly that; Podman is the
/// daemonless engine a hosted control plane carries instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerRuntime {
    Docker,
    Podman,
}

impl ContainerRuntime {
    const PREFERENCE: [Self; 2] = [Self::Docker, Self::Podman];

    fn program(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }

    /// The pinned image, spelled so the engine resolves it without a search
    /// registry: Podman has no implicit `docker.io`.
    fn builder_image(self) -> String {
        match self {
            Self::Docker => BUILDER_IMAGE.to_string(),
            Self::Podman => format!("docker.io/{BUILDER_IMAGE}"),
        }
    }

    /// The network the build container is attached to, which is what gets
    /// disconnected before attacker-controlled source is compiled.
    fn build_network(self) -> &'static str {
        match self {
            Self::Docker => "bridge",
            Self::Podman => "podman",
        }
    }
}

/// `BUILD_CONTAINER_ARGS` with the image spelled for `runtime`.
fn build_container_args(runtime: ContainerRuntime) -> Vec<String> {
    let image = runtime.builder_image();
    BUILD_CONTAINER_ARGS
        .iter()
        .map(|arg| {
            if *arg == BUILDER_IMAGE {
                image.clone()
            } else {
                (*arg).to_string()
            }
        })
        .collect()
}

/// The first container engine this machine has. `RuntimeMissing` means none is
/// installed, which sends the build to a worker; any other spawn failure is a
/// real error.
async fn resolve_container_runtime() -> Result<ContainerRuntime, LocalBuildError> {
    for runtime in ContainerRuntime::PREFERENCE {
        match Command::new(runtime.program())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(_) => return Ok(runtime),
            Err(error) if docker_cli_is_missing(&error) => continue,
            Err(error) => {
                return Err(LocalBuildError::Failed(format!(
                    "start {}: {error}",
                    runtime.program()
                )))
            }
        }
    }
    Err(LocalBuildError::RuntimeMissing)
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("Unsafe GitHub repository URL: expected https://github.com/owner/repo without credentials or query")]
    UnsafeUrl,
    #[error("Unsafe Git ref for repository '{repository}'")]
    UnsafeRef { repository: String },
    #[error("Unsafe plugin directory '{path}' in repository '{repository}'")]
    UnsafePath { repository: String, path: String },
    #[error("GitHub request for repository '{repository}' failed during {operation}: {reason}")]
    GitHub {
        repository: String,
        operation: &'static str,
        reason: String,
    },
    #[error("Repository '{repository}' at commit {commit} has invalid source archive: {reason}")]
    Archive {
        repository: String,
        commit: String,
        reason: String,
    },
    #[error("Repository '{repository}' at commit {commit} has invalid package manifest: {reason}")]
    Manifest {
        repository: String,
        commit: String,
        reason: String,
    },
    #[error("Repository '{repository}' at commit {commit} failed bounded Bun build: {reason}")]
    Build {
        repository: String,
        commit: String,
        reason: String,
    },
    /// The instance has no plugin builder: plugins are compiled in an isolated
    /// Docker container and this process has no Docker CLI. Not a build
    /// failure, so it must not be reported as one.
    /// Nothing can compile the plugin: this server has no Docker and no
    /// worker node took the build. `reason` is composed of fixed text and is
    /// safe to show.
    #[error("Repository '{repository}' at commit {commit} cannot be built: {reason}")]
    BuilderUnavailable {
        repository: String,
        commit: String,
        reason: String,
    },
    /// This server has no `git` executable, so it cannot fetch the source.
    #[error("Repository '{repository}' cannot be fetched: this server has no `git` executable")]
    GitUnavailable { repository: String },
    #[error("Plugin '{name}' is already installed from a different source than '{repository}'")]
    SourceConflict { name: String, repository: String },
}

pub(crate) fn parse_repository(url: &str) -> Result<(String, String), RepositoryError> {
    let parsed = url::Url::parse(url).map_err(|_| RepositoryError::UnsafeUrl)?;
    let parts: Vec<_> = parsed
        .path_segments()
        .map(|parts| parts.collect())
        .unwrap_or_default();
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(RepositoryError::UnsafeUrl);
    }
    let owner = parts[0].to_string();
    let repo = parts[1].trim_end_matches(".git").to_string();
    if repo.is_empty() || repo == "." || repo == ".." {
        return Err(RepositoryError::UnsafeUrl);
    }
    Ok((owner, repo))
}

pub(crate) fn normalize_path(
    path: Option<&str>,
    repository: &str,
) -> Result<Option<String>, RepositoryError> {
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    if path.len() > 512
        || path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.eq_ignore_ascii_case(".git")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(RepositoryError::UnsafePath {
            repository: repository.to_string(),
            path: path.to_string(),
        });
    }
    Ok(Some(path.to_string()))
}

pub(crate) fn valid_git_ref(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('-')
        && reference.len() <= 128
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
        && !reference.contains("..")
}

/// Whether spawning `docker` failed because the CLI is not installed, as on a
/// hosted control plane. Every other spawn failure is a real build error.
fn docker_cli_is_missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Why a build in a local container did not produce a binary.
#[derive(Debug)]
pub enum LocalBuildError {
    /// There is neither `docker` nor `podman` here. Nothing was compiled.
    RuntimeMissing,
    /// The container engine ran and the build failed; the text is the reason.
    Failed(String),
}

/// Where the source for a container build comes from.
pub enum ContainerBuildSource<'a> {
    /// A source tree on this machine.
    Directory(&'a Path),
    /// A tar archive. It is streamed into the container and extracted there,
    /// never on this machine's filesystem.
    Archive(&'a Path),
}

/// Compile a plugin in the pinned, resource-bounded Bun container on this
/// machine's Docker. Used for installs on a machine with Docker, and by a
/// worker's agent for a build a control plane without Docker asked for.
pub async fn build_in_container(
    source: ContainerBuildSource<'_>,
    target: &str,
    output: &Path,
) -> Result<(), LocalBuildError> {
    let run = |reason: String| LocalBuildError::Failed(reason);
    let runtime = resolve_container_runtime().await?;
    let image = runtime.builder_image();
    let local_image = Command::new(runtime.program())
        .args(["image", "inspect", &image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| run(format!("inspect pinned builder: {error}")))?;
    if !local_image.success() {
        let pull = tokio::time::timeout(
            Duration::from_secs(300),
            Command::new(runtime.program())
                .args(["pull", &image])
                .output(),
        )
        .await
        .map_err(|_| run("pinned builder image pull exceeded 300 seconds".into()))?
        .map_err(|error| run(format!("pull pinned builder: {error}")))?;
        if !pull.status.success() {
            return Err(run(format!(
                "pull pinned builder: {}",
                String::from_utf8_lossy(&pull.stderr)
            )));
        }
    }
    let create = Command::new(runtime.program())
        .args(build_container_args(runtime))
        .output()
        .await
        .map_err(|error| run(format!("create container: {error}")))?;
    if !create.status.success() {
        return Err(run(format!(
            "create container: {}",
            String::from_utf8_lossy(&create.stderr)
        )));
    }
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(run(
            "the container engine returned an invalid container identifier".into(),
        ));
    }
    let result = async {
        let start = Command::new(runtime.program())
            .args(["start", &id])
            .output()
            .await
            .map_err(|error| run(format!("start build container: {error}")))?;
        if !start.status.success() {
            return Err(run(format!(
                "start build container: {}",
                String::from_utf8_lossy(&start.stderr)
            )));
        }
        // Docker installs tmpfs mounts only when the container starts. Copying
        // before this point would place the source in its writable image layer.
        let dirs = Command::new(runtime.program())
            .args(["exec", &id, "mkdir", "-p", "/work/home", "/work/cache/bun"])
            .output()
            .await
            .map_err(|error| run(format!("prepare bounded build directories: {error}")))?;
        if !dirs.status.success() {
            return Err(run(format!(
                "prepare bounded build directories: {}",
                String::from_utf8_lossy(&dirs.stderr)
            )));
        }
        // docker cp refuses a read-only rootfs even when the destination is
        // tmpfs. Stream a capped archive through docker exec into /work.
        let mut archive_child = None;
        let mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match &source {
            ContainerBuildSource::Directory(directory) => {
                let mut child = Command::new("tar")
                    .args(["-C", &directory.display().to_string(), "-cf", "-", "."])
                    .env("COPYFILE_DISABLE", "1")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|error| run(format!("archive source for bounded staging: {error}")))?;
                let stdout = child.stdout.take().ok_or_else(|| run("source archive stdout unavailable".into()))?;
                archive_child = Some(child);
                Box::new(stdout)
            }
            ContainerBuildSource::Archive(path) => Box::new(
                tokio::fs::File::open(path)
                    .await
                    .map_err(|error| run(format!("open source archive: {error}")))?,
            ),
        };
        let mut extract = Command::new(runtime.program())
            .args(["exec", "-i", &id, "tar", "--no-same-owner", "-xf", "-", "-C", "/work"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| run(format!("start bounded source staging: {error}")))?;
        let mut extract_stdin = extract.stdin.take().ok_or_else(|| run("source staging stdin unavailable".into()))?;
        let copied = tokio::time::timeout(BUILD_TIMEOUT, tokio::io::copy(&mut (&mut reader).take(MAX_ARCHIVE_BYTES as u64 + 1), &mut extract_stdin))
            .await
            .map_err(|_| run("bounded source staging exceeded 300 seconds".into()))?
            .map_err(|error| run(format!("stream source into bounded work tmpfs: {error}")))?;
        if copied > MAX_ARCHIVE_BYTES as u64 {
            return Err(run(format!("source staging archive exceeds {MAX_ARCHIVE_BYTES} bytes")));
        }
        extract_stdin.shutdown().await.map_err(|error| run(format!("finish bounded source staging: {error}")))?;
        drop(extract_stdin);
        let archive_ok = match archive_child {
            Some(mut child) => tokio::time::timeout(BUILD_TIMEOUT, child.wait()).await.map_err(|_| run("source archive exceeded 300 seconds".into()))?.map_err(|error| run(format!("archive source: {error}")))?.success(),
            None => true,
        };
        let extract_status = tokio::time::timeout(BUILD_TIMEOUT, extract.wait()).await.map_err(|_| run("source extraction exceeded 300 seconds".into()))?.map_err(|error| run(format!("extract source in bounded work tmpfs: {error}")))?;
        if !archive_ok || !extract_status.success() {
            return Err(run(format!("bounded source staging failed (archive ok: {archive_ok}, extract {extract_status})")));
        }
        // Package resolution may use the network, but lifecycle scripts are
        // disabled. Disconnect before compiling attacker-controlled source.
        let install = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new(runtime.program())
                .args([
                    "exec",
                    &id,
                    "bun",
                    "install",
                    "--frozen-lockfile",
                    "--ignore-scripts",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| run("dependency install exceeded 300 seconds".into()))?
        .map_err(|error| run(format!("install dependencies: {error}")))?;
        if !install.status.success() {
            return Err(run(format!(
                "dependency install failed with exit status {}", install.status
            )));
        }
        // Bun downloads a cross-target runtime on first use. Prime that cache
        // with a fixed, installer-owned source while networking is available;
        // the repository's TypeScript is compiled only after disconnection.
        let warmup = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new(runtime.program())
                .args([
                    "exec", &id, "/bin/sh", "-c",
                    &format!(
                        "printf 'console.log(1)\\n' > /tmp/temps-runtime-warmup.ts && bun build --compile --target={target} /tmp/temps-runtime-warmup.ts --outfile /tmp/temps-runtime-warmup"
                    ),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| run("target runtime prefetch exceeded 300 seconds".into()))?
        .map_err(|error| run(format!("prefetch target runtime: {error}")))?;
        if !warmup.success() {
            return Err(run(format!("target runtime prefetch failed with exit status {warmup}")));
        }
        let disconnect = Command::new(runtime.program())
            .args(["network", "disconnect", runtime.build_network(), &id])
            .output()
            .await
            .map_err(|error| run(format!("disconnect build network: {error}")))?;
        if !disconnect.status.success() {
            return Err(run(format!(
                "disconnect build network: {}",
                String::from_utf8_lossy(&disconnect.stderr)
            )));
        }
        let compile = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new(runtime.program())
                .args([
                    "exec",
                    &id,
                    "bun",
                    "build",
                    "--compile",
                    &format!("--target={target}"),
                    "src/index.ts",
                    "--outfile",
                    "plugin",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| run("compile exceeded 300 seconds".into()))?
        .map_err(|error| run(format!("compile source: {error}")))?;
        if !compile.success() {
            return Err(run(format!(
                "Bun compile failed with exit status {compile}"
            )));
        }
        let size = Command::new(runtime.program())
            .args(["exec", &id, "stat", "-c", "%s", "/work/plugin"])
            .output()
            .await
            .map_err(|error| run(format!("inspect compiled size: {error}")))?;
        let byte_count = String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|error| run(format!("invalid compiled size: {error}")))?;
        if !size.status.success() || byte_count > MAX_BINARY_BYTES {
            return Err(run(format!(
                "compiled binary exceeds {}-byte limit",
                MAX_BINARY_BYTES
            )));
        }
        // docker cp cannot inspect tmpfs paths under a read-only rootfs.
        // Stream the output through exec and enforce the same binary cap.
        let mut export = Command::new(runtime.program())
            .args(["exec", &id, "cat", "/work/plugin"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| run(format!("export compiled binary: {error}")))?;
        let export_stdout = export.stdout.take().ok_or_else(|| run("compiled binary stream unavailable".into()))?;
        let mut destination = tokio::fs::File::create(output)
            .await
            .map_err(|error| run(format!("create compiled output {}: {error}", output.display())))?;
        let exported = tokio::time::timeout(BUILD_TIMEOUT, tokio::io::copy(&mut export_stdout.take(MAX_BINARY_BYTES + 1), &mut destination))
            .await
            .map_err(|_| run("compiled output export exceeded 300 seconds".into()))?
            .map_err(|error| run(format!("stream compiled output {}: {error}", output.display())))?;
        if exported > MAX_BINARY_BYTES {
            return Err(run(format!("compiled binary exceeds {}-byte limit", MAX_BINARY_BYTES)));
        }
        destination.flush().await.map_err(|error| run(format!("flush compiled output {}: {error}", output.display())))?;
        let export_status = tokio::time::timeout(BUILD_TIMEOUT, export.wait()).await.map_err(|_| run("compiled output export exceeded 300 seconds".into()))?.map_err(|error| run(format!("finish compiled output export: {error}")))?;
        if !export_status.success() || exported != byte_count {
            return Err(run(format!("compiled output export failed or size changed (status {export_status}, expected {byte_count}, received {exported})")));
        }
        #[cfg(unix)]
        tokio::fs::set_permissions(output, std::fs::Permissions::from_mode(0o500))
            .await
            .map_err(|error| run(format!("mark compiled output {} executable: {error}", output.display())))?;
        Ok(())
    }
    .await;
    let _ = Command::new(runtime.program())
        .args(["rm", "-f", &id])
        .output()
        .await;
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn podman_spells_the_pinned_image_with_its_registry() {
        let docker = build_container_args(ContainerRuntime::Docker);
        let podman = build_container_args(ContainerRuntime::Podman);
        assert!(docker.iter().any(|arg| arg == BUILDER_IMAGE));
        assert!(podman
            .iter()
            .any(|arg| arg == &format!("docker.io/{BUILDER_IMAGE}")));
        assert!(!podman.iter().any(|arg| arg == BUILDER_IMAGE));
        // Only the image differs: every limit and hardening flag is shared.
        assert_eq!(docker.len(), podman.len());
        let strip = |args: &[String]| -> Vec<String> {
            args.iter()
                .filter(|arg| !arg.ends_with(BUILDER_IMAGE))
                .cloned()
                .collect()
        };
        assert_eq!(strip(&docker), strip(&podman));
    }

    #[test]
    fn docker_is_preferred_and_each_engine_names_its_build_network() {
        assert_eq!(ContainerRuntime::PREFERENCE[0], ContainerRuntime::Docker);
        assert_eq!(ContainerRuntime::Docker.build_network(), "bridge");
        assert_eq!(ContainerRuntime::Podman.build_network(), "podman");
    }

    #[test]
    fn a_missing_docker_cli_is_not_a_build_failure() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(docker_cli_is_missing(&missing));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!docker_cli_is_missing(&denied));
    }

    use super::*;

    #[tokio::test]
    async fn builder_writable_paths_are_bounded_when_docker_available() {
        let available = Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success());
        if !available {
            return;
        }
        let image_available = Command::new("docker")
            .args(["image", "inspect", BUILDER_IMAGE])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success());
        if !image_available {
            return;
        }
        eprintln!("Docker builder quota probe running against pinned image");
        let create = Command::new("docker")
            .args(BUILD_CONTAINER_ARGS)
            .output()
            .await
            .expect("create bounded builder");
        assert!(
            create.status.success(),
            "{}",
            String::from_utf8_lossy(&create.stderr)
        );
        let id = String::from_utf8_lossy(&create.stdout).trim().to_owned();
        let start = Command::new("docker")
            .args(["start", &id])
            .output()
            .await
            .expect("start builder");
        let probe = if start.status.success() {
            Some(Command::new("docker")
                .args([
                    "exec", &id, "/bin/sh", "-c",
                    "grep -q ' /work tmpfs ' /proc/mounts && grep -q ' /tmp tmpfs ' /proc/mounts && test \"$HOME\" = /work/home && test \"$XDG_CACHE_HOME\" = /work/cache && test \"$BUN_INSTALL_CACHE_DIR\" = /work/cache/bun && ! touch /root/temps-should-not-write && dd if=/dev/zero of=/work/quota-probe bs=1M count=900 2>/tmp/quota-error; test $? -ne 0 && grep -qi 'no space left' /tmp/quota-error",
                ])
                .output()
                .await
                .expect("probe root and work quota"))
        } else {
            None
        };
        let cleanup = Command::new("docker")
            .args(["rm", "-f", &id])
            .output()
            .await
            .expect("remove builder");
        assert!(
            cleanup.status.success(),
            "{}",
            String::from_utf8_lossy(&cleanup.stderr)
        );
        assert!(
            start.status.success(),
            "{}",
            String::from_utf8_lossy(&start.stderr)
        );
        let probe = probe.expect("started builder has probe result");
        assert!(
            probe.status.success(),
            "stdout: {} stderr: {}",
            String::from_utf8_lossy(&probe.stdout),
            String::from_utf8_lossy(&probe.stderr)
        );
        eprintln!("Docker builder quota probe confirmed read-only root and /work ENOSPC");
    }

    #[test]
    fn selected_plugin_directory_is_strict_and_root_is_canonical() {
        let repository = "https://github.com/example/plugins";
        assert_eq!(normalize_path(None, repository).expect("root"), None);
        assert_eq!(
            normalize_path(Some(""), repository).expect("empty root"),
            None
        );
        assert_eq!(
            normalize_path(Some("plugins/alpha-1"), repository).expect("subtree"),
            Some("plugins/alpha-1".into())
        );
        for invalid in [
            "/plugin",
            "plugin/",
            "a//b",
            ".",
            "..",
            "a/../b",
            "a/.git/b",
            "a/.GIT",
            "a\\b",
            "a%2Fb",
            "a b",
            "é",
            "a".repeat(513).as_str(),
        ] {
            assert!(
                matches!(
                    normalize_path(Some(invalid), repository),
                    Err(RepositoryError::UnsafePath { .. })
                ),
                "accepted {invalid}"
            );
        }
    }

    async fn local_git(checkout: &Path, args: &[&str]) -> Vec<u8> {
        let result = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(args)
            .output()
            .await
            .expect("local git");
        assert!(
            result.status.success(),
            "git: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }

    #[tokio::test]
    async fn local_git_fetch_resolves_branch_tag_sha_and_rejects_missing_ref() {
        let temp = tempfile::tempdir().expect("git fixture");
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).expect("checkout directory");
        local_git(&checkout, &["init", "-q"]).await;
        std::fs::write(checkout.join("fixture"), b"source").expect("fixture source");
        local_git(&checkout, &["add", "fixture"]).await;
        local_git(
            &checkout,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        )
        .await;
        local_git(&checkout, &["branch", "feature/nested"]).await;
        local_git(
            &checkout,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "tag",
                "-am",
                "release",
                "releases/v1",
            ],
        )
        .await;
        let expected = String::from_utf8(local_git(&checkout, &["rev-parse", "HEAD"]).await)
            .expect("sha")
            .trim()
            .to_string();
        for reference in ["feature/nested", "releases/v1", expected.as_str()] {
            let bare = temp
                .path()
                .join(format!("bare-{}", reference.replace('/', "-")));
            std::fs::create_dir(&bare).expect("bare directory");
            local_git(&bare, &["init", "--bare", "-q"]).await;
            let result = Command::new("git")
                .arg("-C")
                .arg(&bare)
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--depth=1",
                    "--",
                    checkout.to_str().expect("checkout path"),
                    reference,
                ])
                .output()
                .await
                .expect("fetch process");
            assert!(
                result.status.success(),
                "fetch {reference}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let actual = String::from_utf8(
                local_git(&bare, &["rev-parse", "--verify", "FETCH_HEAD^{commit}"]).await,
            )
            .expect("resolved commit");
            assert_eq!(actual.trim(), expected, "{reference}");
        }
        let bare = temp.path().join("bare-missing");
        std::fs::create_dir(&bare).expect("bare directory");
        local_git(&bare, &["init", "--bare", "-q"]).await;
        let missing = Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args([
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--depth=1",
                "--",
                checkout.to_str().expect("checkout path"),
                "missing/ref",
            ])
            .output()
            .await
            .expect("missing fetch process");
        assert!(!missing.status.success());
    }

    #[test]
    fn repository_url_is_exactly_github_owner_repo() {
        assert!(parse_repository("https://github.com/example/plugin").is_ok());
        for url in [
            "http://github.com/example/plugin",
            "https://evil.example/plugin",
            "https://github.com/example/plugin/other",
            "https://user:pass@github.com/example/plugin",
            "https://github.com/example/plugin?x=1",
        ] {
            assert!(parse_repository(url).is_err(), "{url}");
        }
    }
}
