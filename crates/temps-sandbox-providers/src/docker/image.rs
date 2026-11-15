// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox images: the per-runtime Dockerfile, image names and release
//! channels, and ensuring an image is present (pull, or build locally).

use super::*;

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
    let (base, extra_packages, extra_run) = runtime_base_layer(runtime);
    let mut dockerfile = String::from(PTY_AGENT_BUILDER_STAGES);
    dockerfile.push_str(&base_tools_section(
        base,
        extra_packages,
        extra_run,
        bun_install_step(runtime),
    ));
    dockerfile.push_str(&sandbox_users_section());
    dockerfile.push_str(&ai_cli_section());
    dockerfile.push_str(&in_sandbox_agents_section());
    dockerfile
}

/// The base image of a runtime preset, the apt packages it adds, and a
/// shell command for extras its base image lacks. Anything that is not a
/// known preset gets the `node` layer.
fn runtime_base_layer(runtime: &str) -> (&'static str, &'static str, &'static str) {
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
    match runtime {
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
    }
}

/// The Bun install step for every preset whose base image lacks Bun, so
/// `bunx @temps-sdk/cli` works whichever runtime the user picked.
fn bun_install_step(runtime: &str) -> &'static str {
    // Install Bun on every non-bun runtime so `bunx @temps-sdk/cli` Just Works
    // regardless of which base image the user picked.
    if runtime == "bun" {
        ""
    } else {
        // Install to /usr/local/bun so it's on PATH for all users (including the
        // non-root `temps` user we create later).
        r#"RUN curl -fsSL https://bun.sh/install | BUN_INSTALL=/usr/local/bun bash \
    && ln -s /usr/local/bun/bin/bun /usr/local/bin/bun \
    && ln -s /usr/local/bun/bin/bunx /usr/local/bin/bunx
"#
    }
}

/// The build stages ahead of the sandbox image: the in-sandbox PTY agent
/// and the git credential helper and daemon, each compiled in its own
/// Rust stage so the final image carries no toolchain.
///
/// Stage 1: Build the temps-pty-agent binary from sources packed into
/// the build context by `pty_agent_bundle`. Isolated in its own stage so
/// the Rust toolchain doesn't bloat the final image. The binary is a
/// ~few-MB statically-linkable agent; we keep the default glibc dynamic
/// link since every base image here has a libc.
///
/// The host's terminal handler connects to /run/temps-pty/agent.sock
/// inside the container — sandbox-entrypoint.sh supervises the agent so
/// it's respawned if it ever dies.
const PTY_AGENT_BUILDER_STAGES: &str = r#"FROM rust:1-slim AS pty-agent-builder
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

/// The base image with the system packages every sandbox needs: apt
/// tools, the runtime extras, Bun, and the GitHub and GitLab CLIs.
///
/// Install tools as root, then create non-root user with sudo for package installs.
///
/// GitHub CLI (gh) and GitLab CLI (glab) are installed from their official
/// releases so the workspace AI can interact with PRs/MRs, issues, and CI.
///
/// PATH includes /home/temps/.local/bin globally so the binary is visible
/// from `docker exec`, tmux panes, and login shells alike — without this,
/// the bare-bash and tmux-wrapped paths would silently miss the installer
/// location and fall back to "claude not found".
fn base_tools_section(
    base: &str,
    extra_packages: &str,
    extra_run: &str,
    bun_install: &str,
) -> String {
    let home = SANDBOX_HOME;
    format!(
        r#"FROM {base}
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
"#
    )
}

