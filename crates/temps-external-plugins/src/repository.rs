// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fetch an exact GitHub commit and compile it in a resource-bounded Bun image.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

pub(crate) const BUILDER_IMAGE: &str =
    "oven/bun@sha256:4f6e31d1a54d6a3dd312daef655fc998101b5043d52e12592ac293ef04b9bc73";
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_FETCH_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 128 * 1024 * 1024;
const BUILD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("Unsafe GitHub repository URL: expected https://github.com/owner/repo without credentials or query")]
    UnsafeUrl,
    #[error("Unsafe Git ref for repository '{repository}'")]
    UnsafeRef { repository: String },
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
    #[error("Plugin '{name}' is already installed from a different source than '{repository}'")]
    SourceConflict { name: String, repository: String },
}

#[derive(Debug, Clone)]
pub(crate) struct RepositorySource {
    pub repository: String,
    pub name: String,
    pub ref_name: String,
    pub commit: String,
    pub version: String,
    pub source_dir: PathBuf,
}

#[derive(Deserialize)]
struct PackageManifest {
    name: String,
    version: String,
    temps: Option<TempsManifest>,
}

#[derive(Deserialize)]
struct TempsManifest {
    name: Option<String>,
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

pub(crate) async fn fetch_source(
    repository_url: &str,
    reference: Option<&str>,
    source_dir: PathBuf,
    expected_name: Option<&str>,
) -> Result<RepositorySource, RepositoryError> {
    let (owner, repo) = parse_repository(repository_url)?;
    if reference.is_some_and(|reference| !valid_git_ref(reference)) {
        return Err(RepositoryError::UnsafeRef {
            repository: format!("https://github.com/{owner}/{repo}"),
        });
    }
    let repository = format!("https://github.com/{owner}/{repo}");
    let git_dir = source_dir.with_extension("git");
    std::fs::create_dir_all(&git_dir).map_err(|error| RepositoryError::GitHub {
        repository: repository.clone(),
        operation: "create bare repository",
        reason: error.to_string(),
    })?;
    git_command(
        &repository,
        "initialize bare repository",
        &git_dir,
        &["init", "--bare", "-q", "."],
    )
    .await?;
    let effective_url = git_command(
        &repository,
        "verify Git URL",
        &git_dir,
        &["ls-remote", "--get-url", &repository],
    )
    .await?;
    if effective_url != format!("{repository}\n").as_bytes() {
        return Err(RepositoryError::GitHub {
            repository,
            operation: "verify Git URL",
            reason: "host Git configuration rewrites the GitHub URL".into(),
        });
    }
    for (operation, key, expected) in [
        ("verify Git redirects", "http.followRedirects", "false\n"),
        ("verify Git TLS", "http.sslVerify", "true\n"),
    ] {
        let effective = git_command(
            &repository,
            operation,
            &git_dir,
            &["config", "--get-urlmatch", key, &repository],
        )
        .await?;
        if effective != expected.as_bytes() {
            return Err(RepositoryError::GitHub {
                repository,
                operation,
                reason: "host Git URL-specific configuration overrides required transport safety"
                    .into(),
            });
        }
    }
    let reference = match reference {
        Some(reference) => reference.to_string(),
        None => resolve_default_branch(&repository, &git_dir).await?,
    };
    // Git runs on the host so its configured credential helper can authenticate
    // private repos. Nothing from this bare repository is copied into Docker.
    git_command(
        &repository,
        "fetch ref",
        &git_dir,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--depth=1",
            "--",
            &repository,
            &reference,
        ],
    )
    .await?;
    let commit_output = git_command(
        &repository,
        "resolve fetched commit",
        &git_dir,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
    )
    .await?;
    let commit = String::from_utf8(commit_output)
        .map_err(|_| RepositoryError::GitHub {
            repository: repository.clone(),
            operation: "resolve fetched commit",
            reason: "non-UTF-8 commit".into(),
        })?
        .trim()
        .to_string();
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RepositoryError::GitHub {
            repository,
            operation: "validate fetched commit",
            reason: "Git returned a non-SHA-1 commit".into(),
        });
    }
    let bytes = archive_commit(&repository, &git_dir, &commit).await?;
    extract_archive(&bytes, &source_dir, &repository, &commit)?;
    let package = std::fs::read(source_dir.join("package.json")).map_err(|error| {
        RepositoryError::Manifest {
            repository: repository.clone(),
            commit: commit.clone(),
            reason: error.to_string(),
        }
    })?;
    if package.len() > 64 * 1024 {
        return Err(RepositoryError::Manifest {
            repository,
            commit,
            reason: "package.json exceeds size limit".into(),
        });
    }
    let manifest: PackageManifest =
        serde_json::from_slice(&package).map_err(|error| RepositoryError::Manifest {
            repository: repository.clone(),
            commit: commit.clone(),
            reason: error.to_string(),
        })?;
    let name = manifest
        .temps
        .as_ref()
        .and_then(|temps| temps.name.as_deref())
        .unwrap_or(&manifest.name);
    if expected_name.is_some_and(|expected| expected != name)
        || semver::Version::parse(&manifest.version).is_err()
        || !source_dir.join("src/index.ts").is_file()
    {
        return Err(RepositoryError::Manifest {
            repository,
            commit,
            reason: "expected matching plugin identity, semantic version, and src/index.ts".into(),
        });
    }
    Ok(RepositorySource {
        repository,
        name: name.to_string(),
        ref_name: reference,
        commit,
        version: manifest.version,
        source_dir,
    })
}

