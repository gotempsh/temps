use async_trait::async_trait;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::user::{SANDBOX_CHOWN, SANDBOX_HOME, SANDBOX_USER, SANDBOX_WORK_DIR};
use super::{
    ExecStream, OnStreamEventCallback, SandboxCreateConfig, SandboxExecResult, SandboxHandle,
    SandboxProvider,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Container naming prefix — used for recovery after server restarts.
const SANDBOX_NAME_PREFIX: &str = "temps-sandbox-";

/// Single-quote a string for safe embedding in a `sh -c` command line.
/// Handles embedded single quotes via the `'\''` idiom.
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Shared Docker network for all workspace/agent sandboxes AND the preview
/// gateway. The gateway resolves `temps-sandbox-<sid>:<port>` via Docker's
/// embedded DNS — both sides must share this user-defined network for that
/// to work. Keep in sync with
/// `temps-cli/src/commands/serve/preview_gateway.rs::PREVIEW_GATEWAY_NETWORK`.
const SANDBOX_NETWORK: &str = "temps-sandbox-net";

/// Path inside the container where the repository is mounted. Aliased to the
/// shared `SANDBOX_WORK_DIR` constant so a future image with a different
/// non-root user (and therefore a different home dir) only requires editing
/// `sandbox::user`.
const CONTAINER_WORK_DIR: &str = SANDBOX_WORK_DIR;

/// Generate a Dockerfile for a given runtime preset.
///
/// Every image gets git, curl, jq, sudo, tmux, and the Claude CLI installed
/// on top of the base. A non-root `temps` user is created (Claude CLI refuses
/// `--dangerously-skip-permissions` as root).
///
/// Claude CLI is installed via the **native installer** (`claude.ai/install.sh`),
/// not npm. The npm package `@anthropic-ai/claude-code` is deprecated; the
/// native installer drops a prebuilt binary into `~/.local/bin/claude` and
/// removes the Node.js runtime requirement, which means runtimes like
/// python/rust/go/full no longer need `nodejs npm` purely to host Claude.
///
/// Important: the native installer must run as the **target user**, not root,
/// because it installs to `$HOME/.local/bin`. We `su - temps` after creating
/// the user (see the trailing block) instead of running as root.
pub fn dockerfile_for_runtime(runtime: &str) -> String {
    // `jq` is required by the workspace memory script (/home/temps/.temps/bin/memory)
    // — it's used to build/parse JSON for the API calls. Always installed.
    //
    // `extra_run` is reserved for runtime-specific extras the base image
    // doesn't provide (e.g. `uv` for python). Claude itself is installed in
    // the unified per-user install step at the bottom, not here.
    // `dtach` is the per-tab PTY supervisor: each workspace terminal tab runs
    // its CLI (claude/codex/opencode/bash) under `dtach -A /run/temps-pty/{tab}.sock`
    // so the PTY owner is decoupled from the `docker exec` lifecycle. When the
    // websocket drops, the dtach client exits but the dtach master keeps the
    // child alive — reconnects just re-attach. This is how we guarantee
    // "claude is launched exactly once per sandbox lifetime" across arbitrary
    // browser refreshes, without losing background-shell state the CLI is
    // tracking internally. See handlers/sessions.rs::handle_session_terminal.
    // Every base needs Node.js available because the codex CLI ships as a
    // Node script (`#!/usr/bin/env node`). Without Node on the path, the
    // post-install `codex --version` check fails with exit 127. We prefer
    // NodeSource's setup_20 over distro packages on the Ubuntu base because
    // it's a known-good major version; on Debian-derived slim bases that
    // don't include a release file curl-friendly source list, we fall back
    // to the distro `nodejs` package which is sufficient to run the
    // pre-bundled codex script.
    let (base, extra_packages, extra_run) = match runtime {
        "bun" => (
            "oven/bun:latest",
            // bun's base is Debian-based; nodejs from apt is fine for codex.
            "git ca-certificates curl jq sudo unzip dtach socat nodejs",
            "true",
        ),
        "python" => (
            "python:3.12-slim",
            "git ca-certificates curl jq sudo unzip dtach socat nodejs",
            "curl -LsSf https://astral.sh/uv/install.sh | sh",
        ),
        "rust" => (
            "rust:1-slim",
            "git ca-certificates curl jq sudo unzip dtach socat nodejs",
            "true",
        ),
        "go" => (
            // `golang:1.23-slim` was pruned from Docker Hub — use the
            // debian-based tag which is still published. (Slim variants
            // for golang don't exist for 1.23+.)
            "golang:1.23-bookworm",
            "git ca-certificates curl jq sudo unzip dtach socat nodejs",
            "true",
        ),
        "full" => (
            "ubuntu:24.04",
            "git ca-certificates curl jq nodejs npm python3 python3-pip golang-go sudo unzip dtach socat",
            "curl -LsSf https://astral.sh/uv/install.sh | sh",
        ),
        // "node" or anything else — Ubuntu-based with Node 20 from NodeSource
        // so users still have npm/npx for their own work. Claude itself no
        // longer rides on top of npm.
        _ => (
            "ubuntu:24.04",
            "git ca-certificates curl jq sudo unzip gnupg dtach socat",
            "curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
                && apt-get install -y --no-install-recommends nodejs",
        ),
    };

    // Install Bun on every non-bun runtime so `bunx @temps-sdk/cli` Just Works
    // regardless of which base image the user picked.
    let bun_install = if runtime == "bun" {
        ""
    } else {
        // Install to /usr/local/bun so it's on PATH for all users (including the
        // non-root `temps` user we create later).
        r#"RUN curl -fsSL https://bun.sh/install | BUN_INSTALL=/usr/local/bun bash \
    && ln -s /usr/local/bun/bin/bun /usr/local/bin/bun \
    && ln -s /usr/local/bun/bin/bunx /usr/local/bin/bunx
"#
    };

    // Install tools as root, then create non-root user with sudo for package installs.
    // Claude CLI refuses --dangerously-skip-permissions when running as root,
    // and the native installer drops the binary in $HOME/.local/bin — both
    // reasons we install Claude as the `temps` user, not as root.
    //
    // GitHub CLI (gh) and GitLab CLI (glab) are installed from their official
    // releases so the workspace AI can interact with PRs/MRs, issues, and CI.
    //
    // PATH includes /home/temps/.local/bin globally so the binary is visible
    // from `docker exec`, tmux panes, and login shells alike — without this,
    // the bare-bash and tmux-wrapped paths would silently miss the installer
    // location and fall back to "claude not found".
    // Stage 1: Build the temps-pty-agent binary from sources packed into
    // the build context by `pty_agent_bundle`. Isolated in its own stage so
    // the Rust toolchain doesn't bloat the final image. The binary is a
    // ~few-MB statically-linkable agent; we keep the default glibc dynamic
    // link since every base image here has a libc.
    //
    // The host's terminal handler connects to /run/temps-pty/agent.sock
    // inside the container — sandbox-entrypoint.sh supervises the agent so
    // it's respawned if it ever dies.
    let pty_agent_stage = r#"FROM rust:1-slim AS pty-agent-builder
WORKDIR /build
# Copy the whole pty-agent context at once — Cargo needs the manifest and
# the src tree in a consistent state before it'll resolve anything.
COPY pty-agent/ ./
RUN cargo build --release --bin temps-pty-agent \
    && strip target/release/temps-pty-agent

# Stage: build the in-sandbox git credential helper + daemon. Same
# rationale as the pty agent — Rust toolchain stays in its own stage so
# the final image isn't carrying a 1.5 GB compiler.
#
# These two binaries are the security boundary of the per-op credential
# system. The helper runs as the user (uid 1000) and holds no secrets.
# The daemon runs as a different uid (1001) and holds the workspace's
# deployment token in its own memory + a 0600 env file the user can't
# read. See temps-git-credential/src/lib.rs for the full architecture.
FROM rust:1-slim AS git-credential-builder
WORKDIR /build
COPY git-credential/ ./
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin temps-git-credential-helper --bin temps-git-credential-daemon \
    && strip target/release/temps-git-credential-helper \
    && strip target/release/temps-git-credential-daemon
"#;

    let user = SANDBOX_USER;
    let home = SANDBOX_HOME;
    let chown = SANDBOX_CHOWN;
    let work_dir = SANDBOX_WORK_DIR;

    format!(
        r#"{pty_agent_stage}FROM {base}
ENV DEBIAN_FRONTEND=noninteractive
ENV PATH={home}/.local/bin:/usr/local/bun/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN apt-get update && apt-get install -y --no-install-recommends {extra_packages} wget tmux bubblewrap && rm -rf /var/lib/apt/lists/*
RUN {extra_run}
{bun_install}# Install GitHub CLI from official apt repo
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | tee /usr/share/keyrings/githubcli-archive-keyring.gpg > /dev/null \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*
# Install GitLab CLI (glab) from official release tarball
RUN GLAB_ARCH=$(dpkg --print-architecture) \
    && GLAB_VERSION=1.91.0 \
    && curl -fsSL "https://gitlab.com/gitlab-org/cli/-/releases/v${{GLAB_VERSION}}/downloads/glab_${{GLAB_VERSION}}_linux_${{GLAB_ARCH}}.tar.gz" -o /tmp/glab.tar.gz \
    && tar -xzf /tmp/glab.tar.gz -C /tmp \
    && mv /tmp/bin/glab /usr/local/bin/glab \
    && chmod +x /usr/local/bin/glab \
    && rm -rf /tmp/glab.tar.gz /tmp/bin
RUN EXISTING_USER=$(getent passwd 1000 | cut -d: -f1) \
    && if [ -n "$EXISTING_USER" ] && [ "$EXISTING_USER" != "{user}" ]; then \
         (userdel -r "$EXISTING_USER" 2>/dev/null || userdel "$EXISTING_USER" 2>/dev/null || true); \
         (groupdel "$EXISTING_USER" 2>/dev/null || true); \
       fi \
    && useradd -m -s /bin/bash -u 1000 {user} \
    && echo '# temps sandbox: scoped sudo for package install only.' > /etc/sudoers.d/{user} \
    && echo 'Cmnd_Alias TEMPS_PKG = /usr/bin/apt, /usr/bin/apt-get, /usr/bin/dpkg, /usr/bin/pip, /usr/bin/pip3, /usr/local/bin/uv, /usr/bin/npm, /usr/local/bin/bun' >> /etc/sudoers.d/{user} \
    && echo '{user} ALL=(ALL) NOPASSWD: TEMPS_PKG' >> /etc/sudoers.d/{user} \
    && echo 'Defaults:{user} !requiretty, !log_input, !log_output' >> /etc/sudoers.d/{user} \
    && chmod 0440 /etc/sudoers.d/{user} \
    && visudo -c -f /etc/sudoers.d/{user}
# Second user for the credential daemon. Runs as uid 1001 so user code
# (uid 1000) cannot ptrace it, cannot read its /proc/<pid>/environ, and
# cannot read the 0600 env file holding the workspace deployment token.
# `git-users` is the bridging group that owns the IPC socket: the user
# (`temps`) is added to it so git can connect; the daemon owns the
# socket file outright so anything stricter than read+connect is
# rejected at the kernel level.
RUN groupadd -g 1100 git-users \
    && groupadd -g 1001 temps-git \
    && useradd -r -u 1001 -g temps-git -G git-users -s /usr/sbin/nologin -d /nonexistent temps-git \
    && usermod -aG git-users {user}
# Daemon is launched by the host-side message_executor via
# `docker exec --user temps-git -d`, NOT by an in-container supervisor:
# the sandbox runs with `no-new-privileges:true`, which blocks `sudo`/
# setuid uid changes from inside the container. The Docker API call
# bypasses that restriction since the host's docker daemon doesn't
# inherit the no-new-privileges flag.
RUN mkdir -p {work_dir} && chown {chown} {work_dir}
# /run/temps-pty holds one Unix socket per terminal tab (one per {{kind,tab}}
# pair). dtach creates these sockets on first attach; subsequent reconnects
# find the existing socket and re-attach instead of respawning the CLI. The
# directory lives in the container's tmpfs, so it's wiped on container
# restart — which is exactly the "launch once per sandbox lifetime" boundary
# we want.
RUN mkdir -p /run/temps-pty && chown {chown} /run/temps-pty && chmod 0700 /run/temps-pty
# Install Claude Code via the official native installer, as the sandbox user.
# Must run as the target user — the installer drops files in $HOME/.local/bin
# and refuses to install system-wide. We also seed PATH in ~/.bashrc so
# interactive shells (e.g. the tmux-wrapped terminal) find the binary even
# if the parent env wasn't propagated.
USER {user}
ENV HOME={home}
# Make every AI CLI bin directory discoverable by all shells — interactive or
# not. Bashrc alone isn't enough: `docker exec` and the workspace terminal
# launch non-login shells that don't source it, so commands were silently
# invisible. Each CLI lives in its own tree:
#   - claude:   ~/.local/bin/claude (native installer)
#   - codex:    ~/.bun/bin/codex (bun add -g)
#   - opencode: ~/.opencode/bin/opencode (curl|bash installer, hardcoded path)
ENV PATH={home}/.local/bin:{home}/.bun/bin:{home}/.opencode/bin:$PATH
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && {home}/.local/bin/claude --version \
    && echo 'export PATH={home}/.local/bin:{home}/.bun/bin:{home}/.opencode/bin:$PATH' >> {home}/.bashrc
# Codex installs via `bun add -g` into ~/.bun. Two snags worth knowing:
#
#  1. `oven/bun:latest` bakes `BUN_INSTALL_BIN=/usr/local/bin` into the
#     image env. That overrides BUN_INSTALL for the symlink step, so
#     `bun add -g` (running here as the unprivileged `temps` user) tries
#     to write its bin shim to /usr/local/bin and fails with EACCES. We
#     pin both BUN_INSTALL *and* BUN_INSTALL_BIN to the temps-owned tree
#     so the link lands somewhere we can write.
#  2. Codex itself ships as a Node script (`#!/usr/bin/env node`), which
#     is why every base also installs `nodejs` in extra_packages —
#     without it the post-install `codex --version` check exits 127.
#
# We rm -rf first to drop any stale state from base images that pre-seed
# `~/.bun` with files owned by their own bun user (we userdel'd that user
# above but the inodes can survive depending on the layer).
RUN rm -rf {home}/.bun && mkdir -p {home}/.bun/bin \
    && BUN_INSTALL={home}/.bun BUN_INSTALL_BIN={home}/.bun/bin \
       bun add -g @openai/codex \
    && {home}/.bun/bin/codex --version
RUN curl -fsSL https://opencode.ai/install | bash \
    && {home}/.opencode/bin/opencode --version
# Backup Claude CLI + Codex + OpenCode to a path outside the home dir so
# named-volume mounts (which overlay the entire home dir) don't mask the
# binaries. The container start-up hook restores from here when the volume
# is stale. Each CLI installer picks its own home subdir, so we mirror all
# three:
#   - ~/.local       → claude (native installer)
#   - ~/.bun         → codex (bun add -g)
#   - ~/.opencode    → opencode (curl|bash installer, hardcoded INSTALL_DIR)
USER root
RUN mkdir -p /opt/claude-backup \
    && cp -a {home}/.local /opt/claude-backup/local \
    && cp -a {home}/.bun /opt/claude-backup/bun \
    && cp -a {home}/.opencode /opt/claude-backup/opencode
USER root
# In-sandbox PTY agent: a single long-lived process that owns every
# interactive terminal in this container. See ADR-008 for rationale.
# The entrypoint supervises it — if it crashes, it's respawned. Existing
# images without this binary still work via the dtach fallback path in
# the terminal handler.
COPY --from=pty-agent-builder /build/target/release/temps-pty-agent /usr/local/bin/temps-pty-agent
COPY pty-agent/sandbox-entrypoint.sh /usr/local/bin/sandbox-entrypoint.sh
RUN chmod 0755 /usr/local/bin/temps-pty-agent /usr/local/bin/sandbox-entrypoint.sh
# In-sandbox git credential pipeline. Read-only mount the binaries to
# `/usr/local/bin` (root:root, mode 0755 — they hold no secrets, the
# whole point of the daemon split is that the helper has nothing
# sensitive). Provision the socket dir + env-file dir with strict
# perms: 0750 socket dir owned by `temps-git:git-users` so only the
# bridging group (which `temps` is in) can traverse it; 0700 env-file
# dir owned by `temps-git:temps-git` so only the daemon can list/read
# it. The actual env file (`credential-daemon.env`) is written by the
# message_executor via `docker exec` at session start, with mode 0600.
COPY --from=git-credential-builder /build/target/release/temps-git-credential-helper /usr/local/bin/temps-git-credential-helper
COPY --from=git-credential-builder /build/target/release/temps-git-credential-daemon /usr/local/bin/temps-git-credential-daemon
RUN chmod 0755 /usr/local/bin/temps-git-credential-helper /usr/local/bin/temps-git-credential-daemon \
    && mkdir -p /run/temps-git \
    && chown temps-git:git-users /run/temps-git \
    && chmod 0750 /run/temps-git \
    && mkdir -p /etc/temps \
    && chown temps-git:git-users /etc/temps \
    && chmod 0710 /etc/temps \
    && touch /etc/temps/credential-daemon.env \
    && chown temps-git:temps-git /etc/temps/credential-daemon.env \
    && chmod 0600 /etc/temps/credential-daemon.env
# System-wide git config: route every HTTPS git auth request through
# the credential helper. `useHttpPath=true` is mandatory — without it
# git omits the `path=` field, and the daemon can't tell what repo is
# being requested, so per-repo scoping degrades to refusal.
RUN git config --system credential.helper /usr/local/bin/temps-git-credential-helper \
    && git config --system credential.useHttpPath true
USER {user}
WORKDIR {work_dir}
# The container's CMD is whatever the caller passes (usually `sleep infinity`).
# The entrypoint starts the agent supervisor and then execs CMD. docker-init
# (enabled via HostConfig.init=true) reaps any zombies the agent leaves behind.
ENTRYPOINT ["/usr/local/bin/sandbox-entrypoint.sh"]
"#
    )
}

/// Build the tar archive that goes to `docker build` as the build context.
/// Contains the Dockerfile plus every file in [`pty_agent_bundle::BUNDLE`]
/// so the `pty-agent-builder` stage has sources to compile from.
fn build_context_tar(dockerfile: &str) -> Result<Vec<u8>, AgentError> {
    let map_tar_err = |what: &str| {
        let what = what.to_string();
        move |e: std::io::Error| AgentError::SandboxProviderUnavailable {
            provider: "docker".to_string(),
            reason: format!("Failed to {what}: {e}"),
        }
    };

    let mut tar_buf = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_buf);

        let dockerfile_bytes = dockerfile.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile_bytes.len() as u64);
        header
            .set_path("Dockerfile")
            .map_err(map_tar_err("set Dockerfile path"))?;
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append(&header, dockerfile_bytes)
            .map_err(map_tar_err("append Dockerfile"))?;

        super::pty_agent_bundle::append_to_tar(&mut tar_builder)
            .map_err(map_tar_err("append pty-agent bundle"))?;

        super::git_credential_bundle::append_to_tar(&mut tar_builder)
            .map_err(map_tar_err("append git-credential bundle"))?;

        tar_builder.finish().map_err(map_tar_err("finish tar"))?;
    }
    Ok(tar_buf)
}

/// Pinned version of the published sandbox images. Tracks the temps server
/// version's `major.minor.patch` (Option A coupling) — server `v0.1.0-*`
/// pairs with sandbox image `:0.1.0`. Pre-release/channel info is carried
/// by the channel suffix (`:0.1.0-beta`), not by this constant.
///
/// Bumping this constant causes every host to pull the new image on the
/// next sandbox start (because the new tag isn't cached locally), which
/// is the only reliable way to roll a fix out without per-host manual
/// `docker pull`. The CI release workflow publishes images at this exact
/// tag.
///
/// Why pin instead of using `:latest`:
///   - `inspect_image` returns Ok if a `:latest` is cached locally, so
///     `ensure_image_for_runtime` short-circuits and never pulls. Once a
///     host has a stale `:latest`, it stays stale forever.
///   - Immutable version tags ("0.1.0") cache-bust naturally: when we bump
///     this constant + ship the corresponding tag, every host re-pulls.
pub const SANDBOX_IMAGE_VERSION: &str = "0.1.0";

/// Release channel for sandbox image pulls. Stable temps builds resolve to
/// the canonical `:<version>` tag; beta builds resolve to `:<version>-beta`.
/// The two streams never share a tag, so a beta Dockerfile change cannot
/// poison a stable host running the same `SANDBOX_IMAGE_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxChannel {
    Stable,
    Beta,
}

impl SandboxChannel {
    /// Read the channel from `TEMPS_SANDBOX_CHANNEL`. Default is stable —
    /// only an explicit `=beta` opts the host into the beta stream.
    fn from_env() -> Self {
        match std::env::var("TEMPS_SANDBOX_CHANNEL").as_deref() {
            Ok("beta") => Self::Beta,
            _ => Self::Stable,
        }
    }

    fn tag_suffix(self) -> &'static str {
        match self {
            Self::Stable => "",
            Self::Beta => "-beta",
        }
    }
}

/// Prefix every published sandbox image carries on GHCR. Centralised so
/// runtime extraction (`runtime_from_image_name`) stays in lock-step with
/// image construction (`image_name_for_runtime_in_channel`).
const SANDBOX_IMAGE_REGISTRY_PREFIX: &str = "ghcr.io/gotempsh/temps-sandbox-";

/// Fully-qualified image name for a runtime preset. The runtime references
/// images by this exact string end-to-end — pull, inspect, container
/// create, recovery — so what you see in `docker ps` matches what was
/// actually pulled (channel suffix included). No local rename step.
///
/// Format: `ghcr.io/gotempsh/temps-sandbox-{runtime}:{version}{channel}`,
/// e.g. `ghcr.io/gotempsh/temps-sandbox-node:0.1.0-beta`.
///
/// Pinned to `SANDBOX_IMAGE_VERSION` so a host that already cached the
/// previous version pulls the new one when we bump the constant.
fn image_name_for_runtime_in_channel(runtime: &str, channel: SandboxChannel) -> String {
    let suffix = channel.tag_suffix();
    let runtime = if runtime.is_empty() { "node" } else { runtime };
    format!("{SANDBOX_IMAGE_REGISTRY_PREFIX}{runtime}:{SANDBOX_IMAGE_VERSION}{suffix}")
}

/// Convenience wrapper that reads the channel from the environment.
pub fn image_name_for_runtime(runtime: &str) -> String {
    image_name_for_runtime_in_channel(runtime, SandboxChannel::from_env())
}

/// Inverse of `image_name_for_runtime`: extract the runtime preset name
/// from a fully-qualified GHCR image string. Returns `None` for anything
/// that isn't one of our preset images (custom images, garbage). Used by
/// the recovery path to figure out which Dockerfile to regenerate when
/// rebuilding a missing image.
fn runtime_from_image_name(image: &str) -> Option<&str> {
    let rest = image.strip_prefix(SANDBOX_IMAGE_REGISTRY_PREFIX)?;
    Some(rest.split(':').next().unwrap_or(rest))
}

/// Configuration for the Docker sandbox provider.
#[derive(Debug, Clone)]
pub struct DockerSandboxConfig {
    /// Runtime preset: "node", "bun", "python", "rust", "go", "full", or "custom"
    pub runtime: String,
    /// Custom Docker image (only used when runtime is "custom")
    pub custom_image: String,
    /// Default CPU limit in cores
    pub default_cpu_limit: f64,
    /// Default memory limit in MB
    pub default_memory_limit_mb: u64,
    /// Network mode: "none" for full isolation, or a bridge name
    pub network_mode: String,
}

impl Default for DockerSandboxConfig {
    fn default() -> Self {
        Self {
            runtime: "node".to_string(),
            custom_image: String::new(),
            default_cpu_limit: 4.0,
            default_memory_limit_mb: 8192,
            network_mode: SANDBOX_NETWORK.to_string(),
        }
    }
}

impl DockerSandboxConfig {
    /// Resolve the image name for the current configuration.
    /// For presets, returns `temps-sandbox-{runtime}:{SANDBOX_IMAGE_VERSION}`.
    /// For custom, returns the user-provided image.
    pub fn resolved_image(&self) -> String {
        if self.runtime == "custom" && !self.custom_image.is_empty() {
            self.custom_image.clone()
        } else {
            image_name_for_runtime(&self.runtime)
        }
    }
}

/// Docker-based sandbox provider. Each agent run gets its own container with
/// bind-mounted work directory, resource limits, and security hardening.
pub struct DockerSandboxProvider {
    docker: Arc<Docker>,
    config: DockerSandboxConfig,
}

impl DockerSandboxProvider {
    pub fn new(docker: Arc<Docker>, config: DockerSandboxConfig) -> Self {
        Self { docker, config }
    }

    /// Build the sandbox image if it doesn't exist.
    /// For preset runtimes, generates a Dockerfile dynamically.
    /// For custom images, assumes the image is already available (pull or pre-built).
    pub async fn ensure_image(&self) -> Result<(), AgentError> {
        self.ensure_image_for_runtime(&self.config.runtime).await
    }

    /// Build a sandbox image for a specific runtime preset.
    async fn ensure_image_for_runtime(&self, runtime: &str) -> Result<(), AgentError> {
        self.ensure_image_for_runtime_with_progress(runtime, None)
            .await
    }

    /// Build a sandbox image, optionally streaming progress via a channel.
    async fn ensure_image_for_runtime_with_progress(
        &self,
        runtime: &str,
        progress: Option<&tokio::sync::mpsc::Sender<String>>,
    ) -> Result<(), AgentError> {
        // Helper to send progress if a channel is provided.
        let send = |msg: String| async {
            if let Some(tx) = progress {
                let _ = tx.send(msg).await;
            }
        };

        // Custom images: just check if they exist (user must pull/build them)
        if runtime == "custom" {
            let img = &self.config.custom_image;
            if img.is_empty() {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: "Custom runtime selected but no image specified".to_string(),
                });
            }
            // Try to pull if not present locally
            if self.docker.inspect_image(img).await.is_err() {
                tracing::info!("Pulling custom sandbox image {}...", img);
                let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
                    .from_image(img.as_str())
                    .build();
                let mut stream = self.docker.create_image(Some(options), None, None);
                while let Some(result) = stream.next().await {
                    if let Err(e) = result {
                        return Err(AgentError::SandboxProviderUnavailable {
                            provider: "docker".to_string(),
                            reason: format!("Failed to pull custom image {}: {}", img, e),
                        });
                    }
                }
            }
            return Ok(());
        }

        let image_name = image_name_for_runtime(runtime);

        // Check if image already exists locally. The image name is the
        // fully-qualified GHCR string (incl. channel suffix), so this
        // check correctly distinguishes a cached beta image from a
        // cached stable one — no rename indirection masking which
        // channel is loaded.
        if self.docker.inspect_image(&image_name).await.is_ok() {
            tracing::debug!("Sandbox image {} already exists", image_name);
            return Ok(());
        }

        // Try pulling a prebuilt image from GHCR first. This is much
        // faster than building locally (~seconds vs ~minutes) because the
        // pushed image is a fully baked layer including Claude CLI, bun,
        // gh, etc. If the pull fails (rate limit, no internet, image not
        // published yet) we fall back to a local build — fail-safe, never
        // blocks startup. The local build tags itself with the same GHCR
        // name so the next inspect_image short-circuits as expected.
        send(format!("Pulling {} from GHCR...", image_name)).await;
        tracing::info!("Trying to pull sandbox image {} from GHCR...", image_name);
        match self.try_pull(&image_name).await {
            Ok(()) => {
                tracing::info!("Pulled {} — skipping local build", image_name);
                send(format!("Pulled {} — done.", image_name)).await;
                return Ok(());
            }
            Err(reason) => {
                tracing::info!(
                    "Pull of {} failed ({}), falling back to local build",
                    image_name,
                    reason
                );
                send(format!("Pull failed ({}), building locally...", reason)).await;
            }
        }

        tracing::info!(
            "Building sandbox image {} (runtime: {})...",
            image_name,
            runtime
        );
        send(format!("Building {} (runtime: {})...", image_name, runtime)).await;

        let dockerfile_content = dockerfile_for_runtime(runtime);
        let tar_buf = build_context_tar(&dockerfile_content)?;

        let options = bollard::query_parameters::BuildImageOptionsBuilder::new()
            .t(&image_name)
            .build();

        let body = http_body_util::Full::new(bytes::Bytes::from(tar_buf));
        let mut stream =
            self.docker
                .build_image(options, None, Some(http_body_util::Either::Left(body)));

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(ref error_detail) = info.error_detail {
                        let msg = error_detail
                            .message
                            .as_deref()
                            .unwrap_or("unknown build error");
                        send(format!("ERROR: {}", msg)).await;
                        return Err(AgentError::SandboxProviderUnavailable {
                            provider: "docker".to_string(),
                            reason: format!("Image build error: {}", msg),
                        });
                    }
                    // Forward build log lines
                    if let Some(ref line) = info.stream {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            send(trimmed.to_string()).await;
                        }
                    }
                }
                Err(e) => {
                    send(format!("ERROR: {}", e)).await;
                    return Err(AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!("Image build failed: {}", e),
                    });
                }
            }
        }

        tracing::info!("Sandbox image {} built successfully", image_name);
        send(format!("Image {} built successfully.", image_name)).await;
        Ok(())
    }

    /// Build a sandbox image locally from the generated Dockerfile.
    /// Used by explicit rebuild operations that should never pull from Hub.
    async fn build_image_locally(
        &self,
        runtime: &str,
        progress: Option<&tokio::sync::mpsc::Sender<String>>,
    ) -> Result<(), AgentError> {
        let send = |msg: String| async {
            if let Some(tx) = progress {
                let _ = tx.send(msg).await;
            }
        };

        let image_name = image_name_for_runtime(runtime);

        tracing::info!(
            "Building sandbox image {} locally (runtime: {})...",
            image_name,
            runtime
        );
        send(format!(
            "Building {} locally (runtime: {})...",
            image_name, runtime
        ))
        .await;

        let dockerfile_content = dockerfile_for_runtime(runtime);
        let tar_buf = build_context_tar(&dockerfile_content)?;

        let options = bollard::query_parameters::BuildImageOptionsBuilder::new()
            .t(&image_name)
            .build();

        let body = http_body_util::Full::new(bytes::Bytes::from(tar_buf));
        let mut stream =
            self.docker
                .build_image(options, None, Some(http_body_util::Either::Left(body)));

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(ref error_detail) = info.error_detail {
                        let msg = error_detail
                            .message
                            .as_deref()
                            .unwrap_or("unknown build error");
                        send(format!("ERROR: {}", msg)).await;
                        return Err(AgentError::SandboxProviderUnavailable {
                            provider: "docker".to_string(),
                            reason: format!("Image build error: {}", msg),
                        });
                    }
                    if let Some(ref line) = info.stream {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            send(trimmed.to_string()).await;
                        }
                    }
                }
                Err(e) => {
                    send(format!("ERROR: {}", e)).await;
                    return Err(AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!("Image build failed: {}", e),
                    });
                }
            }
        }

        tracing::info!("Sandbox image {} built successfully", image_name);
        send(format!("Image {} built successfully.", image_name)).await;
        Ok(())
    }

    /// Pull `image` from its registry (no rename — the image lives under
    /// its full GHCR name end-to-end). Returns `Ok(())` on success,
    /// `Err(reason)` on any failure — callers should treat failure as
    /// non-fatal and fall back to a local build.
    async fn try_pull(&self, image: &str) -> Result<(), String> {
        let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
            .from_image(image)
            .build();
        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            if let Err(e) = result {
                return Err(format!("{}", e));
            }
        }
        Ok(())
    }

    /// Ensure the sandbox network exists.
    async fn ensure_network(&self) -> Result<(), AgentError> {
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("Failed to list networks: {}", e),
            })?;

        let exists = networks
            .iter()
            .any(|n| n.name.as_ref() == Some(&self.config.network_mode));

        if !exists && self.config.network_mode != "none" && self.config.network_mode != "host" {
            tracing::info!("Creating sandbox network: {}", self.config.network_mode);
            let create_opts = bollard::models::NetworkCreateRequest {
                name: self.config.network_mode.clone(),
                driver: Some("bridge".to_string()),
                internal: Some(false), // Allow outbound (Claude CLI needs API access)
                ..Default::default()
            };
            self.docker.create_network(create_opts).await.map_err(|e| {
                AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("Failed to create network: {}", e),
                }
            })?;
        }

        Ok(())
    }

    fn container_name(run_id: i32) -> String {
        format!("{}{}", SANDBOX_NAME_PREFIX, run_id)
    }

    /// Shared recovery by absolute container name — looks up the container
    /// and returns a handle for it regardless of whether it's currently
    /// running or stopped. Only returns `None` when the container has been
    /// removed at the Docker level.
    ///
    /// IMPORTANT: this function must never delete a container. Stopped is a
    /// legitimate long-lived state for standalone sandboxes — the
    /// expiration sweeper parks expired sandboxes there, and the user
    /// resumes them later. An earlier version of this function auto-removed
    /// stopped containers on the assumption that "stopped" meant "leaked
    /// leftover"; that destroyed sandbox filesystems + volumes on every
    /// server restart that happened between stop and resume. Callers who
    /// genuinely want to destroy must go through the explicit `destroy`
    /// path, which is the only code that should call `remove_container`.
    ///
    /// Used by both `recover(run_id)` (numeric naming for agent runs /
    /// workspace sessions) and `recover_by_name(id)` (public_id naming for
    /// standalone sandboxes).
    async fn recover_container(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        match self
            .docker
            .inspect_container(
                container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                let container_id = info.id.unwrap_or_default();
                tracing::info!("Recovered sandbox {} (running={})", container_name, running);
                Ok(Some(SandboxHandle {
                    sandbox_id: container_id,
                    sandbox_name: container_name.to_string(),
                    work_dir: PathBuf::from(CONTAINER_WORK_DIR),
                }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Shared exec implementation — one place that owns the bollard
    /// StartExec stream handling and the IDLE_POLL phantom-stream guard.
    /// Both `exec` (legacy stdout-only callback) and `exec_streamed`
    /// (stream-tagged callback) funnel through here so the two paths
    /// can't drift apart.
    ///
    /// When `on_event` is present, every stdout/stderr line is dispatched
    /// through the callback as it arrives. The returned `SandboxExecResult`
    /// has `stdout` containing **stdout only** and `stderr` containing
    /// **stderr only** — the bollard-side aggregation that used to fold
    /// stderr into `stdout` is gone. Callers that still want the combined
    /// view can concatenate at the call site.
    async fn exec_inner(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
        user: Option<String>,
    ) -> Result<SandboxExecResult, AgentError> {
        let env_vars: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        let exec_config = bollard::models::ExecConfig {
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            cmd: Some(cmd.clone()),
            working_dir: Some(handle.work_dir.to_string_lossy().to_string()),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            user,
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&handle.sandbox_id, exec_config)
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to create exec: {}", e),
            })?;

        let start_config = bollard::exec::StartExecOptions {
            detach: false,
            ..Default::default()
        };

        let output = self
            .docker
            .start_exec(&exec.id, Some(start_config))
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to start exec: {}", e),
            })?;

        let mut stdout_output = String::new();
        let mut stderr_output = String::new();

        match output {
            StartExecResults::Attached { mut output, .. } => {
                // See comment in prior implementation: bollard's exec stream
                // can park forever on phantom completions. IDLE_POLL + an
                // `inspect_exec` check lets us detect and break out of that.
                const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(15);
                loop {
                    match tokio::time::timeout(IDLE_POLL, output.next()).await {
                        Ok(Some(Ok(LogOutput::StdOut { message }))) => {
                            let text = String::from_utf8_lossy(&message);
                            for line in text.lines() {
                                stdout_output.push_str(line);
                                stdout_output.push('\n');
                                if let Some(ref cb) = on_event {
                                    cb(ExecStream::Stdout, line.to_string()).await;
                                }
                            }
                        }
                        Ok(Some(Ok(LogOutput::StdErr { message }))) => {
                            let text = String::from_utf8_lossy(&message);
                            for line in text.lines() {
                                stderr_output.push_str(line);
                                stderr_output.push('\n');
                                if let Some(ref cb) = on_event {
                                    cb(ExecStream::Stderr, line.to_string()).await;
                                }
                            }
                        }
                        Ok(Some(Ok(_))) => {}
                        Ok(Some(Err(e))) => {
                            tracing::warn!(
                                "Sandbox {} exec stream error: {}",
                                handle.sandbox_name,
                                e
                            );
                            break;
                        }
                        Ok(None) => {
                            // Stream closed cleanly — exec is done.
                            break;
                        }
                        Err(_) => match self.docker.inspect_exec(&exec.id).await {
                            Ok(info) if info.running == Some(true) => {
                                tracing::debug!(
                                    "Sandbox {} exec idle (still running), continuing to wait",
                                    handle.sandbox_name
                                );
                                continue;
                            }
                            Ok(_) => {
                                tracing::warn!(
                                        "Sandbox {} exec stream went idle and the exec is no longer running — bailing out to avoid permanent hang",
                                        handle.sandbox_name
                                    );
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                        "Sandbox {} inspect_exec failed during idle check: {} — bailing out",
                                        handle.sandbox_name,
                                        e
                                    );
                                break;
                            }
                        },
                    }
                }
            }
            StartExecResults::Detached => {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: "Exec started in detached mode unexpectedly".to_string(),
                });
            }
        }

        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1) as i32;

        Ok(SandboxExecResult {
            exit_code,
            stdout: stdout_output,
            stderr: stderr_output,
        })
    }
}

#[async_trait]
impl SandboxProvider for DockerSandboxProvider {
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        self.ensure_network().await?;

        let container_name = config
            .container_name_override
            .clone()
            .map(|id| format!("{}{}", SANDBOX_NAME_PREFIX, id))
            .unwrap_or_else(|| Self::container_name(config.run_id));

        // Remove existing container with the same name if any (leftover from crash)
        let _ = self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        // Resolve image: per-run override > provider config
        let default_image = self.config.resolved_image();
        let image = config
            .image
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&default_image);

        // Ensure the image exists (build for presets, pull for custom)
        if self.docker.inspect_image(image).await.is_err() {
            // If this is a preset image (full GHCR path), build it. The
            // helper strips the registry prefix + version tag to recover
            // the runtime name. Anything that doesn't match the preset
            // prefix is treated as a user-supplied custom image.
            if let Some(runtime) = runtime_from_image_name(image) {
                self.ensure_image_for_runtime(runtime).await?;
            }
            // Otherwise it's a custom image — try to pull
            else {
                tracing::info!("Pulling sandbox image {}...", image);
                let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
                    .from_image(image)
                    .build();
                let mut stream = self.docker.create_image(Some(options), None, None);
                while let Some(result) = stream.next().await {
                    if let Err(e) = result {
                        return Err(AgentError::SandboxCreationFailed {
                            run_id: config.run_id,
                            provider: "docker".to_string(),
                            reason: format!("Failed to pull image {}: {}", image, e),
                        });
                    }
                }
            }
        }
        let cpu_limit = config.cpu_limit.unwrap_or(self.config.default_cpu_limit);
        let memory_limit_mb = config
            .memory_limit_mb
            .unwrap_or(self.config.default_memory_limit_mb);
        let network = config
            .network_mode
            .as_deref()
            .unwrap_or(&self.config.network_mode);
        // Map user-friendly names to Docker network modes.
        //
        // IMPORTANT: "full" used to map to docker `host` mode, which bypassed
        // container network isolation entirely and prevented sandboxes from
        // joining the shared `temps-sandbox-net` user-defined network. That
        // broke workspace preview routing because the preview gateway resolves
        // sandbox containers via Docker's embedded DNS, which only works on
        // user-defined networks. We now route "full" through the shared bridge
        // network (`SANDBOX_NETWORK`) — sandboxes still get full outbound
        // internet (the network is created with `internal: false`) but they
        // also get a real container IP and DNS name that the gateway can hit.
        //
        // `host` is still accepted as an explicit opt-out for callers that
        // really need the host stack (e.g. legacy autofixer flows).
        let docker_network = match network {
            "none" => "none".to_string(),
            "host" => "host".to_string(),
            "full" | "restricted" => SANDBOX_NETWORK.to_string(),
            other => other.to_string(),
        };

        let host_work_dir = config.host_work_dir.to_string_lossy().to_string();

        // Build environment variables
        let env_vars: Vec<String> = config
            .env_vars
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        // Bind mount: only the work directory. Auth is handled via env vars
        // (CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY) — no host config mounting.
        //
        // Named volume for the sandbox user's home dir: persists claude
        // session jsonl, shell history, ~/.claude/projects, ~/.config/...
        // across container recreation. Without this, killing and recreating
        // the sandbox would lose all conversation continuity even though the
        // work_dir survives via the bind mount above.
        //
        // The volume name is keyed on run_id so each session keeps its own
        // home isolated, and the volume is auto-created on first mount.
        let home_volume_name = format!("temps-sandbox-home-{}", config.run_id);
        let binds = vec![
            format!("{}:{}", host_work_dir, CONTAINER_WORK_DIR),
            format!("{}:{}", home_volume_name, SANDBOX_HOME),
        ];

        // tmpfs mount for secrets — in-memory only, never written to disk
        let mut tmpfs = HashMap::new();
        tmpfs.insert("/run/secrets".to_string(), "size=1m,mode=0700".to_string());

        let host_config = bollard::models::HostConfig {
            binds: Some(binds),
            network_mode: Some(docker_network),
            tmpfs: Some(tmpfs),
            // Resource limits.
            //
            // `memory_swap == memory` disables swap usage for the container.
            // Without this, Docker's default is `memory_swap = 2 * memory`,
            // meaning each sandbox can silently page an *additional* full
            // memory-limit's worth to host swap. With N sandboxes running
            // dev servers + claude that's a fast path to host swap
            // exhaustion, terrible latency, and no OOM feedback to the
            // sandbox itself. Disabling swap turns over-limit into a clean
            // OOM-kill of the offending process (which is the correct
            // signal — the user sees `next dev` die instead of the whole
            // host dragging).
            nano_cpus: Some((cpu_limit * 1_000_000_000.0) as i64),
            memory: Some(memory_limit_mb as i64 * 1024 * 1024),
            memory_swap: Some(memory_limit_mb as i64 * 1024 * 1024),
            // Security hardening
            cap_drop: Some(vec!["ALL".to_string()]),
            // CHOWN/FOWNER are needed so the post-start chown of the sandbox
            // home dir (fixing stale named-volume ownership) can run as root.
            cap_add: Some(vec!["CHOWN".to_string(), "FOWNER".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(config.pids_limit.unwrap_or(512)),
            init: Some(true),
            // Survive Docker daemon restarts (reboot, `systemctl restart docker`,
            // Mac sleep/wake). Without this the default is "no" and a daemon
            // bounce permanently kills the container, leaving the DB row stuck
            // in `running` with nothing behind it. `unless-stopped` means an
            // explicit `docker stop` (including our own pause/destroy paths)
            // still keeps the container stopped — we only auto-restart after
            // daemon-level events.
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };

        let mut labels = HashMap::new();
        labels.insert("sh.temps.sandbox".to_string(), "true".to_string());
        labels.insert(
            "sh.temps.sandbox.run_id".to_string(),
            config.run_id.to_string(),
        );

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image.to_string()),
            // Keep the container alive — exec calls run commands inside it
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            working_dir: Some(CONTAINER_WORK_DIR.to_string()),
            host_config: Some(host_config),
            labels: Some(labels),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to create container: {}", e),
            })?;

        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to start container: {}", e),
            })?;

        // Fix sandbox home ownership: the named volume inherits the host's
        // anonymous-volume root uid on first mount, and stale volumes from
        // earlier image builds may be owned by a different uid entirely.
        // Running chown as root (not USER {sandbox-user}) normalizes it
        // every start.
        {
            let exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::models::ExecConfig {
                        user: Some("0:0".to_string()),
                        cmd: Some(vec![
                            "chown".to_string(),
                            "-R".to_string(),
                            SANDBOX_CHOWN.to_string(),
                            SANDBOX_HOME.to_string(),
                        ]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!("Failed to create chown exec: {}", e),
                })?;
            let _ = self
                .docker
                .start_exec(
                    &exec.id,
                    Some(bollard::exec::StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await;
        }

        // Fix work-dir ownership: the bind-mounted host directory carries
        // its host-side uid into the container, regardless of the image's
        // `USER {sandbox-user}` directive. In production the temps server
        // runs as root, so the host work_dir is created root-owned and
        // `git clone` (which executes on the host) writes root-owned files.
        // Inside the container the sandbox user then can't write — TUIs
        // fail, dev servers can't open lockfiles, etc. Mirror the home-dir
        // fix: recursively chown as root, every start.
        {
            let exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::models::ExecConfig {
                        user: Some("0:0".to_string()),
                        cmd: Some(vec![
                            "chown".to_string(),
                            "-R".to_string(),
                            SANDBOX_CHOWN.to_string(),
                            CONTAINER_WORK_DIR.to_string(),
                        ]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!("Failed to create workspace chown exec: {}", e),
                })?;
            let _ = self
                .docker
                .start_exec(
                    &exec.id,
                    Some(bollard::exec::StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await;
        }

        // Ensure AI CLIs are present in the home volume. Named volumes
        // persist across image rebuilds and mask the image's home dir,
        // wiping claude/codex/opencode every time the volume gets recycled.
        // Strategy: restore from /opt/claude-backup (local builds always
        // populate it), fall back to re-running the claude installer if
        // /opt/claude-backup is missing entirely (older Hub images).
        //
        // We restore both ~/.local (claude + opencode) and ~/.bun (codex)
        // because bun installs codex into its own global tree, not ~/.local.
        {
            let restore_script = format!(
                "need_restore=0; \
                 [ -x {home}/.local/bin/claude ] || need_restore=1; \
                 [ -x {home}/.bun/bin/codex ] || need_restore=1; \
                 [ -x {home}/.opencode/bin/opencode ] || need_restore=1; \
                 if [ \"$need_restore\" = \"0\" ]; then exit 0; fi; \
                 echo 'AI CLIs missing in home volume, restoring...'; \
                 if [ -d /opt/claude-backup/local ]; then \
                   mkdir -p {home}/.local {home}/.bun {home}/.opencode && \
                   cp -a /opt/claude-backup/local/. {home}/.local/ && \
                   cp -a /opt/claude-backup/bun/. {home}/.bun/ && \
                   cp -a /opt/claude-backup/opencode/. {home}/.opencode/ && \
                   chown -R {chown} {home}/.local {home}/.bun {home}/.opencode; \
                 elif [ -d /opt/claude-backup ]; then \
                   cp -a /opt/claude-backup/. {home}/.local/ && \
                   chown -R {chown} {home}/.local; \
                 elif command -v curl >/dev/null 2>&1; then \
                   su - {user} -c 'curl -fsSL https://claude.ai/install.sh | bash' 2>&1; \
                 fi",
                home = SANDBOX_HOME,
                chown = SANDBOX_CHOWN,
                user = SANDBOX_USER,
            );
            let exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::models::ExecConfig {
                        user: Some("0:0".to_string()),
                        cmd: Some(vec!["sh".to_string(), "-c".to_string(), restore_script]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "docker".to_string(),
                    reason: format!("Failed to create claude-fix exec: {}", e),
                })?;
            let _ = self
                .docker
                .start_exec(
                    &exec.id,
                    Some(bollard::exec::StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await;
        }

        tracing::info!(
            "Sandbox container {} ({}) created for run {}",
            container_name,
            &container.id[..12],
            config.run_id
        );

        Ok(SandboxHandle {
            sandbox_id: container.id,
            sandbox_name: container_name,
            work_dir: PathBuf::from(CONTAINER_WORK_DIR),
        })
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        // Wrap the legacy stdout-only callback into a stream-aware one that
        // discards stderr events, then delegate to the unified inner impl.
        // Keeping one implementation avoids drift between `exec` and
        // `exec_streamed`.
        let stream_cb: Option<OnStreamEventCallback> = on_output.map(|cb| {
            let cb = cb.clone();
            let f: OnStreamEventCallback =
                std::sync::Arc::new(move |stream: ExecStream, line: String| {
                    let cb = cb.clone();
                    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                        Box::pin(async move {
                            if matches!(stream, ExecStream::Stdout) {
                                cb(line).await;
                            }
                        });
                    fut
                });
            f
        });
        self.exec_inner(handle, cmd, env, stream_cb, None).await
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as_user(handle, "0:0", cmd, env, on_output).await
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let stream_cb: Option<OnStreamEventCallback> = on_output.map(|cb| {
            let cb = cb.clone();
            let f: OnStreamEventCallback =
                std::sync::Arc::new(move |stream: ExecStream, line: String| {
                    let cb = cb.clone();
                    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                        Box::pin(async move {
                            if matches!(stream, ExecStream::Stdout) {
                                cb(line).await;
                            }
                        });
                    fut
                });
            f
        });
        self.exec_inner(handle, cmd, env, stream_cb, Some(user.to_string()))
            .await
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_inner(handle, cmd, env, on_event, None).await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        match self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => {
                let running = info.state.and_then(|s| s.running).unwrap_or(false);
                Ok(running)
            }
            Err(_) => Ok(false),
        }
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        tracing::info!(
            "Destroying sandbox container {} ({})",
            handle.sandbox_name,
            &handle.sandbox_id[..std::cmp::min(12, handle.sandbox_id.len())]
        );

        // Stop gracefully (5s timeout), then force remove
        let _ = self
            .docker
            .stop_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(5),
                    signal: None,
                }),
            )
            .await;

        self.docker
            .remove_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to remove container: {}", e),
            })?;

        // Only remove the named home volume when the caller asks for a
        // full purge (session *delete*, or ephemeral agent runs). On a
        // plain session *close* the volume must survive so claude auth,
        // shell history, and ~/.claude/projects are preserved when the
        // session is reopened.
        if purge_volumes {
            if let Some(run_id_str) = handle.sandbox_name.strip_prefix(SANDBOX_NAME_PREFIX) {
                let home_volume_name = format!("temps-sandbox-home-{}", run_id_str);
                if let Err(e) = self
                    .docker
                    .remove_volume(
                        &home_volume_name,
                        None::<bollard::query_parameters::RemoveVolumeOptions>,
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to remove sandbox home volume {} (may not exist): {}",
                        home_volume_name,
                        e
                    );
                }
            }
        }

        Ok(())
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        tracing::info!("Stopping sandbox container {}", handle.sandbox_name);
        self.docker
            .stop_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to stop container: {}", e),
            })?;
        Ok(())
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        tracing::info!("Starting sandbox container {}", handle.sandbox_name);
        self.docker
            .start_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to start container: {}", e),
            })?;
        Ok(())
    }

    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        tracing::info!("Restarting sandbox container {}", handle.sandbox_name);
        self.docker
            .restart_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::RestartContainerOptions>,
            )
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to restart container: {}", e),
            })?;
        Ok(())
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        // Split the absolute path into the parent dir (extraction target) and
        // the file basename (entry name inside the tar). Docker's
        // upload_to_container extracts the tar at the given `path`.
        let (parent_dir, file_name) = match path.rsplit_once('/') {
            Some((p, f)) if !f.is_empty() => {
                let parent = if p.is_empty() { "/" } else { p };
                (parent.to_string(), f.to_string())
            }
            _ => {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: format!(
                        "write_file: path '{}' must be absolute with a filename",
                        path
                    ),
                });
            }
        };

        // Build an in-memory tar with a single file entry. tar crate is sync,
        // so we do this on the current thread (cheap for skill/CLAUDE/.env sized files).
        let tar_bytes = {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            // Files under /home/temps must be owned by the `temps` user (uid 1000)
            // created in the sandbox Dockerfile, otherwise tight modes like 0600
            // become unreadable by the container's runtime user.
            if path.starts_with("/home/temps") {
                header.set_uid(1000);
                header.set_gid(1000);
            }
            header.set_mtime(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            header.set_cksum();

            let mut buf: Vec<u8> = Vec::with_capacity(contents.len() + 1024);
            {
                let mut builder = tar::Builder::new(&mut buf);
                builder
                    .append_data(&mut header, &file_name, contents)
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_file: tar build failed for {}: {}", path, e),
                    })?;
                builder
                    .finish()
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_file: tar finish failed for {}: {}", path, e),
                    })?;
            }
            buf
        };

        // Ensure parent directory exists. upload_to_container won't create
        // intermediate dirs — extraction fails if `parent_dir` is missing.
        // Use a short, well-bounded exec for mkdir (it produces no output but
        // returns quickly; the polling exec loop handles the phantom case).
        let mkdir = vec!["mkdir".to_string(), "-p".to_string(), parent_dir.clone()];
        // Best-effort: if mkdir hangs, the upload below will fail loudly with
        // a clear "no such directory" error rather than hanging silently.
        let _ = self.exec(handle, mkdir, HashMap::new(), None).await;

        let options = bollard::query_parameters::UploadToContainerOptionsBuilder::default()
            .path(&parent_dir)
            .build();

        let body = bollard::body_full(tar_bytes.into());

        // Hard timeout so we never replicate the phantom-stream hang.
        let upload = self
            .docker
            .upload_to_container(&handle.sandbox_id, Some(options), body);

        match tokio::time::timeout(std::time::Duration::from_secs(30), upload).await {
            Ok(Ok(())) => {
                tracing::debug!(
                    "write_file: uploaded {} bytes to {} in container {}",
                    contents.len(),
                    path,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_file: upload to {} failed: {}", path, e),
            }),
            Err(_) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_file: upload to {} timed out after 30s", path),
            }),
        }
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        use futures::StreamExt;
        use std::io::Read;

        let options = bollard::query_parameters::DownloadFromContainerOptionsBuilder::default()
            .path(path)
            .build();

        let stream = self
            .docker
            .download_from_container(&handle.sandbox_id, Some(options));

        // Collect tar stream into memory with a hard 30s cap so we never hang.
        let collect = async {
            let mut buf: Vec<u8> = Vec::new();
            let mut s = stream;
            while let Some(chunk) = s.next().await {
                match chunk {
                    Ok(bytes) => buf.extend_from_slice(&bytes),
                    Err(e) => {
                        return Err(AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id: handle.sandbox_id.clone(),
                            reason: format!("read_file: download {} failed: {}", path, e),
                        });
                    }
                }
            }
            Ok(buf)
        };

        let tar_bytes =
            match tokio::time::timeout(std::time::Duration::from_secs(30), collect).await {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("read_file: download {} timed out after 30s", path),
                    });
                }
            };

        // Extract the single file from the tar. Docker's archive endpoint
        // returns a tar whose top-level entry is the basename of `path`.
        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let mut entries = archive
            .entries()
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("read_file: tar open for {} failed: {}", path, e),
            })?;

        for entry in entries.by_ref() {
            let mut entry = entry.map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("read_file: tar entry for {} failed: {}", path, e),
            })?;
            // Skip directories, symlinks, etc. — we want the regular file.
            if entry.header().entry_type().is_file() {
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("read_file: read entry for {} failed: {}", path, e),
                    })?;
                return Ok(contents);
            }
        }

        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!("read_file: no regular file entry in tar for {}", path),
        })
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        use walkdir::WalkDir;

        // Build an in-memory tar containing all files from local_dir,
        // preserving relative paths.
        let tar_bytes = {
            let mut buf: Vec<u8> = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut buf);

                for entry in WalkDir::new(local_dir)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let path = entry.path();
                    let relative = path.strip_prefix(local_dir).unwrap_or(path);

                    if entry.file_type().is_dir() {
                        continue; // dirs are created implicitly by tar entries
                    }

                    if entry.file_type().is_file() {
                        let contents =
                            std::fs::read(path).map_err(|e| AgentError::SandboxExecFailed {
                                run_id: 0,
                                sandbox_id: handle.sandbox_id.clone(),
                                reason: format!(
                                    "write_directory: failed to read {}: {}",
                                    path.display(),
                                    e
                                ),
                            })?;

                        let mut header = tar::Header::new_gnu();
                        header.set_size(contents.len() as u64);
                        header.set_mode(0o644);
                        // Set ownership for /home/temps paths
                        let full_target = format!("{}/{}", target_path, relative.display());
                        if full_target.starts_with("/home/temps") {
                            header.set_uid(1000);
                            header.set_gid(1000);
                        }
                        header.set_mtime(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                        );
                        header.set_cksum();

                        builder
                            .append_data(&mut header, relative, std::io::Cursor::new(&contents))
                            .map_err(|e| AgentError::SandboxExecFailed {
                                run_id: 0,
                                sandbox_id: handle.sandbox_id.clone(),
                                reason: format!(
                                    "write_directory: tar append failed for {}: {}",
                                    relative.display(),
                                    e
                                ),
                            })?;
                    }
                }

                builder
                    .finish()
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_directory: tar finish failed: {}", e),
                    })?;
            }
            buf
        };

        // Ensure target directory exists
        let mkdir = vec![
            "mkdir".to_string(),
            "-p".to_string(),
            target_path.to_string(),
        ];
        let _ = self.exec(handle, mkdir, HashMap::new(), None).await;

        let options = bollard::query_parameters::UploadToContainerOptionsBuilder::default()
            .path(target_path)
            .build();

        let body = bollard::body_full(tar_bytes.into());

        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            self.docker
                .upload_to_container(&handle.sandbox_id, Some(options), body),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    "write_directory: uploaded {} to container {}",
                    target_path,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_directory: upload to {} failed: {}", target_path, e),
            }),
            Err(_) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "write_directory: upload to {} timed out after 60s",
                    target_path
                ),
            }),
        }
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: super::KillSignal,
    ) -> Result<(), AgentError> {
        // Fresh exec running pkill. pkill exits 0 if something was killed,
        // 1 if nothing matched — both are success from our POV. Bounded by
        // a 10s timeout so we never replicate the phantom-stream hang.
        //
        // Use `pgrep` + `kill` instead of `pkill -f` to handle both busybox
        // and util-linux pkill variants uniformly.
        let sig_num = signal.as_number();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "pgrep -f {pattern_q} 2>/dev/null | xargs -r kill -{sig} 2>/dev/null; exit 0",
                pattern_q = shell_quote(pattern),
                sig = sig_num,
            ),
        ];

        let exec = self.exec(handle, cmd, HashMap::new(), None);
        match tokio::time::timeout(std::time::Duration::from_secs(10), exec).await {
            Ok(Ok(_)) => {
                tracing::debug!(
                    "kill_processes: sent signal {} to '{}' in {}",
                    sig_num,
                    pattern,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => {
                // Don't propagate — kill is best-effort. Log and move on.
                tracing::warn!(
                    "kill_processes: exec failed for '{}' in {}: {}",
                    pattern,
                    handle.sandbox_name,
                    e
                );
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    "kill_processes: timed out killing '{}' in {}",
                    pattern,
                    handle.sandbox_name
                );
                Ok(())
            }
        }
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        let container_name = Self::container_name(run_id);
        self.recover_container(&container_name).await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        let full_name = format!("{}{}", SANDBOX_NAME_PREFIX, container_name);
        self.recover_container(&full_name).await
    }

    fn name(&self) -> &str {
        "docker"
    }

    async fn is_available(&self) -> bool {
        self.docker.ping().await.is_ok()
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        let image_name = self.config.resolved_image();
        let ready = self.docker.inspect_image(&image_name).await.is_ok();
        Ok((ready, image_name))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        let image_name = self.config.resolved_image();

        // Remove existing image (force, in case containers reference it)
        if self.docker.inspect_image(&image_name).await.is_ok() {
            let opts = bollard::query_parameters::RemoveImageOptionsBuilder::new()
                .force(true)
                .build();
            let _ = self
                .docker
                .remove_image(&image_name, Some(opts), None)
                .await;
            tracing::info!("Removed old sandbox image {}", image_name);
        }

        // Rebuild locally — explicit rebuilds always build from the
        // generated Dockerfile so local changes (Claude backup, custom
        // packages, etc.) are picked up. Never pull from Hub here.
        self.build_image_locally(&self.config.runtime, None).await?;

        Ok(image_name)
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        let image_name = self.config.resolved_image();

        // Remove existing image
        if self.docker.inspect_image(&image_name).await.is_ok() {
            let _ = on_progress
                .send(format!("Removing old image {}...", image_name))
                .await;
            let opts = bollard::query_parameters::RemoveImageOptionsBuilder::new()
                .force(true)
                .build();
            let _ = self
                .docker
                .remove_image(&image_name, Some(opts), None)
                .await;
            tracing::info!("Removed old sandbox image {}", image_name);
        }

        // Rebuild locally with progress — never pull from Hub on explicit rebuild.
        self.build_image_locally(&self.config.runtime, Some(&on_progress))
            .await?;

        Ok(image_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_container_name_format() {
        assert_eq!(
            DockerSandboxProvider::container_name(42),
            "temps-sandbox-42"
        );
    }

    #[test]
    fn test_default_config() {
        let config = DockerSandboxConfig::default();
        assert_eq!(config.runtime, "node");
        assert_eq!(config.custom_image, "");
        assert_eq!(config.default_cpu_limit, 4.0);
        assert_eq!(config.default_memory_limit_mb, 8192);
        assert_eq!(config.network_mode, SANDBOX_NETWORK);
    }

    #[test]
    fn test_resolved_image_for_presets_stable_channel() {
        let v = SANDBOX_IMAGE_VERSION;
        // Test against the channel-explicit helper so the result doesn't
        // depend on whatever TEMPS_SANDBOX_CHANNEL happens to be set to in
        // the test runner's environment.
        for (runtime, expected) in [
            ("node", format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")),
            (
                "python",
                format!("ghcr.io/gotempsh/temps-sandbox-python:{v}"),
            ),
            ("rust", format!("ghcr.io/gotempsh/temps-sandbox-rust:{v}")),
            ("bun", format!("ghcr.io/gotempsh/temps-sandbox-bun:{v}")),
            ("go", format!("ghcr.io/gotempsh/temps-sandbox-go:{v}")),
            ("full", format!("ghcr.io/gotempsh/temps-sandbox-full:{v}")),
        ] {
            assert_eq!(
                image_name_for_runtime_in_channel(runtime, SandboxChannel::Stable),
                expected,
                "runtime={}",
                runtime
            );
        }
    }

    #[test]
    fn test_resolved_image_custom() {
        let config = DockerSandboxConfig {
            runtime: "custom".to_string(),
            custom_image: "my-registry/my-agent:v2".to_string(),
            ..Default::default()
        };
        assert_eq!(config.resolved_image(), "my-registry/my-agent:v2");
    }

    #[test]
    fn test_resolved_image_custom_empty_falls_back() {
        // Custom runtime with empty custom_image falls through to the
        // preset path, producing a "custom" preset name. (The actual
        // preset list rejects this at create time; the resolver doesn't
        // validate.) Channel suffix tracks env, so we test the helper
        // explicitly to avoid env-dependence.
        assert_eq!(
            image_name_for_runtime_in_channel("custom", SandboxChannel::Stable),
            format!("ghcr.io/gotempsh/temps-sandbox-custom:{SANDBOX_IMAGE_VERSION}")
        );
    }

    #[test]
    fn test_dockerfile_for_runtime_node() {
        let df = dockerfile_for_runtime("node");
        assert!(df.contains("FROM ubuntu:24.04"));
        assert!(
            df.contains("claude.ai/install.sh"),
            "must use native Claude installer"
        );
        assert!(df.contains("git"));
        assert!(df.contains("jq"), "jq must be installed for memory script");
        assert!(
            df.contains("nodesource"),
            "node runtime must install Node.js via NodeSource"
        );
    }

    #[test]
    fn test_all_runtimes_install_jq() {
        // The memory script requires jq. Every runtime preset must install it.
        for runtime in &["node", "bun", "python", "rust", "go", "full"] {
            let df = dockerfile_for_runtime(runtime);
            assert!(
                df.contains("jq"),
                "runtime {} dockerfile must install jq",
                runtime
            );
        }
    }

    #[test]
    fn test_dockerfile_for_runtime_python() {
        let df = dockerfile_for_runtime("python");
        assert!(df.contains("FROM python:3.12-slim"));
        assert!(df.contains("claude.ai/install.sh"));
        assert!(df.contains("uv"));
    }

    #[test]
    fn test_dockerfile_for_runtime_rust() {
        let df = dockerfile_for_runtime("rust");
        assert!(df.contains("FROM rust:1-slim"));
        assert!(df.contains("claude.ai/install.sh"));
    }

    #[test]
    fn test_dockerfile_for_runtime_bun() {
        let df = dockerfile_for_runtime("bun");
        assert!(df.contains("FROM oven/bun:latest"));
        assert!(df.contains("claude.ai/install.sh"));
    }

    #[test]
    fn test_dockerfile_for_runtime_go() {
        let df = dockerfile_for_runtime("go");
        assert!(df.contains("FROM golang:1.23-bookworm"));
        assert!(df.contains("claude.ai/install.sh"));
    }

    #[test]
    fn test_dockerfile_for_runtime_full() {
        let df = dockerfile_for_runtime("full");
        assert!(df.contains("FROM ubuntu:24.04"));
        assert!(df.contains("claude.ai/install.sh"));
        assert!(df.contains("python3"));
        assert!(df.contains("golang-go"));
        assert!(df.contains("nodejs"));
        assert!(df.contains("uv"));
    }

    #[test]
    fn test_dockerfile_for_unknown_runtime_defaults_to_node() {
        let df = dockerfile_for_runtime("unknown");
        assert!(df.contains("FROM ubuntu:24.04"));
    }

    #[test]
    fn test_image_name_for_runtime_shape() {
        // The env-reading wrapper picks a channel at runtime, so we don't
        // pin the exact tag — just check the structural shape every
        // returned name MUST have. The channel-explicit variants below
        // (test_image_name_for_runtime_*_channel) cover the exact strings.
        for runtime in &["node", "", "python", "bun", "rust", "go", "full"] {
            let img = image_name_for_runtime(runtime);
            assert!(
                img.starts_with("ghcr.io/gotempsh/temps-sandbox-"),
                "image must be GHCR-qualified: {img}"
            );
            assert!(img.contains(':'), "image must carry a tag: {img}");
        }
    }

    #[tokio::test]
    async fn test_docker_provider_recover_no_docker() {
        // If Docker isn't available, connect will fail — we test gracefully
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };

        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        let provider = DockerSandboxProvider::new(Arc::new(docker), DockerSandboxConfig::default());

        // Recover a run that doesn't exist
        let result = provider.recover(999999).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_docker_sandbox_e2e_lifecycle() {
        // Full lifecycle: create → exec → is_alive → recover → destroy
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping e2e test");
                return;
            }
        };
        let docker = Arc::new(docker);

        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping e2e test");
            return;
        }

        let config = DockerSandboxConfig::default();
        let provider = DockerSandboxProvider::new(docker.clone(), config);

        // Ensure the default image is built
        if let Err(e) = provider.ensure_image().await {
            println!("Cannot build sandbox image, skipping e2e test: {}", e);
            return;
        }

        let run_id = 99990; // Unlikely to conflict
        let work_dir = std::env::temp_dir().join(format!("sandbox-e2e-test-{}", run_id));
        let _ = std::fs::create_dir_all(&work_dir);
        std::fs::write(work_dir.join("test.txt"), "hello from test").unwrap();

        // 1. Create sandbox
        let create_config = SandboxCreateConfig {
            run_id,
            container_name_override: None,
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(512),
            pids_limit: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::from([("TEST_VAR".to_string(), "test_value".to_string())]),
            idle_timeout: Duration::from_secs(120),
        };

        let handle = provider.create(create_config).await.unwrap();
        assert!(handle.sandbox_name.contains("temps-sandbox-"));
        assert!(!handle.sandbox_id.is_empty());

        // 2. Verify it's alive
        assert!(provider.is_alive(&handle).await.unwrap());

        // 3. Execute a command — check the work dir is mounted
        let result = provider
            .exec(
                &handle,
                vec![
                    "cat".to_string(),
                    format!("{}/test.txt", CONTAINER_WORK_DIR),
                ],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello from test"));

        // 4. Execute with env vars
        let result = provider
            .exec(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo $MY_VAR".to_string(),
                ],
                HashMap::from([("MY_VAR".to_string(), "injected".to_string())]),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("injected"));

        // 5. Verify recovery — simulate finding existing container
        let recovered = provider.recover(run_id).await.unwrap();
        assert!(recovered.is_some());
        let recovered_handle = recovered.unwrap();
        assert_eq!(recovered_handle.sandbox_name, handle.sandbox_name);

        // 6. Destroy
        provider.destroy(&handle, true).await.unwrap();

        // 7. Verify it's gone
        assert!(!provider.is_alive(&handle).await.unwrap_or(false));
        let after_destroy = provider.recover(run_id).await.unwrap();
        assert!(after_destroy.is_none());

        // Cleanup
        let _ = std::fs::remove_dir_all(&work_dir);
    }

    #[tokio::test]
    async fn test_docker_sandbox_image_status() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        let docker = Arc::new(docker);

        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        let provider = DockerSandboxProvider::new(docker, DockerSandboxConfig::default());
        assert!(provider.is_available().await);

        let (_, image_name) = provider.image_status().await.unwrap();
        assert!(
            image_name.starts_with("ghcr.io/gotempsh/temps-sandbox-"),
            "got: {image_name}"
        );
    }

    #[tokio::test]
    async fn test_docker_sandbox_custom_runtime() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        let docker = Arc::new(docker);

        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        // Test that different runtimes produce different images
        let config = DockerSandboxConfig {
            runtime: "python".to_string(),
            ..Default::default()
        };
        let provider = DockerSandboxProvider::new(docker, config);

        let (_, image_name) = provider.image_status().await.unwrap();
        // image_status returns whatever channel the env points at; both
        // stable and beta are valid targets here, so we check the
        // structural shape rather than the exact string.
        assert!(
            image_name.starts_with("ghcr.io/gotempsh/temps-sandbox-python:"),
            "got: {image_name}"
        );
    }

    #[test]
    fn test_image_name_for_runtime_stable_channel() {
        let v = SANDBOX_IMAGE_VERSION;
        let stable = SandboxChannel::Stable;
        assert_eq!(
            image_name_for_runtime_in_channel("node", stable),
            format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("", stable),
            format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("python", stable),
            format!("ghcr.io/gotempsh/temps-sandbox-python:{v}")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("bun", stable),
            format!("ghcr.io/gotempsh/temps-sandbox-bun:{v}")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("full", stable),
            format!("ghcr.io/gotempsh/temps-sandbox-full:{v}")
        );
    }

    #[test]
    fn test_image_name_for_runtime_beta_channel() {
        let v = SANDBOX_IMAGE_VERSION;
        let beta = SandboxChannel::Beta;
        assert_eq!(
            image_name_for_runtime_in_channel("node", beta),
            format!("ghcr.io/gotempsh/temps-sandbox-node:{v}-beta")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("python", beta),
            format!("ghcr.io/gotempsh/temps-sandbox-python:{v}-beta")
        );
        assert_eq!(
            image_name_for_runtime_in_channel("full", beta),
            format!("ghcr.io/gotempsh/temps-sandbox-full:{v}-beta")
        );
    }

    #[test]
    fn test_runtime_from_image_name() {
        let v = SANDBOX_IMAGE_VERSION;
        // Round-trip: every runtime preset should be recoverable from the
        // image name we'd publish for it. This is what protects the
        // recovery code path that needs to figure out which Dockerfile to
        // regenerate when a missing image needs rebuilding.
        for runtime in &["node", "python", "rust", "bun", "go", "full"] {
            let stable = image_name_for_runtime_in_channel(runtime, SandboxChannel::Stable);
            assert_eq!(runtime_from_image_name(&stable), Some(*runtime));
            let beta = image_name_for_runtime_in_channel(runtime, SandboxChannel::Beta);
            assert_eq!(runtime_from_image_name(&beta), Some(*runtime));
        }
        // Custom images (anything outside our prefix) return None so the
        // recovery code falls back to a plain `docker pull` instead of
        // trying to materialize a Dockerfile for an unknown "runtime".
        assert_eq!(
            runtime_from_image_name("docker.io/library/alpine:3.19"),
            None
        );
        assert_eq!(runtime_from_image_name("temps-sandbox-node:0.1.0"), None);
        // Tag is optional in the parser — recovery still works even if
        // the input lost its tag somehow.
        assert_eq!(
            runtime_from_image_name(&format!("ghcr.io/gotempsh/temps-sandbox-node:{v}")),
            Some("node")
        );
    }

    #[test]
    fn test_sandbox_channel_default_is_stable() {
        // Whatever the developer's env happens to be, an unset / non-`beta`
        // value must always resolve to stable. The matcher only treats the
        // exact string "beta" as opt-in.
        assert_eq!(SandboxChannel::Stable.tag_suffix(), "");
        assert_eq!(SandboxChannel::Beta.tag_suffix(), "-beta");
    }

    #[tokio::test]
    async fn test_pull_fallback_on_missing_hub_image() {
        // Verify that ensure_image_for_runtime succeeds even when the
        // Docker Hub image doesn't exist — it should fall back to a
        // local build. We test by pointing at a non-existent hub image
        // (which is the normal case until images are published).
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        let docker = Arc::new(docker);
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());

        let local_image = format!("temps-sandbox-node:{SANDBOX_IMAGE_VERSION}");

        // Delete the local image first (if any) so we exercise the
        // pull→fail→build path. Ignore errors if it doesn't exist.
        let _ = docker
            .remove_image(
                &local_image,
                None::<bollard::query_parameters::RemoveImageOptions>,
                None,
            )
            .await;

        // This should succeed via fallback build even when the registry
        // image doesn't exist (or rate-limits the pull) — the local build
        // path is the safety net.
        let result = provider.ensure_image_for_runtime("node").await;
        assert!(
            result.is_ok(),
            "ensure_image should succeed via fallback build: {:?}",
            result.err()
        );

        // Image should now exist locally
        assert!(docker.inspect_image(&local_image).await.is_ok());
    }

    #[tokio::test]
    async fn test_kill_processes_term_and_kill() {
        // Integration test: create a sandbox, spawn a `sleep` process,
        // kill it with SIGTERM, verify it's gone. Then spawn another,
        // kill with SIGKILL, verify it's gone.
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        let docker = Arc::new(docker);
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        // If `temps serve` is running, it periodically cleans up sandbox
        // containers whose run_id isn't in the database. That kills our
        // test containers within ~1s. Skip the test to avoid flakes.
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: false,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec!["temps-sandbox-".to_string()],
                )])),
                ..Default::default()
            }))
            .await
            .unwrap_or_default();
        if !containers.is_empty() {
            println!(
                "temps serve is managing {} sandbox(es) — skipping kill_processes test to avoid flakes",
                containers.len()
            );
            return;
        }

        let config = DockerSandboxConfig::default();
        let provider = DockerSandboxProvider::new(docker.clone(), config);

        if let Err(e) = provider.ensure_image().await {
            println!("Cannot build sandbox image, skipping: {}", e);
            return;
        }

        let run_id = 99992;
        let work_dir = std::env::temp_dir().join(format!("sandbox-kill-test-{}", run_id));
        let _ = std::fs::create_dir_all(&work_dir);

        let create_config = SandboxCreateConfig {
            run_id,
            container_name_override: None,
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
        };

        let handle = provider.create(create_config).await.unwrap();

        // Verify the sandbox is actually alive before proceeding.
        // If `temps serve` is running, it may kill test containers whose
        // run_id isn't in the database — skip gracefully rather than fail.
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !provider.is_alive(&handle).await.unwrap_or(false) {
            println!("Container exited immediately after create — skipping kill_processes test");
            let _ = provider.destroy(&handle, true).await;
            let _ = std::fs::remove_dir_all(&work_dir);
            return;
        }

        // --- SIGTERM test ---
        // Write a script that daemonizes itself (double-fork pattern) so
        // the sleep outlives the Docker exec session.
        if let Err(e) = provider
            .write_file(
                &handle,
                "/tmp/spawn_sleep.sh",
                b"#!/bin/sh\n(sleep 9999 &)\nexit 0\n",
                0o755,
            )
            .await
        {
            println!(
                "Container died before write_file (temps serve cleanup?) — skipping: {}",
                e
            );
            let _ = provider.destroy(&handle, true).await;
            let _ = std::fs::remove_dir_all(&work_dir);
            return;
        }

        let _ = provider
            .exec(
                &handle,
                vec!["/tmp/spawn_sleep.sh".to_string()],
                HashMap::new(),
                None,
            )
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify sleep is running
        let result = provider
            .exec(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "pgrep -x sleep | wc -l".to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let count: i32 = result.stdout.trim().parse().unwrap_or(0);
        assert!(count > 0, "sleep should be running before kill");

        // Kill with SIGTERM via our typed enum.
        // Use the exact binary name "sleep" rather than a full-command
        // pattern to avoid matching pgrep itself.
        provider
            .kill_processes(&handle, "sleep", crate::sandbox::KillSignal::Term)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify sleep is gone
        let result = provider
            .exec(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "pgrep -x sleep | wc -l".to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let count: i32 = result.stdout.trim().parse().unwrap_or(0);
        assert_eq!(count, 0, "sleep should be gone after SIGTERM");

        // --- SIGKILL test ---
        // Spawn a new sleep for the SIGKILL test.
        let _ = provider
            .exec(
                &handle,
                vec!["/tmp/spawn_sleep.sh".to_string()],
                HashMap::new(),
                None,
            )
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify it started
        let result = provider
            .exec(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "pgrep -x sleep | wc -l".to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let count: i32 = result.stdout.trim().parse().unwrap_or(0);
        assert!(count > 0, "sleep should be running before SIGKILL");

        // SIGKILL — cannot be trapped
        provider
            .kill_processes(&handle, "sleep", crate::sandbox::KillSignal::Kill)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let result = provider
            .exec(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "pgrep -x sleep | wc -l".to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let count: i32 = result.stdout.trim().parse().unwrap_or(0);
        assert_eq!(count, 0, "sleep should be gone after SIGKILL");

        // Cleanup
        provider.destroy(&handle, true).await.unwrap();
        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// Regression test for the "stopped container auto-removed on
    /// recovery" bug. The earlier `recover_container` force-removed any
    /// container it found in stopped state, destroying the filesystem +
    /// volumes of any expired sandbox the user hadn't resumed yet.
    ///
    /// Invariant this pins down: after `stop` + `recover_by_name`, the
    /// container must still exist in Docker, a handle must be returned,
    /// and a subsequent `start` must succeed. If this test starts failing,
    /// the sandbox fleet is silently losing user data on every server
    /// restart that happens between stop and resume — fix the registry
    /// or provider, not the test.
    #[tokio::test]
    async fn recover_by_name_preserves_stopped_containers() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping recovery regression test");
                return;
            }
        };
        let docker = Arc::new(docker);
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping recovery regression test");
            return;
        }

        // If `temps serve` is running it will clean up any container whose
        // run_id isn't in its DB. That would kill this test. Skip to avoid
        // flakes — same guard the kill_processes test uses.
        let existing = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: false,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec!["temps-sandbox-".to_string()],
                )])),
                ..Default::default()
            }))
            .await
            .unwrap_or_default();
        if !existing.is_empty() {
            println!(
                "temps serve is managing {} sandbox(es) — skipping recovery regression test",
                existing.len()
            );
            return;
        }

        let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
        if provider.ensure_image().await.is_err() {
            println!("Cannot build sandbox image, skipping recovery regression test");
            return;
        }

        // Use a label-style name (what standalone sandboxes use) so we
        // exercise exactly the recover_by_name path, not recover(run_id).
        let label = "recover-test-abcdef";
        let run_id = 99994;
        let work_dir = std::env::temp_dir().join(format!("sandbox-recover-test-{}", run_id));
        let _ = std::fs::create_dir_all(&work_dir);

        let create_config = SandboxCreateConfig {
            run_id,
            container_name_override: Some(label.to_string()),
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
        };

        let handle = provider
            .create(create_config)
            .await
            .expect("create sandbox");

        // Stop the container — this is the state the expiration sweeper
        // leaves a sandbox in when the user doesn't resume before expiry.
        provider.stop(&handle).await.expect("stop container");

        // Sanity check: docker still knows about the container.
        let inspect = docker
            .inspect_container(
                &handle.sandbox_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("container still exists after stop");
        let running = inspect.state.and_then(|s| s.running).unwrap_or(true);
        assert!(!running, "container should be stopped, not running");

        // NOW the recovery call the bug lived in. Previously this
        // force-removed the stopped container and returned None. Post-fix
        // it must return a handle and leave the container alone.
        let recovered = provider
            .recover_by_name(label)
            .await
            .expect("recover_by_name does not error")
            .expect(
                "recover_by_name must return a handle for stopped \
                 containers — the old behavior silently deleted them, \
                 losing user data on every server restart between stop \
                 and resume",
            );
        assert_eq!(recovered.sandbox_name, handle.sandbox_name);

        // Container is still there AND still stopped (recovery is read-only).
        let inspect2 = docker
            .inspect_container(
                &handle.sandbox_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("container still exists after recovery");
        assert!(
            !inspect2.state.and_then(|s| s.running).unwrap_or(true),
            "recovery must not start the container — that's the caller's job"
        );

        // And it's actually usable: we can start it back up via the
        // recovered handle, which is exactly what resume_sandbox does.
        provider
            .start(&recovered)
            .await
            .expect("start using recovered handle");

        // Cleanup (only place that should ever call remove_container).
        provider.destroy(&recovered, true).await.unwrap();
        let _ = std::fs::remove_dir_all(&work_dir);
    }
}