/// The non-root sandbox user with scoped sudo, the credential daemon's
/// user, the workspace directory and the per-tab PTY socket directory.
fn sandbox_users_section() -> String {
    let user = SANDBOX_USER;
    let chown = SANDBOX_CHOWN;
    let work_dir = SANDBOX_WORK_DIR;
    let uid = SANDBOX_UID;
    format!(
        r#"RUN EXISTING_USER=$(getent passwd {uid} | cut -d: -f1) \
    && if [ -n "$EXISTING_USER" ] && [ "$EXISTING_USER" != "{user}" ]; then \
         (userdel -r "$EXISTING_USER" 2>/dev/null || userdel "$EXISTING_USER" 2>/dev/null || true); \
         (groupdel "$EXISTING_USER" 2>/dev/null || true); \
       fi \
    && useradd -m -s /bin/bash -u {uid} {user} \
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
# Mark the workspace as a trusted Git directory regardless of stat owner.
# Without this, `git pull` inside the sandbox fails with "dubious ownership"
# whenever the bind-mounted host work_dir comes in owned by a uid other
# than the container's `temps` user (uid 1000) — which happens whenever the
# host-side temps server runs as root, or when userns-remap is enabled on
# the host Docker daemon. Both situations are normal on production hosts.
# The post-start `chown -R` is the primary fix for filesystem semantics,
# but Git's safety check is independent of permissions, so we belt-and-
# suspenders it here. Scoped to /home/temps/workspace (not '*') so we
# don't blanket-trust every repo a user might mount or clone elsewhere.
RUN git config --system --add safe.directory {work_dir}
# /run/temps-pty holds one Unix socket per terminal tab (one per {{kind,tab}}
# pair). dtach creates these sockets on first attach; subsequent reconnects
# find the existing socket and re-attach instead of respawning the CLI. The
# directory lives in the container's tmpfs, so it's wiped on container
# restart — which is exactly the "launch once per sandbox lifetime" boundary
# we want.
RUN mkdir -p /run/temps-pty && chown {chown} /run/temps-pty && chmod 0700 /run/temps-pty
"#
    )
}

/// Claude Code, Codex and OpenCode installed as the sandbox user, with a
/// backup outside the home directory for a volume that masks it.
///
/// Claude CLI refuses --dangerously-skip-permissions when running as root,
/// and the native installer drops the binary in $HOME/.local/bin — both
/// reasons we install Claude as the `temps` user, not as root.
fn ai_cli_section() -> String {
    let user = SANDBOX_USER;
    let home = SANDBOX_HOME;
    format!(
        r#"# Install Claude Code via the official native installer, as the sandbox user.
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
"#
    )
}

/// The in-sandbox PTY agent, the git credential pipeline, the system git
/// config that routes through it, and the entrypoint.
fn in_sandbox_agents_section() -> String {
    let user = SANDBOX_USER;
    let work_dir = SANDBOX_WORK_DIR;
    format!(
        r#"USER root
# In-sandbox PTY agent: a single long-lived process that owns every
# interactive terminal in this container. See ADR-008 for rationale.
# The entrypoint supervises it — if it crashes, it's respawned. Existing
# images without this binary still work via the dtach fallback path in
# the terminal handler.
COPY --from=pty-agent-builder /build/target/release/temps-pty-agent /usr/local/bin/temps-pty-agent
COPY pty-agent/sandbox-entrypoint.sh /usr/local/bin/sandbox-entrypoint.sh
COPY pty-agent/vault-git.sh /usr/local/bin/git
RUN chmod 0755 /usr/local/bin/temps-pty-agent /usr/local/bin/sandbox-entrypoint.sh /usr/local/bin/git
LABEL sh.temps.service-access.protocol="1"
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
    && chmod 2750 /run/temps-git \
    && mkdir -p /etc/temps \
    && chown temps-git:git-users /etc/temps \
    && chmod 0710 /etc/temps \
    && touch /etc/temps/credential-daemon.env \
    && chown temps-git:temps-git /etc/temps/credential-daemon.env \
    && chmod 0600 /etc/temps/credential-daemon.env
# Mode 2750 on /run/temps-git/ sets the setgid bit on the directory.
# Without it, files (including the IPC socket) created inside inherit
# the *creating process's egid* — `temps-git`'s primary group, which
# is also `temps-git`. With setgid, new files inherit the parent dir's
# group (`git-users`) instead, which is what the user shell needs for
# `connect()` to succeed under mode 0660. Belt-and-braces: the daemon
# also explicitly chgrp's the socket on bind, so existing images
# without the setgid bit still recover after a daemon restart.
# System-wide git config: route every HTTPS git auth request through
# the credential helper. `useHttpPath=true` is mandatory — without it
# git omits the `path=` field, and the daemon can't tell what repo is
# being requested, so per-repo scoping degrades to refusal.
RUN git config --system credential.helper /usr/local/bin/temps-git-credential-helper \
    && git config --system credential.useHttpPath true
# Fail immediately when the credential daemon cannot supply credentials.
ENV GIT_TERMINAL_PROMPT=0
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
pub(super) enum SandboxChannel {
    Stable,
    Beta,
}

impl SandboxChannel {
    /// Channel for images no release manifest pinned (builds from source).
    /// Reads `TEMPS_SANDBOX_CHANNEL`; the default is beta, because a stable
    /// tag only exists once a release publishes it and a host that cached an
    /// older stable `:<version>` would keep running a sandbox image that
    /// predates the code (no service access mount, no proxy-aware terminal).
    /// Release builds never reach this: their manifest pins the image.
    /// `TEMPS_SANDBOX_CHANNEL=stable` opts a source build back into the
    /// canonical tag.
    #[allow(
        clippy::disallowed_methods,
        reason = "legacy runtime env config; follow-up: move to a DB-backed setting (P0.7 follow-up). TEMPS_SANDBOX_CHANNEL"
    )]
    fn from_env() -> Self {
        Self::from_setting(std::env::var("TEMPS_SANDBOX_CHANNEL").ok().as_deref())
    }

    pub(super) fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("stable") => Self::Stable,
            _ => Self::Beta,
        }
    }

    pub(super) fn tag_suffix(self) -> &'static str {
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
pub(super) fn image_name_for_runtime_in_channel(runtime: &str, channel: SandboxChannel) -> String {
    let suffix = channel.tag_suffix();
    let runtime = if runtime.is_empty() { "node" } else { runtime };
    format!("{SANDBOX_IMAGE_REGISTRY_PREFIX}{runtime}:{SANDBOX_IMAGE_VERSION}{suffix}")
}