fn valid_git_ref(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('-')
        && reference.len() <= 128
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
        && !reference.contains("..")
}

async fn resolve_default_branch(
    repository: &str,
    git_dir: &Path,
) -> Result<String, RepositoryError> {
    let output = git_command(
        repository,
        "resolve default branch",
        git_dir,
        &["ls-remote", "--symref", repository, "HEAD"],
    )
    .await?;
    let text = std::str::from_utf8(&output).map_err(|_| RepositoryError::GitHub {
        repository: repository.into(),
        operation: "resolve default branch",
        reason: "non-UTF-8 Git response".into(),
    })?;
    let branch = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")
                .and_then(|line| line.strip_suffix("\tHEAD"))
        })
        .ok_or_else(|| RepositoryError::GitHub {
            repository: repository.into(),
            operation: "resolve default branch",
            reason: "remote HEAD is not a branch".into(),
        })?;
    if !valid_git_ref(branch) {
        return Err(RepositoryError::UnsafeRef {
            repository: repository.into(),
        });
    }
    Ok(branch.to_string())
}
fn safe_git_command(git_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(git_dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "protocol.allow=never"])
        .args(["-c", "protocol.https.allow=always"])
        .args(["-c", "protocol.file.allow=never"])
        .args(["-c", "protocol.ext.allow=never"])
        .args(["-c", "http.followRedirects=false"])
        .args(["-c", "http.sslVerify=true"])
        .args(["-c", "fetch.recurseSubmodules=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE_CURL")
        .env_remove("GIT_CURL_VERBOSE")
        .env_remove("GIT_TRACE_PACKET")
        .env_remove("GIT_TRACE_PERFORMANCE")
        .env_remove("GIT_TRACE_SETUP")
        .env_remove("GIT_TRACE2")
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE2_PERF")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn git_command(
    repository: &str,
    operation: &'static str,
    git_dir: &Path,
    args: &[&str],
) -> Result<Vec<u8>, RepositoryError> {
    let mut command = safe_git_command(git_dir);
    command.args(args).stdout(
        if matches!(
            operation,
            "resolve fetched commit"
                | "resolve default branch"
                | "verify Git URL"
                | "verify Git redirects"
                | "verify Git TLS"
        ) {
            Stdio::piped()
        } else {
            Stdio::null()
        },
    );
    if operation == "fetch ref" {
        let mut child = command.spawn().map_err(|error| RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: error.to_string(),
        })?;
        let status = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                tokio::select! {
                    status = child.wait() => return status.map_err(|error| error.to_string()),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if directory_size_capped(git_dir, MAX_FETCH_BYTES)
                            .map_err(|error| error.to_string())? > MAX_FETCH_BYTES {
                            let _ = child.kill().await;
                            return Err("fetched Git data exceeded 256 MiB staging limit".into());
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: "Git command timed out".into(),
        })?
        .map_err(|reason| RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason,
        })?;
        if !status.success() {
            return Err(RepositoryError::GitHub {
                repository: repository.into(),
                operation,
                reason: format!("Git exited with status {status}"),
            });
        }
        if directory_size_capped(git_dir, MAX_FETCH_BYTES).map_err(|error| {
            RepositoryError::GitHub {
                repository: repository.into(),
                operation,
                reason: error.to_string(),
            }
        })? > MAX_FETCH_BYTES
        {
            return Err(RepositoryError::GitHub {
                repository: repository.into(),
                operation,
                reason: "fetched Git data exceeded 256 MiB staging limit".into(),
            });
        }
        return Ok(Vec::new());
    }
    let result = tokio::time::timeout(Duration::from_secs(90), command.output())
        .await
        .map_err(|_| RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: "Git command timed out".into(),
        })?
        .map_err(|error| RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: error.to_string(),
        })?;
    if !result.status.success() {
        return Err(RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: format!("Git exited with status {}", result.status),
        });
    }
    if result.stdout.len() > 512 {
        return Err(RepositoryError::GitHub {
            repository: repository.into(),
            operation,
            reason: "Git output exceeded limit".into(),
        });
    }
    Ok(result.stdout)
}