/// Convenience wrapper that reads the channel from the environment.
pub fn image_name_for_runtime(runtime: &str) -> String {
    let embedded = match runtime {
        "" | "node" => temps_core::release_images::SANDBOX_NODE,
        "bun" => temps_core::release_images::SANDBOX_BUN,
        "python" => temps_core::release_images::SANDBOX_PYTHON,
        "rust" => temps_core::release_images::SANDBOX_RUST,
        "go" => temps_core::release_images::SANDBOX_GO,
        "full" => temps_core::release_images::SANDBOX_FULL,
        _ => None,
    };
    if let Some(image) = embedded {
        return image.to_string();
    }
    image_name_for_runtime_in_channel(runtime, SandboxChannel::from_env())
}

/// Inverse of `image_name_for_runtime`: extract the runtime preset name
/// from a fully-qualified GHCR image string. Returns `None` for anything
/// that isn't one of our preset images (custom images, garbage). Used by
/// the recovery path to figure out which Dockerfile to regenerate when
/// rebuilding a missing image.
pub(super) fn runtime_from_image_name(image: &str) -> Option<&str> {
    let rest = image.strip_prefix(SANDBOX_IMAGE_REGISTRY_PREFIX)?;
    Some(rest.split([':', '@']).next().unwrap_or(rest))
}

pub(super) fn selected_preset_runtime(image: &str) -> Option<&str> {
    runtime_from_image_name(image).filter(|runtime| image == image_name_for_runtime(runtime))
}

pub(super) fn ensure_mutable_rebuild_target(image: &str) -> Result<(), AgentError> {
    if image.contains("@sha256:") || image.starts_with("sha256:") {
        return Err(AgentError::ImmutableSandboxImageRebuild {
            image: image.to_string(),
        });
    }
    Ok(())
}

impl DockerSandboxProvider {
    /// Build the sandbox image if it doesn't exist.
    /// For preset runtimes, generates a Dockerfile dynamically.
    /// For custom images, assumes the image is already available (pull or pre-built).
    ///
    /// This is only ever called on demand — the first time a sandbox is
    /// created (see `create()`). It is intentionally NOT invoked at startup:
    /// pulling/building the image at boot would block or bog down the host
    /// before anyone has actually requested an agent run.
    pub async fn ensure_image(&self) -> Result<(), AgentError> {
        self.ensure_image_for_runtime(&self.config.runtime).await
    }

    /// Build a sandbox image for a specific runtime preset.
    pub(super) async fn ensure_image_for_runtime(&self, runtime: &str) -> Result<(), AgentError> {
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
                temps_docker_util::pull_image(&self.docker, temps_docker_util::PullSpec::new(img))
                    .await
                    .map_err(|e| AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!("Failed to pull custom image {}: {}", img, e.cause_text()),
                    })?;
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
                if image_name.contains("@sha256:") {
                    return Err(AgentError::SandboxProviderUnavailable {
                        provider: "docker".to_string(),
                        reason: format!(
                            "Failed to pull pinned runtime image {image_name}: {reason}"
                        ),
                    });
                }
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
    pub(super) async fn build_image_locally(
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
        temps_docker_util::pull_image(&self.docker, temps_docker_util::PullSpec::new(image))
            .await
            .map_err(|e| e.cause_text())
    }
}

#[cfg(test)]
mod golden_tests {
    //! Byte-for-byte captures of the Dockerfile every runtime preset
    //! generates. The sandbox image is built from this text, so any change
    //! to it, intended or not, must show up as a fixture diff. Regenerate a
    //! fixture with
    //! `cargo run -p temps-sandbox-providers --example print_dockerfile -- <runtime>`
    //! redirected into `tests/fixtures/golden/sandbox-dockerfile-<runtime>.txt`.

    use super::dockerfile_for_runtime;

    const GOLDEN: &[(&str, &str)] = &[
        (
            "node",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-node.txt"),
        ),
        (
            "bun",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-bun.txt"),
        ),
        (
            "python",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-python.txt"),
        ),
        (
            "rust",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-rust.txt"),
        ),
        (
            "go",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-go.txt"),
        ),
        (
            "full",
            include_str!("../../tests/fixtures/golden/sandbox-dockerfile-full.txt"),
        ),
    ];

    /// Fail at the first differing line, with both versions of it, instead
    /// of printing two 300-line strings.
    fn assert_matches_golden(runtime: &str, expected: &str, actual: &str) {
        if expected == actual {
            return;
        }
        let expected_lines: Vec<&str> = expected.split('\n').collect();
        let actual_lines: Vec<&str> = actual.split('\n').collect();
        let first_difference = expected_lines
            .iter()
            .zip(actual_lines.iter())
            .position(|(expected, actual)| expected != actual)
            .unwrap_or(expected_lines.len().min(actual_lines.len()));
        panic!(
            "the {runtime} Dockerfile differs from its golden fixture at line {}:\n  \
             expected: {:?}\n  actual:   {:?}\n(expected {} lines, got {})",
            first_difference + 1,
            expected_lines.get(first_difference),
            actual_lines.get(first_difference),
            expected_lines.len(),
            actual_lines.len(),
        );
    }

    #[test]
    fn every_runtime_preset_generates_its_golden_dockerfile() {
        for (runtime, golden) in GOLDEN {
            assert_matches_golden(runtime, golden, &dockerfile_for_runtime(runtime));
        }
    }

    #[test]
    fn an_unknown_or_empty_runtime_generates_the_node_dockerfile() {
        let (_, node) = GOLDEN[0];
        for runtime in ["", "unknown", "NODE", "node20"] {
            assert_matches_golden(runtime, node, &dockerfile_for_runtime(runtime));
        }
    }

    #[test]
    fn the_golden_fixtures_are_distinct_per_runtime() {
        // Guards the table above: a fixture copied under the wrong runtime
        // would otherwise pass both tests.
        for (index, (runtime, golden)) in GOLDEN.iter().enumerate() {
            for (other_runtime, other) in &GOLDEN[index + 1..] {
                assert_ne!(
                    golden, other,
                    "{runtime} and {other_runtime} fixtures are identical"
                );
            }
        }
    }
}