fn directory_size_capped(directory: &Path, limit: u64) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut remaining = vec![directory.to_path_buf()];
    let mut entries_seen = 0usize;
    while let Some(path) = remaining.pop() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            entries_seen += 1;
            if entries_seen > 8192 {
                return Ok(limit + 1);
            }
            let metadata = entry.path().symlink_metadata()?;
            if metadata.is_dir() {
                remaining.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
                if total > limit {
                    return Ok(total);
                }
            }
        }
    }
    Ok(total)
}

async fn archive_commit(
    repository: &str,
    git_dir: &Path,
    commit: &str,
) -> Result<Vec<u8>, RepositoryError> {
    let mut command = safe_git_command(git_dir);
    command
        .args(["archive", "--format=tar", "--prefix=root/", commit])
        .stdout(Stdio::piped());
    let mut child = command.spawn().map_err(|error| RepositoryError::Archive {
        repository: repository.into(),
        commit: commit.into(),
        reason: error.to_string(),
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: "missing Git archive stream".into(),
        })?;
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        stdout
            .take((MAX_ARCHIVE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Ok(None);
        }
        child.wait().await.map(Some)
    })
    .await
    .map_err(|_| RepositoryError::Archive {
        repository: repository.into(),
        commit: commit.into(),
        reason: "archive timed out".into(),
    })?
    .map_err(|error: std::io::Error| RepositoryError::Archive {
        repository: repository.into(),
        commit: commit.into(),
        reason: error.to_string(),
    })?;
    if result.is_none() {
        let _ = child.kill().await;
        return Err(RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: "archive exceeds size limit".into(),
        });
    }
    if !result.is_some_and(|status| status.success()) {
        return Err(RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: "git archive failed".into(),
        });
    }
    Ok(bytes)
}

fn extract_archive(
    bytes: &[u8],
    destination: &Path,
    repository: &str,
    commit: &str,
) -> Result<(), RepositoryError> {
    let mut archive = tar::Archive::new(bytes);
    std::fs::create_dir_all(destination).map_err(|error| RepositoryError::Archive {
        repository: repository.into(),
        commit: commit.into(),
        reason: error.to_string(),
    })?;
    let mut total = 0u64;
    let entries = archive
        .entries()
        .map_err(|error| RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: error.to_string(),
        })?;
    for (index, item) in entries.enumerate() {
        if index >= 4096 {
            return Err(RepositoryError::Archive {
                repository: repository.into(),
                commit: commit.into(),
                reason: "too many archive entries".into(),
            });
        }
        let mut entry = item.map_err(|error| RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: error.to_string(),
        })?;
        let path = entry.path().map_err(|error| RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: error.to_string(),
        })?;
        let mut components = path.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_))) {
            return Err(RepositoryError::Archive {
                repository: repository.into(),
                commit: commit.into(),
                reason: "archive root is not a normal directory".into(),
            });
        }
        let relative: PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(RepositoryError::Archive {
                repository: repository.into(),
                commit: commit.into(),
                reason: "unsafe archive path".into(),
            });
        }
        let kind = entry.header().entry_type();
        if !kind.is_dir() && !kind.is_file() {
            return Err(RepositoryError::Archive {
                repository: repository.into(),
                commit: commit.into(),
                reason: "archive contains link or special file".into(),
            });
        }
        total = total.saturating_add(entry.size());
        if total > MAX_SOURCE_BYTES {
            return Err(RepositoryError::Archive {
                repository: repository.into(),
                commit: commit.into(),
                reason: "expanded source exceeds size limit".into(),
            });
        }
        let target = destination.join(relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&target)
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| RepositoryError::Archive {
                    repository: repository.into(),
                    commit: commit.into(),
                    reason: error.to_string(),
                })?;
            }
            std::fs::File::create(&target)
                .and_then(|mut file| std::io::copy(&mut entry, &mut file).map(|_| ()))
        }
        .map_err(|error| RepositoryError::Archive {
            repository: repository.into(),
            commit: commit.into(),
            reason: error.to_string(),
        })?;
    }
    Ok(())
}

pub(crate) fn bun_target() -> Option<&'static str> {
    match (
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl"),
    ) {
        ("linux", "x86_64", true) => Some("bun-linux-x64-musl"),
        ("linux", "aarch64", true) => Some("bun-linux-arm64-musl"),
        ("linux", "x86_64", false) => Some("bun-linux-x64-baseline"),
        ("linux", "aarch64", false) => Some("bun-linux-arm64"),
        ("macos", "x86_64", _) => Some("bun-darwin-x64"),
        ("macos", "aarch64", _) => Some("bun-darwin-arm64"),
        _ => None,
    }
}

pub(crate) async fn build(source: &RepositorySource, output: &Path) -> Result<(), RepositoryError> {
    let run = |reason: String| RepositoryError::Build {
        repository: source.repository.clone(),
        commit: source.commit.clone(),
        reason,
    };
    let target = bun_target().ok_or_else(|| run("unsupported Bun compile target".into()))?;
    let local_image = Command::new("docker")
        .args(["image", "inspect", BUILDER_IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| run(format!("inspect pinned builder: {error}")))?;
    if !local_image.success() {
        let pull = tokio::time::timeout(
            Duration::from_secs(120),
            Command::new("docker")
                .args(["pull", BUILDER_IMAGE])
                .output(),
        )
        .await
        .map_err(|_| run("pinned builder image pull timed out".into()))?
        .map_err(|error| run(format!("pull pinned builder: {error}")))?;
        if !pull.status.success() {
            return Err(run(format!(
                "pull pinned builder: {}",
                String::from_utf8_lossy(&pull.stderr)
            )));
        }
    }
    let create = Command::new("docker")
        .args([
            "create",
            "--network",
            "bridge",
            "--cpus",
            "1",
            "--memory",
            "512m",
            "--pids-limit",
            "128",
            "--user",
            "0:0",
            "--security-opt",
            "no-new-privileges",
            "--cap-drop",
            "ALL",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=128m",
            "--workdir",
            "/work",
            "--entrypoint",
            "/bin/sh",
            BUILDER_IMAGE,
            "-c",
            "sleep 660",
        ])
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
        return Err(run("Docker returned an invalid container identifier".into()));
    }
    let result = async {
        let copy = Command::new("docker")
            .args([
                "cp",
                &format!("{}/.", source.source_dir.display()),
                &format!("{id}:/work"),
            ])
            .output()
            .await
            .map_err(|error| run(format!("copy source: {error}")))?;
        if !copy.status.success() {
            return Err(run(format!(
                "copy source: {}",
                String::from_utf8_lossy(&copy.stderr)
            )));
        }
        let start = Command::new("docker")
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
        // Package resolution may use the network, but lifecycle scripts are
        // disabled. Disconnect before compiling attacker-controlled source.
        let install = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new("docker")
                .args([
                    "exec",
                    &id,
                    "bun",
                    "install",
                    "--frozen-lockfile",
                    "--ignore-scripts",
                ])
                .stdout(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| run("dependency install exceeded 300 seconds".into()))?
        .map_err(|error| run(format!("install dependencies: {error}")))?;
        if !install.status.success() {
            #[cfg(test)]
            eprintln!("Bun install diagnostic: {}", String::from_utf8_lossy(&install.stderr));
            return Err(run(format!(
                "dependency install failed with exit status {}", install.status
            )));
        }
        // Bun downloads a cross-target runtime on first use. Prime that cache
        // with a fixed, installer-owned source while networking is available;
        // the repository's TypeScript is compiled only after disconnection.
        let warmup = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new("docker")
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
        let disconnect = Command::new("docker")
            .args(["network", "disconnect", "bridge", &id])
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
            Command::new("docker")
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
        let size = Command::new("docker")
            .args(["exec", &id, "stat", "-c", "%s", "/work/plugin"])
            .output()
            .await
            .map_err(|error| run(format!("inspect compiled size: {error}")))?;
        let byte_count = String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|error| run(format!("invalid compiled size: {error}")))?;
        if !size.status.success() || byte_count > crate::install::MAX_BINARY_BYTES {
            return Err(run(format!(
                "compiled binary exceeds {}-byte limit",
                crate::install::MAX_BINARY_BYTES
            )));
        }
        let copy = Command::new("docker")
            .args([
                "cp",
                &format!("{id}:/work/plugin"),
                &output.display().to_string(),
            ])
            .output()
            .await
            .map_err(|error| run(format!("copy output: {error}")))?;
        if !copy.status.success() {
            return Err(run(format!(
                "copy output: {}",
                String::from_utf8_lossy(&copy.stderr)
            )));
        }
        Ok(())
    }
    .await;
    let _ = Command::new("docker")
        .args(["rm", "-f", &id])
        .output()
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn public_template_build_produces_host_binary_when_docker_available() {
        let docker = Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if !docker.is_ok_and(|status| status.success()) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let source = fetch_source(
            "https://github.com/gotempsh/temps-plugin-template",
            None,
            temp.path().join("source"),
            None,
        )
        .await
        .expect("fetch public template");
        let output = temp.path().join("plugin");
        build(&source, &output)
            .await
            .expect("compile public template");
        use std::io::Read as _;
        let mut file = std::fs::File::open(&output).expect("compiled output");
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).expect("binary magic");
        #[cfg(target_os = "macos")]
        assert_eq!(magic, [0xcf, 0xfa, 0xed, 0xfe], "Mach-O 64-bit");
        #[cfg(target_os = "macos")]
        {
            let signature = Command::new("codesign")
                .args(["--verify", "--strict", &output.display().to_string()])
                .output()
                .await
                .expect("inspect Mach-O code signature");
            assert!(
                signature.status.success(),
                "cross-compiled Mach-O code signature invalid: {}",
                String::from_utf8_lossy(&signature.stderr)
            );
        }
        #[cfg(target_os = "linux")]
        assert_eq!(magic, [0x7f, b'E', b'L', b'F'], "ELF");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_ne!(
                std::fs::metadata(&output)
                    .expect("binary metadata")
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }

    #[tokio::test]
    async fn url_scoped_git_transport_override_is_detectable() {
        let temp = tempfile::tempdir().expect("tempdir");
        local_git(temp.path(), &["init", "-q"]).await;
        local_git(
            temp.path(),
            &["config", "http.https://github.com/.followRedirects", "true"],
        )
        .await;
        let effective = git_command(
            "https://github.com/example/plugin",
            "verify Git redirects",
            temp.path(),
            &[
                "config",
                "--get-urlmatch",
                "http.followRedirects",
                "https://github.com/example/plugin",
            ],
        )
        .await
        .expect("effective setting");
        assert_eq!(effective, b"true\n");
    }

    #[test]
    fn git_staging_size_limit_counts_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("pack"), [0u8; 16]).expect("pack fixture");
        assert!(directory_size_capped(temp.path(), 8).expect("size") > 8);
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
    async fn host_git_preserves_credential_helper_and_archives_exact_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout = temp.path().join("checkout");
        tokio::fs::create_dir_all(&checkout)
            .await
            .expect("checkout");
        local_git(&checkout, &["init", "-q"]).await;
        local_git(
            &checkout,
            &[
                "config",
                "credential.helper",
                "!printf 'username=fixture\\npassword=fixture-secret\\n'",
            ],
        )
        .await;
        tokio::fs::write(
            checkout.join("package.json"),
            b"{\"name\":\"fixture-plugin\"}",
        )
        .await
        .expect("write fixture");
        local_git(&checkout, &["add", "package.json"]).await;
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
        let helper = safe_git_command(&checkout)
            .args(["config", "--get", "credential.helper"])
            .output()
            .await
            .expect("credential config");
        assert_eq!(
            helper.stdout,
            b"!printf 'username=fixture\\npassword=fixture-secret\\n'\n"
        );
        let mut credential = safe_git_command(&checkout);
        credential
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!printf 'username=fixture\\npassword=fixture-secret\\n'",
                "credential",
                "fill",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = credential.spawn().expect("start mock credential fill");
        use tokio::io::AsyncWriteExt as _;
        child
            .stdin
            .take()
            .expect("credential stdin")
            .write_all(b"protocol=https\nhost=credential.example.invalid\n\n")
            .await
            .expect("credential request");
        let filled = child.wait_with_output().await.expect("credential response");
        assert!(filled.status.success());
        assert!(filled
            .stdout
            .windows(b"username=fixture".len())
            .any(|value| value == b"username=fixture"));
        assert!(filled
            .stdout
            .windows(b"password=fixture-secret".len())
            .any(|value| value == b"password=fixture-secret"));
        let commit =
            String::from_utf8(local_git(&checkout, &["rev-parse", "HEAD"]).await).expect("commit");
        let archive = archive_commit(
            "https://github.com/example/plugin",
            &checkout,
            commit.trim(),
        )
        .await
        .expect("archive exact commit");
        let source = temp.path().join("source");
        extract_archive(
            &archive,
            &source,
            "https://github.com/example/plugin",
            commit.trim(),
        )
        .expect("extract source");
        assert!(source.join("package.json").is_file());
        assert!(!source.join(".git").exists());
    }

    #[tokio::test]
    async fn public_github_ref_fetches_with_host_git_when_reachable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let git_dir = temp.path().join("repository.git");
        tokio::fs::create_dir_all(&git_dir)
            .await
            .expect("bare directory");
        git_command(
            "https://github.com/octocat/Hello-World",
            "initialize bare repository",
            &git_dir,
            &["init", "--bare", "-q", "."],
        )
        .await
        .expect("initialize");
        if git_command(
            "https://github.com/octocat/Hello-World",
            "probe GitHub",
            &git_dir,
            &[
                "ls-remote",
                "https://github.com/octocat/Hello-World",
                "HEAD",
            ],
        )
        .await
        .is_err()
        {
            return; // Network may be unavailable in CI.
        }
        let fetched = git_command(
            "https://github.com/octocat/Hello-World",
            "fetch ref",
            &git_dir,
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--depth=1",
                "--",
                "https://github.com/octocat/Hello-World",
                "master",
            ],
        )
        .await;
        fetched.expect("fetch publicly reachable GitHub ref");
        let commit = git_command(
            "https://github.com/octocat/Hello-World",
            "resolve fetched commit",
            &git_dir,
            &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        )
        .await
        .expect("resolve");
        let commit = String::from_utf8(commit).expect("commit");
        let archive = archive_commit(
            "https://github.com/octocat/Hello-World",
            &git_dir,
            commit.trim(),
        )
        .await
        .expect("archive");
        assert!(!archive.is_empty());
    }

    #[tokio::test]
    async fn pinned_builder_compiles_without_host_mounts_when_image_available() {
        let docker_available = Command::new("docker")
            .args(["info"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success());
        if !docker_available {
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
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        tokio::fs::create_dir_all(source_dir.join("src"))
            .await
            .expect("source directory");
        tokio::fs::write(
            source_dir.join("package.json"),
            br#"{"name":"fixture-plugin","version":"1.0.0","dependencies":{}}"#,
        )
        .await
        .expect("package manifest");
        tokio::fs::write(
            source_dir.join("src/index.ts"),
            b"console.log('fixture');\n",
        )
        .await
        .expect("entrypoint");
        let lockfile = Command::new("bun")
            .args(["install", "--lockfile-only", "--ignore-scripts"])
            .current_dir(&source_dir)
            .output()
            .await;
        match lockfile {
            Ok(output) if output.status.success() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Ok(output) => panic!(
                "fixture lockfile: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => panic!("fixture lockfile: {error}"),
        }
        let source = RepositorySource {
            repository: "https://github.com/example/plugin".into(),
            name: "fixture-plugin".into(),
            ref_name: "main".into(),
            commit: "a".repeat(40),
            version: "1.0.0".into(),
            source_dir,
        };
        let output = temp.path().join("plugin");
        build(&source, &output).await.expect("bounded compile");
        assert!(
            tokio::fs::metadata(&output)
                .await
                .expect("compiled binary")
                .len()
                > 1024
        );
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

    #[test]
    fn host_has_a_known_bun_compile_target() {
        if matches!(std::env::consts::OS, "linux" | "macos")
            && matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
        {
            assert!(bun_target().is_some());
        }
    }

    #[test]
    fn source_archive_rejects_symlinks() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder
                .append_link(&mut header, "root/src/index.ts", "../../outside")
                .expect("append link");
            builder.finish().expect("finish tar");
        }
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            extract_archive(
                &tar_bytes,
                temp.path(),
                "https://github.com/example/plugin",
                &"a".repeat(40)
            ),
            Err(RepositoryError::Archive { .. })
        ));
        assert!(!temp.path().join("src/index.ts").exists());
    }

    #[test]
    fn source_archive_extracts_only_inside_destination() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let body = b"export default 42;";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "root/src/index.ts", &body[..])
                .expect("append source");
            builder.finish().expect("finish tar");
        }
        let temp = tempfile::tempdir().expect("tempdir");
        extract_archive(
            &tar_bytes,
            temp.path(),
            "https://github.com/example/plugin",
            &"a".repeat(40),
        )
        .expect("extract bounded source");
        assert_eq!(
            std::fs::read(temp.path().join("src/index.ts")).expect("read source"),
            b"export default 42;"
        );
    }
}
