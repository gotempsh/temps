use async_trait::async_trait;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::user::{SANDBOX_CHOWN, SANDBOX_HOME, SANDBOX_UID, SANDBOX_USER, SANDBOX_WORK_DIR};
use super::{
    ExecStream, OnStreamEventCallback, PtyAttachment, SandboxCreateConfig, SandboxExecResult,
    SandboxHandle, SandboxProvider, PTY_AGENT_SOCKET,
};
use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;

/// Container naming prefix — used for recovery after server restarts.
const SANDBOX_NAME_PREFIX: &str = "temps-sandbox-";

/// Naming prefix for the named volume backing a sandbox's `/home/temps`.
pub(crate) const HOME_VOLUME_PREFIX: &str = "temps-sandbox-home-";

/// Scheme marker in every home volume name this build generates.
///
/// This exists to make one specific attack impossible. Before the naming
/// fix, standalone sandboxes keyed their home volume on `sandboxes.id` and
/// agent runs keyed theirs on `agent_runs.id` — two independent sequences
/// sharing one namespace. On an upgraded host the standalone ones are
/// stranded (destroy no longer computes their name), and Docker attaches an
/// existing volume by name, so a later agent run whose id happened to match
/// would silently mount a *different user's* `/home/temps` — their Claude
/// credentials, shell history, and project state, read-write.
///
/// Every name we generate now carries this infix, so no name this build
/// produces can ever collide with a pre-fix volume. Stranded legacy volumes
/// become inert: nothing mounts them again, and the operator removes them
/// (see `HOME_VOLUME_LABEL`).
///
/// Cost of the change: a sandbox whose container is recreated across the
/// upgrade gets a fresh home once. Agent-run homes are ephemeral (purged on
/// destroy) and standalone containers are not recreated in place — stop,
/// start, and restart all reuse the existing container and its mounts — so
/// nothing a user is actively relying on is lost.
pub(crate) const HOME_VOLUME_SCHEME: &str = "v2-";

/// Label stamped on every home volume this provider creates.
///
/// Nothing in the server reads it — volumes are removed only by an explicit
/// sandbox destroy, which knows the exact name. It exists so an operator
/// can reclaim volumes this build strands (a destroy that failed to reach
/// the daemon, or a create that failed after the volume was made):
///
/// ```text
/// docker volume prune --filter label=sh.temps.sandbox.home
/// ```
///
/// That command does NOT cover volumes created before this build — those
/// were auto-created by the bind mount and carry no label. They are the
/// `v2-`-less numeric names, and are removed with:
///
/// ```text
/// docker volume ls -q --filter dangling=true \
///   | grep -E '^temps-sandbox-home-[0-9]+$' \
///   | xargs -r docker volume rm
/// ```
///
/// `dangling=true` is what makes that safe to run on a live host: a volume
/// still attached to a container is not listed.
const HOME_VOLUME_LABEL: &str = "sh.temps.sandbox.home";

/// Single-quote a string for safe embedding in a `sh -c` command line.
/// Handles embedded single quotes via the `'\''` idiom.
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

// ── Credential-scrubbing helpers (ADR-037 §4) ─────────────────────────────────
//
// These are module-level so they can be unit-tested without a Docker daemon.
// The production path in `take_snapshot` calls them; the test suite verifies
// the pattern list and the scrubbing logic independently.

/// Known-sensitive env-var key patterns from ADR-013 + ADR-037.
///
/// Matching is case-insensitive substring: a key is considered sensitive when
/// its uppercased form **contains** any of these patterns. This deliberately
/// catches variants like `MY_ANTHROPIC_API_KEY` and `GITHUB_TOKEN_READONLY`
/// without requiring an exhaustive allowlist.
///
/// **Security invariant**: this list is the single source of truth for which
/// env vars the scrubber strips. Adding a new secret kind to the sandbox API
/// must be accompanied by adding its pattern here. Keep this in sync with
/// every env var injected at sandbox creation time in:
///   - `crates/temps-agents/src/services/executor.rs`  (CLAUDE_CODE_OAUTH_TOKEN)
///   - `crates/temps-agents/src/handlers/trigger.rs`   (CLAUDE_CODE_OAUTH_TOKEN)
///   - `crates/temps-agents/src/services/sandbox_injector.rs`
pub(crate) const SENSITIVE_ENV_PATTERNS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "BITBUCKET_TOKEN",
    "GIT_TOKEN",
    "API_KEY",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AWS_SECRET",
    "AWS_ACCESS_KEY",
    "AZURE_CLIENT_SECRET",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CODEX_API_KEY",
    "OPENCODE_API_KEY",
    // OAuth tokens — covers CLAUDE_CODE_OAUTH_TOKEN injected by executor.rs
    // and trigger.rs for Claude subscription auth, and any future OAuth tokens.
    "OAUTH_TOKEN",
    "TEMPS_",
];

/// Returns `true` if `key` (case-insensitive) matches any sensitive pattern.
///
/// Called by `take_snapshot` for both the scrubbing step (zero-out the value)
/// and the verification step (reject if a non-empty value survives).
pub(crate) fn is_sensitive_env_key(key: &str) -> bool {
    let key_upper = key.to_uppercase();
    SENSITIVE_ENV_PATTERNS
        .iter()
        .any(|pat| key_upper.contains(pat))
}

/// Build Dockerfile-style `ENV KEY=` change instructions for every sensitive
/// entry found in a `KEY=VALUE` env list.
///
/// For each entry whose key matches [`is_sensitive_env_key`], produces a
/// `"ENV KEY="` string (empty value). These strings are passed to
/// `CommitContainerOptionsBuilder::changes()`, which maps to Docker's
/// `changes` query parameter — the **only** mechanism that actually overwrites
/// env-var values in the committed image's `Config.Env`.
///
/// **Why zeroing rather than removal:** Docker's commit API has no mechanism
/// to delete an env entry; it can only overwrite the value. `ENV KEY=` sets
/// the key to an empty string in the committed image. This was verified
/// against a real Docker daemon using `docker commit --change 'ENV KEY='`.
/// The `ContainerConfig` body's `env` field (the previous approach) is
/// silently ignored by the Docker Engine and has zero effect on the committed
/// image — a confirmed no-op, not a design choice.
///
/// Non-sensitive entries are not included in the result — only the change
/// instructions for the keys that need scrubbing are returned.
pub(crate) fn build_env_scrub_changes(env: &[String]) -> Vec<String> {
    env.iter()
        .filter_map(|kv| {
            let key = kv.split('=').next().unwrap_or("");
            if is_sensitive_env_key(key) {
                Some(format!("ENV {}=", key))
            } else {
                None
            }
        })
        .collect()
}

/// Returns the keys of any env entries that are sensitive AND have a non-empty
/// value after `=` (i.e. were not successfully zeroed by the scrubbing step).
///
/// A sensitive key with an **empty** value (`KEY=`) has been successfully
/// zeroed by `docker commit --change 'ENV KEY='` and is NOT a survivor.
/// A sensitive key with a **non-empty** value (`KEY=actual-secret`) means
/// the Docker `changes` mechanism was bypassed or failed and the value leaked
/// into the committed image — this is the real failure condition.
///
/// Called after `docker commit` to verify the scrubbing worked. An empty
/// return means the image config is clean. A non-empty return triggers an
/// abort: the staged image is removed and `take_snapshot` returns an error.
pub(crate) fn find_surviving_sensitive_keys(env: &[String]) -> Vec<String> {
    env.iter()
        .filter_map(|kv| {
            let mut parts = kv.splitn(2, '=');
            let key = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            // A sensitive key with an empty value has been zeroed by the
            // `ENV KEY=` change instruction — that is the expected post-commit
            // state and is NOT a failure. Only a non-empty value means the
            // secret survived into the committed image.
            if is_sensitive_env_key(key) && !value.is_empty() {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect()
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
    let uid = SANDBOX_UID;

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
RUN EXISTING_USER=$(getent passwd {uid} | cut -d: -f1) \
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

    /// Run a command as root inside a freshly-started sandbox container,
    /// drain its output, and return its exit code. Used for the post-start
    /// fix-up steps (chown of home + work dir, AI-CLI restore) where we
    /// need to know whether the command actually succeeded — earlier
    /// versions used `let _ = start_exec(...)` and silently masked
    /// failures, leaving sandboxes with root-owned workspaces that broke
    /// `git pull` ("dubious ownership") and writes from the temps user.
    async fn run_root_exec(
        &self,
        container_id: &str,
        run_id: i32,
        label: &str,
        cmd: Vec<String>,
    ) -> Result<i64, AgentError> {
        let exec = self
            .docker
            .create_exec(
                container_id,
                bollard::models::ExecConfig {
                    user: Some("0:0".to_string()),
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to create {} exec: {}", label, e),
            })?;

        let output = self
            .docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to start {} exec: {}", label, e),
            })?;

        // Drain the output stream so the exec actually runs to completion;
        // bollard tears the exec down when the stream is dropped, which
        // can race with the underlying command on slow hosts.
        let mut stderr_tail = String::new();
        if let StartExecResults::Attached { mut output, .. } = output {
            while let Some(chunk) = output.next().await {
                if let Ok(LogOutput::StdErr { message }) = chunk {
                    let text = String::from_utf8_lossy(&message);
                    stderr_tail.push_str(&text);
                }
            }
        }

        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1);

        if exit_code != 0 {
            let trimmed = stderr_tail.trim();
            tracing::error!(
                "Sandbox {} step '{}' exited {} (stderr: {})",
                container_id,
                label,
                exit_code,
                if trimmed.is_empty() {
                    "<empty>"
                } else {
                    trimmed
                }
            );
        }

        Ok(exit_code)
    }

    /// Run the per-container ownership-normalization steps: chown -R
    /// /home/temps and /home/temps/workspace to temps:temps.
    ///
    /// Idempotent and safe to call repeatedly. We invoke it on every create
    /// AND on every recover — recovered containers might have been left
    /// root-owned by an earlier server that crashed before chown ran, or by
    /// host-side git operations performed after the first chown.
    ///
    /// chown-work is treated as **fatal**: without it the sandbox user can
    /// never write under /home/temps/workspace, so the entire run will fail
    /// with confusing "Permission denied" errors. Better to abort startup
    /// with a clear message. chown-home is best-effort: it operates on the
    /// home volume which may legitimately contain files we don't need to
    /// touch (e.g. socket files, FIFOs) and whose individual failures don't
    /// break workflows.
    async fn normalize_ownership(&self, container_id: &str, run_id: i32) -> Result<(), AgentError> {
        // Sandbox home: best-effort. Some files in the named volume (sockets,
        // FIFOs, files owned by uids the kernel won't let us touch even with
        // CAP_CHOWN) can make a recursive chown exit non-zero while still
        // doing useful work. Don't fail the whole sandbox over that.
        let home_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "chown-home",
                vec![
                    "chown".to_string(),
                    "-R".to_string(),
                    SANDBOX_CHOWN.to_string(),
                    SANDBOX_HOME.to_string(),
                ],
            )
            .await?;
        if home_exit != 0 {
            tracing::warn!(
                "Sandbox {} chown-home returned {} — best-effort, continuing. \
                 Investigate if subsequent home-dir writes fail.",
                container_id,
                home_exit
            );
        }

        // Work dir: bind-mounted from the host where the temps server
        // (often root) ran `git clone`, so files arrive owned by uid 0.
        // Without this chown the sandbox user can't `mkdir reports/`,
        // can't `git commit`, can't open lockfiles. This is the exact
        // root cause of "mkdir: cannot create directory '/home/temps/
        // workspace/reports': Permission denied".
        //
        // We can't hard-fail on non-zero exit: some bind-mount backends
        // (macOS Docker Desktop's virtiofs, userns-remap on Linux) return
        // EPERM for chown even when the operation is logically a no-op.
        // Instead we verify the result with `stat` and only error if the
        // ownership didn't actually take. That way prod Linux failures
        // (real permission problem) abort startup with a clear message,
        // while dev-machine warnings (chown says no but ownership is
        // already correct or doesn't matter) flow through.
        let chown_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "chown-work",
                vec![
                    "chown".to_string(),
                    "-R".to_string(),
                    SANDBOX_CHOWN.to_string(),
                    CONTAINER_WORK_DIR.to_string(),
                ],
            )
            .await?;

        // Probe: can the sandbox user actually write into the workspace?
        // `su temps -c 'touch ...'` is the truest test — if this fails,
        // every subsequent workflow command will fail too, so refuse to
        // hand back a broken sandbox.
        let probe_path = format!("{}/.temps-write-probe", CONTAINER_WORK_DIR);
        let probe_exit = self
            .run_root_exec(
                container_id,
                run_id,
                "write-probe",
                vec![
                    "su".to_string(),
                    SANDBOX_USER.to_string(),
                    "-c".to_string(),
                    format!("touch {} && rm {}", probe_path, probe_path),
                ],
            )
            .await?;

        if probe_exit != 0 {
            return Err(AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!(
                    "sandbox user '{}' cannot write to {} (chown-work exit {}, \
                     write-probe exit {}). The bind-mounted workspace is owned by \
                     a uid the container can't normalize — usually because the \
                     host process that cloned the repo ran as root and the \
                     container lacks CAP_CHOWN, or userns-remap is rewriting uids \
                     in a way chown can't follow. Workflows would all fail with \
                     'Permission denied' so we're aborting startup instead.",
                    SANDBOX_USER, CONTAINER_WORK_DIR, chown_exit, probe_exit
                ),
            });
        }

        if chown_exit != 0 {
            tracing::warn!(
                "Sandbox {} chown-work exited {} but write-probe succeeded — \
                 likely a bind-mount backend (macOS Docker / userns-remap) where \
                 chown reports EPERM but the resulting ownership is workable.",
                container_id,
                chown_exit
            );
        }

        Ok(())
    }

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

    /// Ensure the sandbox bridge network exists and apply iptables egress filtering.
    ///
    /// After Docker creates the `temps-sandbox-net` bridge, this method installs a
    /// dedicated `TEMPS_SANDBOX_EGRESS` iptables chain that blocks outbound traffic
    /// from sandbox containers to RFC-1918 ranges (10/8, 172.16/12, 192.168/16),
    /// link-local (169.254/16), and loopback (127/8).  This prevents a compromised
    /// sandbox from reaching the host gateway, the control plane API (`:8080`), the
    /// database (`:5432`), or cloud-metadata endpoints while still allowing full
    /// public-internet access needed for npm, pip, cargo, and GitHub.
    ///
    /// The filter is **best-effort**: if iptables is unavailable (rootless Docker,
    /// macOS Docker Desktop, missing CAP_NET_ADMIN) a `WARN` is logged and sandbox
    /// creation continues normally. On macOS the filter is automatically skipped
    /// because iptables is not available on the host (Docker Desktop runs inside a
    /// Linux VM that already isolates sandbox traffic from the macOS host network).
    async fn ensure_network(&self) -> Result<(), AgentError> {
        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|e| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("Failed to list networks: {}", e),
            })?;

        let existing = networks
            .iter()
            .find(|n| n.name.as_ref() == Some(&self.config.network_mode));

        let network_id: Option<String> = if existing.is_none()
            && self.config.network_mode != "none"
            && self.config.network_mode != "host"
        {
            tracing::info!("Creating sandbox network: {}", self.config.network_mode);
            let create_opts = bollard::models::NetworkCreateRequest {
                name: self.config.network_mode.clone(),
                driver: Some("bridge".to_string()),
                internal: Some(false), // Allow outbound (Claude CLI needs API access)
                ..Default::default()
            };
            let resp = self.docker.create_network(create_opts).await.map_err(|e| {
                AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("Failed to create network: {}", e),
                }
            })?;
            Some(resp.id)
        } else {
            existing.and_then(|n| n.id.clone())
        };

        // Apply iptables egress filter to block sandbox access to RFC-1918 ranges,
        // link-local (169.254/16), and loopback (127/8).  This prevents a compromised
        // sandbox from reaching the host gateway, the control plane API, the database,
        // or cloud-metadata endpoints while still allowing full public-internet access
        // needed for npm, pip, cargo, and GitHub.
        //
        // Operator note: on systems using iptables-nft (Debian 12+, Ubuntu 22.04+)
        // the kernel module name is `iptables` but the userspace binary may be
        // `iptables-legacy`; ensure the `iptables` command resolves to the nft-compat
        // shim or install `iptables-legacy` if this step reports permission errors
        // despite CAP_NET_ADMIN being present.
        if let Some(ref id) = network_id {
            apply_sandbox_egress_filter(&self.docker, id).await;
        }

        Ok(())
    }

    fn container_name(run_id: i32) -> String {
        format!("{}{}", SANDBOX_NAME_PREFIX, run_id)
    }

    /// The container name and home-volume name for a create request.
    ///
    /// `create` must take both from here rather than computing either
    /// itself. The leak this replaced came from exactly that: create
    /// derived the volume from `config.run_id` while destroy derived it
    /// from the container name, and nothing forced the two to agree. With
    /// one function owning the pair, a future change to either name has to
    /// go through a single place, and `sandbox_names_agree_for_*` fails if
    /// they ever diverge again — without needing a Docker daemon.
    fn sandbox_names(config: &SandboxCreateConfig) -> (String, String) {
        let container_name = config
            .container_name_override
            .clone()
            .map(|id| format!("{}{}", SANDBOX_NAME_PREFIX, id))
            .unwrap_or_else(|| Self::container_name(config.run_id));
        let home_volume_name = Self::home_volume_name(&container_name);
        (container_name, home_volume_name)
    }

    /// The `HostConfig.binds` for a sandbox container: the host work dir at
    /// `/workspace`, and the home volume at `/home/temps`.
    ///
    /// Pure, and separate from `create`, so a test can assert that the
    /// volume the container actually mounts is the one `destroy` will
    /// remove — without needing a Docker daemon. The original leak lived
    /// exactly here: `create` built this bind from one name while `destroy`
    /// computed another, and only an e2e could see it.
    fn container_binds(host_work_dir: &str, home_volume_name: &str) -> Vec<String> {
        vec![
            format!("{}:{}", host_work_dir, CONTAINER_WORK_DIR),
            format!("{}:{}", home_volume_name, SANDBOX_HOME),
        ]
    }

    /// Name of the named volume holding a sandbox's `/home/temps`.
    ///
    /// Derived from the **container** name, never from `run_id` directly.
    /// That distinction is the whole point: agent runs are named
    /// `temps-sandbox-<run_id>` while standalone sandboxes override the
    /// suffix with their opaque `public_id` label, so keying the volume on
    /// `run_id` made create and destroy disagree for every standalone
    /// sandbox — create made `temps-sandbox-home-<row.id>`, destroy tried
    /// to remove `temps-sandbox-home-<hex>`, and the real volume leaked on
    /// the host forever. Both sides now go through this one function, so
    /// they cannot drift again.
    ///
    /// Deriving from the container name also removes a cross-tenant
    /// collision: standalone sandbox row 5 and agent run 5 used to share
    /// `temps-sandbox-home-5`, i.e. one user's `~/.claude` credentials and
    /// shell history mounted into another user's sandbox. `HOME_VOLUME_SCHEME`
    /// is what makes that unreachable rather than merely unlikely — it keeps
    /// every generated name out of the pre-fix namespace, so a stranded
    /// legacy volume can never be picked up by a new sandbox either.
    fn home_volume_name(container_name: &str) -> String {
        super::home_volume_name_for_label(
            container_name
                .strip_prefix(SANDBOX_NAME_PREFIX)
                .unwrap_or(container_name),
        )
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

                // Re-run ownership normalization on the recovered container.
                // Without this, any container created before the original
                // chown landed (or whose host workspace was rewritten by a
                // later host-side git operation) stays root-owned, and the
                // sandbox user gets "Permission denied" on every write into
                // /home/temps/workspace.
                //
                // Best-effort if the container isn't running: starting it
                // here would change recovery semantics, so we just log and
                // skip — the next create()/start path will fix it.
                if running && !container_id.is_empty() {
                    // run_id is only used for error context; we don't have
                    // it on this path, so pass 0.
                    if let Err(e) = self.normalize_ownership(&container_id, 0).await {
                        tracing::warn!(
                            "Sandbox {} recovered but ownership normalization failed: {} \
                             — workspace writes may still fail",
                            container_name,
                            e
                        );
                    }
                }

                Ok(Some(SandboxHandle {
                    sandbox_id: container_id,
                    sandbox_name: container_name.to_string(),
                    work_dir: PathBuf::from(CONTAINER_WORK_DIR),
                    backend: super::SandboxBackend::Docker,
                    image: String::new(),
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

        let (container_name, home_volume_name) = Self::sandbox_names(&config);

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
        // `home_volume_name` came from `sandbox_names` above, alongside the
        // container name — `destroy` re-derives it from the container name
        // via `home_volume_name`, which is what keeps the two in sync.
        let binds = Self::container_binds(&host_work_dir, &home_volume_name);

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
            // Minimum caps required for the post-start ownership-normalization
            // dance (see `normalize_ownership`) and for `su temps -c ...` to
            // drop privileges into the sandbox user:
            //   CHOWN          — chown the home volume + bind-mounted workdir
            //   FOWNER         — chmod/utime files we don't own (recover path)
            //   DAC_OVERRIDE   — read/traverse dirs whose mode bits forbid it
            //                    after a prior chown left them o-rwx; without
            //                    this, even root gets EACCES on readdir() of
            //                    /home/temps when a previous run scoped it
            //   SETUID/SETGID  — `su` needs both to call setuid/setgroups when
            //                    handing the write-probe (and every later
            //                    workflow step) off to the temps user. Without
            //                    them `su` fails with "cannot set groups:
            //                    Operation not permitted" and the sandbox is
            //                    declared broken at startup.
            cap_add: Some(vec![
                "CHOWN".to_string(),
                "FOWNER".to_string(),
                "DAC_OVERRIDE".to_string(),
                "SETUID".to_string(),
                "SETGID".to_string(),
            ]),
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

        // Create the home volume explicitly, labelled, rather than letting
        // the bind auto-create it unlabelled — see HOME_VOLUME_LABEL for why
        // the label earns its extra call. Idempotent: Docker returns an
        // existing volume unchanged, which is what recreating a sandbox over
        // a surviving home relies on.
        //
        // Deliberately here rather than at the top of `create`: everything
        // that can fail cheaply and repeatedly (image resolution, a cold
        // pull of a caller-supplied image) has already happened, so a caller
        // looping failed creates can't mint a volume per attempt. A failure
        // after this point still strands an empty one, which is what the
        // label-filtered prune is for.
        if let Err(e) = self
            .docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(home_volume_name.clone()),
                labels: Some(HashMap::from([(
                    HOME_VOLUME_LABEL.to_string(),
                    "true".to_string(),
                )])),
                ..Default::default()
            })
            .await
        {
            // Non-fatal: the bind still auto-creates the volume. The only
            // loss is the label, i.e. this one volume won't appear in an
            // operator's label-filtered prune.
            tracing::warn!(
                "Could not pre-create labelled home volume {}: {} — continuing",
                home_volume_name,
                e
            );
        }

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

        // Normalize ownership of /home/temps (named volume) and
        // /home/temps/workspace (bind-mount). Both inherit uids from outside
        // the container — the home volume from Docker's anonymous-volume
        // initialisation, the workspace from the host process that ran
        // `git clone`. Without this, the sandbox user can't write either
        // tree and every subsequent command fails. Strict variant: a chown
        // failure aborts container creation rather than leaving a broken
        // sandbox that mints "Permission denied" for the rest of the run.
        self.normalize_ownership(&container.id, config.run_id)
            .await?;

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
            // Best-effort: a missing AI-CLI restore shouldn't fail the
            // whole sandbox creation (the bind-mount + chown above are
            // already what unblocks `git pull` and the sandbox terminal),
            // so we log the exit code via run_root_exec but ignore
            // non-zero results. The helper itself emits a tracing::error
            // line so the failure is still visible in logs.
            if let Err(e) = self
                .run_root_exec(
                    &container.id,
                    config.run_id,
                    "ai-cli-restore",
                    vec!["sh".to_string(), "-c".to_string(), restore_script],
                )
                .await
            {
                tracing::warn!(
                    "Sandbox {} ai-cli-restore step failed to launch: {} — continuing",
                    container.id,
                    e
                );
            }
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
            backend: super::SandboxBackend::Docker,
            image: image.to_string(),
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
            let home_volume_name = Self::home_volume_name(&handle.sandbox_name);
            if let Err(e) = self
                .docker
                .remove_volume(
                    &home_volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                // Not fatal — the container is already gone, so the sandbox
                // is destroyed either way. Nothing sweeps up afterwards, so
                // the volume is stranded until an operator reclaims it; it
                // carries HOME_VOLUME_LABEL for exactly that.
                tracing::warn!(
                    "Failed to remove sandbox home volume {} (may not exist): {}",
                    home_volume_name,
                    e
                );
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

    /// Bridge to the in-sandbox PTY agent, exactly as ADR-008 §Host-side
    /// Bridge specifies: a `docker exec` running `socat` that relays the
    /// exec's stdio to the agent's unix socket.
    ///
    /// Nothing long-lived runs inside this exec — `socat` exits when its
    /// stdin closes, and the exec dies with it. The only persistent
    /// in-container process is the agent itself, which is what keeps a
    /// `claude` session alive across a dropped connection.
    ///
    /// Runs as `SANDBOX_USER` because the socket is mode 0600 owned by that
    /// user. Executing as root would work but would break the "same trust
    /// boundary as the sandbox user" property the ADR relies on.
    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        let exec_failed = |reason: String| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason,
        };

        let exec_config = bollard::models::ExecConfig {
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            // No `tty: true` here. The PTY lives *inside* the agent; this
            // exec is a plain binary pipe carrying framed protocol bytes.
            // Asking Docker for a TTY would mangle them with ONLCR.
            tty: Some(false),
            cmd: Some(vec![
                "socat".to_string(),
                "-".to_string(),
                format!("UNIX-CONNECT:{}", PTY_AGENT_SOCKET),
            ]),
            user: Some(SANDBOX_USER.to_string()),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&handle.sandbox_id, exec_config)
            .await
            .map_err(|e| exec_failed(format!("failed to create PTY attach exec: {}", e)))?;

        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| exec_failed(format!("failed to start PTY attach exec: {}", e)))?;

        match started {
            StartExecResults::Attached { output, input } => {
                let sandbox_id = handle.sandbox_id.clone();
                // Docker frames the exec's stdout/stderr; the agent's bytes
                // arrive as StdOut. StdErr here is socat complaining (e.g.
                // the socket is missing because the image predates the
                // agent), which must surface as an error rather than be
                // silently mixed into the protocol stream.
                let output = output.map(move |chunk| match chunk {
                    Ok(LogOutput::StdOut { message }) => Ok(message),
                    Ok(LogOutput::Console { message }) => Ok(message),
                    Ok(LogOutput::StdErr { message }) => Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: sandbox_id.clone(),
                        reason: format!(
                            "PTY agent relay error: {}",
                            String::from_utf8_lossy(&message).trim()
                        ),
                    }),
                    Ok(LogOutput::StdIn { .. }) => Ok(bytes::Bytes::new()),
                    Err(e) => Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: sandbox_id.clone(),
                        reason: format!("PTY attach stream failed: {}", e),
                    }),
                });
                Ok(PtyAttachment {
                    output: Box::pin(output),
                    input,
                })
            }
            StartExecResults::Detached => Err(exec_failed(
                "PTY attach exec returned detached; expected an attached stream".to_string(),
            )),
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

    fn supports_backend(&self, backend: super::SandboxBackend) -> bool {
        matches!(backend, super::SandboxBackend::Docker)
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

    // ── Snapshot: take ────────────────────────────────────────────────────────

    /// Capture the current state of `handle` as a content-addressed tarball.
    ///
    /// **Security contract (ADR-037 §4):** The caller (`SnapshotService`) is
    /// responsible for shredding `/etc/temps/credential-daemon.env` via
    /// `exec_as_root` **before** stopping the sandbox and calling this method.
    /// Any failure in that step must abort the snapshot at the service layer.
    ///
    /// This method then executes two additional scrubbing steps:
    ///
    /// 1. Inspect the stopped container's `Config.Env` and zero every
    ///    known-sensitive env-var value in the committed image config via
    ///    Docker's `--change "ENV KEY="` mechanism. Each sensitive key is set
    ///    to an empty value (`KEY=`) — this is the only mechanism the Docker
    ///    commit API actually supports; deletion is not possible. Verified
    ///    against a real Docker daemon. The `ContainerConfig` body's `env`
    ///    field (the previous approach) is silently ignored by the Docker
    ///    Engine and has no effect on the committed image — confirmed no-op.
    /// 2. Inspect the committed image's `Config.Env` and reject the snapshot —
    ///    removing the committed image and returning `SandboxExecFailed` — if
    ///    any known-sensitive key has a non-empty value (i.e. the zeroing
    ///    did not take effect for that key).
    ///
    /// The sandbox must be stopped by the caller before this is called for
    /// filesystem consistency.
    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
    ) -> Result<super::SnapshotArtifact, AgentError> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;

        let container_id = &handle.sandbox_id;

        // ── Validate label early, before any file I/O ─────────────────────────
        // An empty label after sanitization produces an invalid Docker tag.
        // Validate here so the rename to the final path never orphans a tarball
        // because a later tag call fails.
        if let Some(ref lbl) = label {
            if lbl.trim().is_empty() {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: container_id.clone(),
                    reason: "snapshot: label must not be empty when provided \
                             (empty label produces an invalid Docker image tag; \
                             omit the label field to use the content digest instead)"
                        .to_string(),
                });
            }
        }

        // ── Step 1: collect and scrub known-sensitive env vars ────────────────
        // Inspect the stopped container's Config.Env to find injected vars.
        // The sensitive-pattern list and scrubbing helpers are module-level
        // functions (see `SENSITIVE_ENV_PATTERNS`, `scrub_env_vars`, and
        // `find_surviving_sensitive_keys`) so they are unit-testable without
        // a live Docker daemon.
        let container_inspect = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!("snapshot: failed to inspect container: {}", e),
            })?;

        // Collect env vars set on the container (from the original create call
        // via ContainerCreateBody.env).
        let container_env: Vec<String> = container_inspect
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .map(|env| env.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        // Build Dockerfile-style "ENV KEY=" change instructions for each
        // sensitive key, then count for telemetry.
        //
        // IMPORTANT: The ContainerConfig body's `env` field is NOT used for
        // scrubbing. Live testing against a real Docker daemon confirmed that
        // passing `ContainerConfig { env: Some(scrubbed_env), .. }` to
        // `commit_container` has zero effect — the Docker Engine silently
        // ignores/merges the body's Env field rather than replacing the
        // committed image's Config.Env. The `changes` query parameter (i.e.
        // `docker commit --change 'ENV KEY='`) is the only mechanism that
        // actually zeroes values in the committed image, verified directly.
        let scrub_changes = build_env_scrub_changes(&container_env);
        let scrubbed_key_count = scrub_changes.len();

        // ── Step 1b: commit the container with sensitive env vars zeroed ───────
        // The snapshot image tag is `temps-snapshot/<container_id_short>:latest`
        // during the commit phase; we rename it to the public_id tag after
        // digest verification. Using the container_id avoids a race if two
        // snapshots are taken concurrently.
        let short_id = &container_id[..container_id.len().min(12)];
        let commit_tag = format!("temps-snapshot-staging/{}", short_id);

        tracing::info!(
            container_id = %container_id,
            commit_tag = %commit_tag,
            scrubbed_key_count = %scrubbed_key_count,
            "snapshot: committing container image"
        );

        // Pass `ENV KEY=` change instructions via the `changes` parameter —
        // this is Docker's `--change` flag, which is the only API mechanism
        // that actually overwrites env-var values in the committed image's
        // Config.Env. Each sensitive key is set to an empty value; Docker's
        // commit API cannot delete entries, only overwrite them.
        //
        // bollard's `CommitContainerOptionsBuilder::changes()` takes a single
        // `&str`; multiple Dockerfile instructions are separated by newlines.
        let changes_str = scrub_changes.join("\n");
        let mut commit_opts_builder =
            bollard::query_parameters::CommitContainerOptionsBuilder::new()
                .container(container_id.as_str())
                .repo(commit_tag.as_str())
                .tag("latest");
        if !changes_str.is_empty() {
            commit_opts_builder = commit_opts_builder.changes(&changes_str);
        }
        let commit_opts = commit_opts_builder.build();

        // The ContainerConfig body is passed as required by bollard's API
        // signature but left at defaults — its `env` field does nothing (the
        // Docker Engine ignores it; only the `changes` query parameter above
        // is effective, as confirmed against a live Docker daemon).
        let config_override = bollard::models::ContainerConfig::default();

        let commit_resp = self
            .docker
            .commit_container(commit_opts, config_override)
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!("snapshot: docker commit failed: {}", e),
            })?;

        // IdResponse.id is a plain String (not Option<String>) in bollard 0.20.
        let committed_image_id = commit_resp.id;

        // ── Step 3: verify no sensitive key survived in the committed image ───
        let committed_inspect = self
            .docker
            .inspect_image(&committed_image_id)
            .await
            .map_err(|e| {
                // Clean up the staged image before returning the error
                let docker = self.docker.clone();
                let img = committed_image_id.clone();
                tokio::spawn(async move {
                    let _ = docker
                        .remove_image(
                            &img,
                            Some(
                                bollard::query_parameters::RemoveImageOptionsBuilder::new()
                                    .force(true)
                                    .build(),
                            ),
                            None,
                        )
                        .await;
                });
                AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: container_id.clone(),
                    reason: format!(
                        "snapshot: failed to inspect committed image for scrub verification: {}",
                        e
                    ),
                }
            })?;

        let committed_env: Vec<String> = committed_inspect
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .map(|env| env.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        // Use the module-level helper to find any sensitive keys that survived.
        // An empty list means the image is clean; any key triggers an abort.
        let survivors = find_surviving_sensitive_keys(&committed_env);
        if let Some(key) = survivors.first() {
            // Scrub verification failed — remove the staged image and abort.
            let docker = self.docker.clone();
            let img = committed_image_id.clone();
            tokio::spawn(async move {
                let _ = docker
                    .remove_image(
                        &img,
                        Some(
                            bollard::query_parameters::RemoveImageOptionsBuilder::new()
                                .force(true)
                                .build(),
                        ),
                        None,
                    )
                    .await;
            });
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!(
                    "snapshot: scrub verification failed — sensitive key '{}' survived in committed image config; snapshot aborted and staged image removed",
                    key
                ),
            });
        }

        // ── Export to content-addressed tarball ───────────────────────────────
        let data_dir = std::env::var("TEMPS_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join(".temps")
            });
        let snapshots_dir = data_dir.join("snapshots");
        tokio::fs::create_dir_all(&snapshots_dir)
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!(
                    "snapshot: failed to create snapshots directory {}: {}",
                    snapshots_dir.display(),
                    e
                ),
            })?;

        // Stream the export through a Sha256 hasher while writing to a temp
        // file, then rename atomically. All file I/O uses tokio::fs so we
        // never block the async runtime on a potentially multi-GB write.
        let tmp_path = snapshots_dir.join(format!(".tmp-{}", short_id));

        // Helper: best-effort cleanup on failure — remove the staged Docker
        // image (same pattern the earlier branches use) and unlink the temp
        // file if it was created. Both are best-effort: a cleanup failure
        // should not mask the original error.
        let cleanup_on_err = {
            let docker = self.docker.clone();
            let img = committed_image_id.clone();
            let tmp = tmp_path.clone();
            move || {
                tokio::spawn(async move {
                    let _ = docker
                        .remove_image(
                            &img,
                            Some(
                                bollard::query_parameters::RemoveImageOptionsBuilder::new()
                                    .force(true)
                                    .build(),
                            ),
                            None,
                        )
                        .await;
                    // Best-effort unlink of the partially-written temp file.
                    let _ = tokio::fs::remove_file(&tmp).await;
                });
            }
        };

        let mut tmp_file = match tokio::fs::File::create(&tmp_path).await {
            Ok(f) => f,
            Err(e) => {
                cleanup_on_err();
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: container_id.clone(),
                    reason: format!("snapshot: failed to create temp file: {}", e),
                });
            }
        };

        let mut hasher = Sha256::new();
        let mut size_bytes: u64 = 0;

        {
            let mut export_stream = self.docker.export_image(&committed_image_id);
            while let Some(chunk) = futures::StreamExt::next(&mut export_stream).await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        drop(tmp_file);
                        cleanup_on_err();
                        return Err(AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id: container_id.clone(),
                            reason: format!("snapshot: image export stream error: {}", e),
                        });
                    }
                };
                hasher.update(&chunk);
                size_bytes += chunk.len() as u64;
                if let Err(e) = tmp_file.write_all(&chunk).await {
                    drop(tmp_file);
                    cleanup_on_err();
                    return Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: container_id.clone(),
                        reason: format!("snapshot: failed to write to temp file: {}", e),
                    });
                }
            }
        }

        if let Err(e) = tmp_file.flush().await {
            drop(tmp_file);
            cleanup_on_err();
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!("snapshot: failed to flush temp file: {}", e),
            });
        }
        drop(tmp_file);

        let digest_hex = hex::encode(hasher.finalize());
        let final_path = snapshots_dir.join(format!("{}.tar", digest_hex));

        // Atomic rename — the file is only visible at the final path once
        // fully written, so a concurrent reader never sees a partial write.
        if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
            cleanup_on_err();
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_id.clone(),
                reason: format!("snapshot: atomic rename failed: {}", e),
            });
        }

        // ── Tag the committed image with the public-facing name ───────────────
        // Derive an image_ref from the label if provided, otherwise use digest.
        let image_label = label
            .as_deref()
            .map(|l| {
                // Sanitize: lowercase, replace non-alphanumeric with '-'
                l.chars()
                    .map(|c| {
                        if c.is_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
            })
            .unwrap_or_else(|| digest_hex[..16].to_string());
        let image_ref = format!("temps-snapshot/{}:latest", image_label);

        // Re-tag the committed image to the canonical snapshot name.
        self.docker
            .tag_image(
                &committed_image_id,
                Some(
                    bollard::query_parameters::TagImageOptionsBuilder::new()
                        .repo(format!("temps-snapshot/{}", image_label).as_str())
                        .tag("latest")
                        .build(),
                ),
            )
            .await
            .map_err(|e| {
                tracing::warn!(
                    "snapshot: failed to tag committed image as {}: {}",
                    image_ref,
                    e
                );
                // Non-fatal: the tarball is written; the image can be re-imported
                // from the tarball on restore.
                AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: container_id.clone(),
                    reason: format!("snapshot: failed to tag image {}: {}", image_ref, e),
                }
            })?;

        // Remove the staging tag now that the canonical tag is in place.
        let _ = self
            .docker
            .remove_image(
                &format!("{}:latest", commit_tag),
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .noprune(true) // keep the layers — only untag
                        .build(),
                ),
                None,
            )
            .await;

        tracing::info!(
            container_id = %container_id,
            digest = %digest_hex,
            size_bytes = %size_bytes,
            path = %final_path.display(),
            image_ref = %image_ref,
            "snapshot: completed successfully"
        );

        Ok(super::SnapshotArtifact {
            content_path: final_path,
            content_digest: digest_hex,
            size_bytes,
            backend: super::SandboxBackend::Docker,
            image_ref: Some(image_ref),
        })
    }

    // ── Snapshot: restore ─────────────────────────────────────────────────────

    /// Create a new sandbox seeded from a snapshot artifact.
    ///
    /// Ensures the snapshot image is present in the Docker daemon (loading
    /// from the tarball if the tag is absent or stale), then delegates to
    /// `create` with the image override set to `artifact.image_ref`.
    async fn create_from_snapshot(
        &self,
        artifact: &super::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        let image_ref =
            artifact
                .image_ref
                .as_deref()
                .ok_or_else(|| AgentError::SandboxExecFailed {
                    run_id: config.run_id,
                    sandbox_id: String::new(),
                    reason: "snapshot: artifact has no image_ref (non-Docker snapshot?)"
                        .to_string(),
                })?;

        // Check if the image is already present in the daemon.
        let image_present = self.docker.inspect_image(image_ref).await.is_ok();

        if !image_present {
            // Load from the tarball. `docker load` imports all tags embedded
            // in the tarball — the image will re-appear under `image_ref`.
            tracing::info!(
                image_ref = %image_ref,
                path = %artifact.content_path.display(),
                "snapshot: image not in daemon — loading from tarball"
            );

            // Hard cap: reject tarballs larger than 20 GiB before reading
            // anything into memory. This prevents an unbounded Vec allocation
            // on a 4 GB reference host. The cap is intentionally generous
            // (a typical sandbox image is 2–4 GB) while still bounding risk.
            const MAX_SNAPSHOT_BYTES: u64 = 20 * 1024 * 1024 * 1024; // 20 GiB

            let path = artifact.content_path.clone();
            let file_size = tokio::fs::metadata(&path)
                .await
                .map_err(|e| AgentError::SandboxExecFailed {
                    run_id: config.run_id,
                    sandbox_id: String::new(),
                    reason: format!("snapshot: failed to stat tarball: {}", e),
                })?
                .len();

            if file_size > MAX_SNAPSHOT_BYTES {
                return Err(AgentError::SandboxExecFailed {
                    run_id: config.run_id,
                    sandbox_id: String::new(),
                    reason: format!(
                        "snapshot: tarball too large to import: {} bytes exceeds \
                         maximum {} bytes (20 GiB); \
                         contact support if you need to restore a larger snapshot",
                        file_size, MAX_SNAPSHOT_BYTES,
                    ),
                });
            }

            // Read the tarball via spawn_blocking so the async runtime is not
            // stalled on a potentially multi-GB disk read. The bollard
            // `import_image_stream` API requires the full bytes upfront
            // (it does not accept an async reader); the cap above bounds
            // the worst-case allocation.
            let buf = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                use std::io::Read;
                let mut f = std::fs::File::open(&path)?;
                let mut buf = Vec::with_capacity(file_size as usize);
                f.read_to_end(&mut buf)?;
                Ok(buf)
            })
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: config.run_id,
                sandbox_id: String::new(),
                reason: format!("snapshot: spawn_blocking join error reading tarball: {}", e),
            })?
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: config.run_id,
                sandbox_id: String::new(),
                reason: format!(
                    "snapshot: failed to read tarball {}: {}",
                    artifact.content_path.display(),
                    e
                ),
            })?;

            let mut load_stream = self.docker.import_image_stream(
                bollard::query_parameters::ImportImageOptionsBuilder::new()
                    .quiet(true)
                    .build(),
                futures::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from(buf))
                }),
                None,
            );

            while let Some(result) = futures::StreamExt::next(&mut load_stream).await {
                match result {
                    Ok(info) => {
                        if let Some(ref detail) = info.error_detail {
                            let msg = detail
                                .message
                                .as_deref()
                                .unwrap_or("unknown docker load error");
                            return Err(AgentError::SandboxExecFailed {
                                run_id: config.run_id,
                                sandbox_id: String::new(),
                                reason: format!("snapshot: docker load error: {}", msg),
                            });
                        }
                    }
                    Err(e) => {
                        return Err(AgentError::SandboxExecFailed {
                            run_id: config.run_id,
                            sandbox_id: String::new(),
                            reason: format!("snapshot: docker load stream error: {}", e),
                        });
                    }
                }
            }

            tracing::info!(
                image_ref = %image_ref,
                "snapshot: image loaded from tarball"
            );
        }

        // Delegate to the normal create path with the snapshot image.
        let mut config = config;
        config.image = Some(image_ref.to_string());
        self.create(config).await
    }

    // ── Snapshot: delete image ────────────────────────────────────────────────

    /// Remove the Docker image tag for a deleted snapshot.
    ///
    /// Called by `SnapshotService::delete_snapshot` after the DB row is
    /// soft-deleted and the tarball has been removed. This cleans up the
    /// Docker daemon's image store so it doesn't accumulate stale snapshot
    /// images indefinitely.
    ///
    /// Uses `noprune: false` (default) so untagged intermediate layers are
    /// also reclaimed. `force: false` so an image referenced by a running
    /// container isn't deleted — that's a bug, not a normal condition.
    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        tracing::debug!(image_ref = %image_ref, "snapshot: removing Docker image");
        match self
            .docker
            .remove_image(
                image_ref,
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .force(false)
                        .noprune(false)
                        .build(),
                ),
                None,
            )
            .await
        {
            Ok(_) => {
                tracing::debug!(image_ref = %image_ref, "snapshot: Docker image removed");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                // Image already absent — idempotent.
                tracing::debug!(
                    image_ref = %image_ref,
                    "snapshot: Docker image not found during delete (already removed)"
                );
                Ok(())
            }
            Err(e) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: format!(
                    "snapshot: failed to remove Docker image '{}': {}",
                    image_ref, e
                ),
            }),
        }
    }
}

/// Apply iptables FORWARD-chain rules on the sandbox bridge to block egress to
/// RFC-1918, link-local, and loopback ranges.
///
/// # Why this exists
///
/// Without this filter, a process inside a sandbox can reach the host gateway,
/// the control-plane API (typically 172.x.x.1 or 10.x.x.1 on the bridge),
/// the PostgreSQL/TimescaleDB port, and cloud-metadata endpoints
/// (169.254.169.254).  This function installs a dedicated iptables chain
/// `TEMPS_SANDBOX_EGRESS` and hooks it into the FORWARD chain on the bridge
/// interface, dropping traffic to the five private ranges while leaving all
/// public-internet egress (npm, pip, cargo, GitHub, …) unrestricted.
///
/// # Platform behaviour
///
/// - **Linux** (production): full iptables filtering applied.
/// - **macOS / non-Linux** (developer laptops): the function logs a warning
///   and returns immediately without error.  Docker Desktop on macOS handles
///   networking inside a VM where host iptables are irrelevant.
///
/// # Permissions
///
/// The temps server process needs `CAP_NET_ADMIN` to manipulate iptables.  If
/// the iptables command exits with a permission error the function logs a
/// `WARN` with an actionable message and continues — sandbox creation is NOT
/// aborted.  This keeps the server functional on constrained environments
/// (rootless Docker, unprivileged containers) at the cost of the egress filter
/// being inactive.
///
/// # Idempotency
///
/// The function flushes and recreates the `TEMPS_SANDBOX_EGRESS` chain on
/// every call and uses `-D` before `-I` for the FORWARD hook, so running on
/// every server start is safe and never accumulates duplicate rules.
async fn apply_sandbox_egress_filter(docker: &Docker, network_id: &str) {
    // This is a Linux-only operation.  On macOS (developer machines running
    // Docker Desktop), iptables doesn't exist on the host — the sandbox runs
    // inside a VM and host-level filtering is irrelevant.
    if !cfg!(target_os = "linux") {
        tracing::warn!(
            network_id = network_id,
            "Sandbox egress filter (iptables) is Linux-only; skipping on this platform. \
             Ensure the control plane runs on Linux in production."
        );
        return;
    }

    // Resolve the bridge interface name.  Docker stores it under the
    // "com.docker.network.bridge.name" option.  If that key is absent (e.g.
    // for overlay drivers) we fall back to the `br-<first 12 chars of id>`
    // convention that the bridge driver uses by default.
    let bridge_name = match docker
        .inspect_network(
            network_id,
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await
    {
        Ok(info) => info
            .options
            .as_ref()
            .and_then(|o| o.get("com.docker.network.bridge.name"))
            .cloned()
            .unwrap_or_else(|| {
                let prefix = if network_id.len() >= 12 {
                    &network_id[..12]
                } else {
                    network_id
                };
                format!("br-{}", prefix)
            }),
        Err(e) => {
            tracing::warn!(
                network_id = network_id,
                error = %e,
                "Could not inspect sandbox network to resolve bridge interface name; \
                 egress filter will not be applied. Sandboxes may be able to reach \
                 internal addresses."
            );
            return;
        }
    };

    tracing::info!(
        network_id = network_id,
        bridge_interface = %bridge_name,
        "Applying iptables egress filter to sandbox bridge"
    );

    // Build the ordered list of iptables commands.  Order matters:
    //  1. Create the chain (ignore "already exists").
    //  2. Flush it (idempotent on every server start).
    //  3. Append DROP rules for all private ranges.
    //  4. Append RETURN so non-matched packets fall back to the default policy.
    //  5. Delete then re-insert the FORWARD hook (idempotent; avoids duplicates).
    let cmds: &[&[&str]] = &[
        // Create dedicated chain — error "Chain already exists" is benign.
        &["iptables", "-N", "TEMPS_SANDBOX_EGRESS"],
        // Flush ensures idempotency across server restarts.
        &["iptables", "-F", "TEMPS_SANDBOX_EGRESS"],
        // RFC-1918 private ranges
        &[
            "iptables",
            "-A",
            "TEMPS_SANDBOX_EGRESS",
            "-d",
            "10.0.0.0/8",
            "-j",
            "DROP",
        ],
        &[
            "iptables",
            "-A",
            "TEMPS_SANDBOX_EGRESS",
            "-d",
            "172.16.0.0/12",
            "-j",
            "DROP",
        ],
        &[
            "iptables",
            "-A",
            "TEMPS_SANDBOX_EGRESS",
            "-d",
            "192.168.0.0/16",
            "-j",
            "DROP",
        ],
        // Link-local (cloud metadata, APIPA)
        &[
            "iptables",
            "-A",
            "TEMPS_SANDBOX_EGRESS",
            "-d",
            "169.254.0.0/16",
            "-j",
            "DROP",
        ],
        // Loopback
        &[
            "iptables",
            "-A",
            "TEMPS_SANDBOX_EGRESS",
            "-d",
            "127.0.0.0/8",
            "-j",
            "DROP",
        ],
        // Fall through to default policy for public addresses.
        &["iptables", "-A", "TEMPS_SANDBOX_EGRESS", "-j", "RETURN"],
    ];

    for cmd in cmds {
        let status = match tokio::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .status()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                // ENOENT means iptables isn't installed — treat as non-fatal.
                tracing::warn!(
                    command = cmd.join(" "),
                    error = %e,
                    "iptables command could not be executed; sandbox egress filter \
                     may be incomplete. Ensure iptables (or iptables-legacy on \
                     iptables-nft systems) is installed and the server has CAP_NET_ADMIN."
                );
                return;
            }
        };
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            // Exit code 1 from `-N` means "chain already exists" — that's fine.
            // Exit code 4 means "another process is already using iptables" —
            // transient and harmless for this best-effort setup.
            let is_chain_exists = code == 1 && cmd.contains(&"-N");
            let is_lock_contention = code == 4;
            if !is_chain_exists && !is_lock_contention {
                tracing::warn!(
                    command = cmd.join(" "),
                    exit_code = code,
                    "iptables command failed; sandbox egress filter may be incomplete. \
                     If this is a permission error, ensure the temps server process \
                     has CAP_NET_ADMIN (e.g. add it to the systemd service's \
                     AmbientCapabilities). On iptables-nft systems, install \
                     iptables-legacy or ensure /etc/alternatives/iptables points \
                     to the nft-compat shim."
                );
            }
        }
    }

    // Hook the chain into FORWARD on this specific bridge interface.
    // Delete first (ignore failure) then insert — guarantees exactly one entry.
    let delete_args = [
        "-D",
        "FORWARD",
        "-i",
        &bridge_name,
        "-j",
        "TEMPS_SANDBOX_EGRESS",
    ];
    // Ignore failure — rule may not exist yet on first run.
    let _ = tokio::process::Command::new("iptables")
        .args(delete_args)
        .status()
        .await;

    let insert_args = [
        "-I",
        "FORWARD",
        "-i",
        &bridge_name,
        "-j",
        "TEMPS_SANDBOX_EGRESS",
    ];
    match tokio::process::Command::new("iptables")
        .args(insert_args)
        .status()
        .await
    {
        Ok(s) if s.success() => {
            tracing::info!(
                network_id = network_id,
                bridge_interface = %bridge_name,
                "Sandbox egress filter applied: RFC-1918 + 169.254/16 + 127/8 blocked on bridge"
            );
        }
        Ok(s) => {
            tracing::warn!(
                network_id = network_id,
                bridge_interface = %bridge_name,
                exit_code = s.code().unwrap_or(-1),
                "Failed to insert FORWARD hook for sandbox egress filter; \
                 sandboxes may be able to reach internal addresses"
            );
        }
        Err(e) => {
            tracing::warn!(
                network_id = network_id,
                bridge_interface = %bridge_name,
                error = %e,
                "Failed to execute iptables to insert FORWARD hook; \
                 sandboxes may be able to reach internal addresses"
            );
        }
    }
}

/// Build the list of (chain, cidr) pairs that `apply_sandbox_egress_filter`
/// would DROP.  Extracted for unit-testing without requiring a live Docker
/// daemon or iptables binary.
#[cfg(test)]
fn sandbox_egress_drop_ranges() -> Vec<(&'static str, &'static str)> {
    vec![
        ("TEMPS_SANDBOX_EGRESS", "10.0.0.0/8"),
        ("TEMPS_SANDBOX_EGRESS", "172.16.0.0/12"),
        ("TEMPS_SANDBOX_EGRESS", "192.168.0.0/16"),
        ("TEMPS_SANDBOX_EGRESS", "169.254.0.0/16"),
        ("TEMPS_SANDBOX_EGRESS", "127.0.0.0/8"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::time::Duration;
    use tokio::sync::Mutex;

    /// Serializes Docker integration tests that mutate the shared sandbox
    /// image (`ghcr.io/gotempsh/temps-sandbox-node:<version>`). Without
    /// this, `test_pull_fallback_on_missing_hub_image` (which deletes the
    /// image, forces a rebuild, then inspects it) can race with
    /// `test_docker_sandbox_e2e_lifecycle` (which creates a container
    /// from that same image) and produce
    ///   `unable to find image "sha256:..."`
    /// when the delete lands mid-create.
    fn docker_image_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_container_name_format() {
        assert_eq!(
            DockerSandboxProvider::container_name(42),
            "temps-sandbox-42"
        );
    }

    /// Regression: the home volume name must be derivable from the
    /// container name alone.
    ///
    /// The leak this replaced: `create` built the volume name from
    /// `config.run_id` while `destroy` rebuilt it by stripping the prefix
    /// off `handle.sandbox_name`. For agent runs both produce the same
    /// string, so the bug was invisible there — but standalone sandboxes
    /// override the container suffix with their `public_id` label, so
    /// destroy asked Docker to remove a volume that had never existed and
    /// the real one stayed on disk for good.
    #[test]
    fn home_volume_name_round_trips_through_container_name() {
        // Agent-run style: numeric suffix.
        assert_eq!(
            DockerSandboxProvider::home_volume_name(&DockerSandboxProvider::container_name(42)),
            "temps-sandbox-home-v2-42"
        );
        // Standalone style: opaque public_id label suffix. This is the
        // case that used to leak.
        assert_eq!(
            DockerSandboxProvider::home_volume_name("temps-sandbox-a1b2c3d4e5f60718"),
            "temps-sandbox-home-v2-a1b2c3d4e5f60718"
        );
    }

    /// A standalone sandbox (`container_name_override`) and an agent run
    /// that happen to share a numeric id must not share a home volume.
    /// Keying on `run_id` meant sandbox row 5 and agent run 5 both mounted
    /// `temps-sandbox-home-5` — one sandbox reading another's shell
    /// history, `~/.claude` credentials, and project files.
    #[test]
    fn home_volume_name_does_not_collide_across_naming_schemes() {
        let agent_run =
            DockerSandboxProvider::home_volume_name(&DockerSandboxProvider::container_name(5));
        let standalone = DockerSandboxProvider::home_volume_name("temps-sandbox-abc123");
        assert_ne!(agent_run, standalone);
    }

    fn create_config_for(run_id: i32, override_label: Option<&str>) -> SandboxCreateConfig {
        SandboxCreateConfig {
            owner_user_id: None,
            run_id,
            container_name_override: override_label.map(|s| s.to_string()),
            host_work_dir: std::path::PathBuf::from("/tmp/does-not-matter"),
            workspace_volume: None,
            image: None,
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            network_mode: None,
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
        }
    }

    /// The Docker-less guard for the leak. `create` takes both names from
    /// `sandbox_names`, and `destroy` re-derives the volume from the
    /// container name — so asserting the pair agrees here catches a
    /// reintroduction of the original bug on any machine, including CI
    /// runners with no Docker daemon (where the e2e below skips).
    #[test]
    fn sandbox_names_agree_for_standalone_and_agent_run() {
        // Standalone: opaque public_id label. The old code derived the
        // volume from run_id instead, producing temps-sandbox-home-7 here
        // while destroy looked for temps-sandbox-home-a1b2c3d4e5f60718.
        let (container, volume) =
            DockerSandboxProvider::sandbox_names(&create_config_for(7, Some("a1b2c3d4e5f60718")));
        assert_eq!(container, "temps-sandbox-a1b2c3d4e5f60718");
        assert_eq!(volume, "temps-sandbox-home-v2-a1b2c3d4e5f60718");
        assert_eq!(volume, DockerSandboxProvider::home_volume_name(&container));
        assert_ne!(volume, "temps-sandbox-home-7");
        assert_ne!(volume, "temps-sandbox-home-v2-7");

        // Agent run: numeric naming, no override.
        let (container, volume) =
            DockerSandboxProvider::sandbox_names(&create_config_for(42, None));
        assert_eq!(container, "temps-sandbox-42");
        assert_eq!(volume, "temps-sandbox-home-v2-42");
        assert_eq!(volume, DockerSandboxProvider::home_volume_name(&container));
    }

    /// The bind the container actually gets must mount the volume `destroy`
    /// will remove.
    ///
    /// This is the Docker-less guard for the original bug's *call site*.
    /// The naming tests above only prove the helpers agree with each other;
    /// re-inlining `format!("temps-sandbox-home-{}", config.run_id)` into
    /// `create`'s binds would leave every one of them green and re-ship the
    /// leak on any CI runner without a daemon. Asserting on the bind string
    /// closes that, because the bind is what `create` actually hands Docker.
    #[test]
    fn create_binds_mount_the_volume_destroy_will_remove() {
        let cfg = create_config_for(7, Some("a1b2c3d4e5f60718"));
        let (container, volume) = DockerSandboxProvider::sandbox_names(&cfg);
        let binds = DockerSandboxProvider::container_binds("/host/work", &volume);

        // The home bind must name the volume destroy re-derives from the
        // container name — not anything keyed on run_id.
        let home_bind = format!(
            "{}:{}",
            DockerSandboxProvider::home_volume_name(&container),
            SANDBOX_HOME
        );
        assert!(
            binds.contains(&home_bind),
            "binds {:?} must mount {} — otherwise create and destroy \
             disagree about which volume belongs to this sandbox",
            binds,
            home_bind
        );
        assert!(
            !binds.iter().any(|b| b.starts_with("temps-sandbox-home-7:")),
            "binds {:?} must not key the home volume on run_id",
            binds
        );
        // And the work dir is still mounted where the sandbox expects it.
        assert!(binds.contains(&format!("/host/work:{}", CONTAINER_WORK_DIR)));
    }

    /// No name this build generates may land in the pre-fix namespace.
    ///
    /// Legacy volumes are `temps-sandbox-home-<digits>`. On an upgraded host
    /// they are stranded but still present, and Docker attaches an existing
    /// volume by name — so if we could still generate one of those names, an
    /// agent run would mount a previous standalone sandbox's `/home/temps`,
    /// i.e. another user's credentials and shell history.
    #[test]
    fn generated_names_never_reenter_the_pre_fix_namespace() {
        let legacy_shaped = |name: &str| {
            name.strip_prefix(HOME_VOLUME_PREFIX)
                .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false)
        };
        // Sanity: the predicate really does describe the legacy shape.
        assert!(legacy_shaped("temps-sandbox-home-5"));

        for container in [
            DockerSandboxProvider::container_name(5),
            DockerSandboxProvider::container_name(1),
            DockerSandboxProvider::container_name(i32::MAX),
            "temps-sandbox-0123456789012345".to_string(), // all-digit hex label
        ] {
            let volume = DockerSandboxProvider::home_volume_name(&container);
            assert!(
                !legacy_shaped(&volume),
                "{} produced {}, which collides with a pre-fix volume and \
                 could mount another tenant's home",
                container,
                volume
            );
        }
    }

    /// Every home volume must carry the prefix an operator filters on when
    /// reclaiming stranded volumes by hand — a naming change on the create
    /// side would otherwise leave volumes nothing can find.
    #[test]
    fn home_volume_names_carry_the_documented_prefix() {
        for container in ["temps-sandbox-1", "temps-sandbox-deadbeef", "no-prefix"] {
            assert!(
                DockerSandboxProvider::home_volume_name(container).starts_with(HOME_VOLUME_PREFIX),
                "{} produced a volume outside the documented prefix",
                container
            );
        }
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
        // Serialize against other tests that delete/rebuild the shared
        // sandbox image; see `docker_image_lock`.
        let _guard = docker_image_lock().lock().await;
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
            owner_user_id: None,
            run_id,
            container_name_override: None,
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(512),
            pids_limit: None,
            disk_size_mb: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::from([("TEST_VAR".to_string(), "test_value".to_string())]),
            idle_timeout: Duration::from_secs(120),
            backend: None,
        };

        // Some dev environments (macOS Docker Desktop's virtiofs / userns-remap
        // on Linux) refuse to honor chown on bind-mounted host directories,
        // which trips the post-create write-probe with "permission denied".
        // That's a real signal in production but expected here, so degrade
        // to skip rather than fail.
        let handle = match provider.create(create_config).await {
            Ok(h) => h,
            Err(AgentError::SandboxCreationFailed { reason, .. })
                if reason.contains("write-probe") =>
            {
                println!(
                    "Skipping e2e test: bind-mount filesystem doesn't honor chown ({})",
                    reason
                );
                return;
            }
            Err(e) => panic!("sandbox creation failed: {:?}", e),
        };
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
        // Serialize against other tests that build/use the shared sandbox
        // image; this test deletes it mid-suite which would otherwise race
        // with `test_docker_sandbox_e2e_lifecycle` and produce a
        // `unable to find image "sha256:..."` container-create failure.
        let _guard = docker_image_lock().lock().await;
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

        // `ensure_image_for_runtime` tags both the pulled and the
        // locally-built image with the fully-qualified GHCR name including
        // the channel suffix — that's the string we must remove beforehand
        // and inspect afterwards. An earlier version of this test used the
        // short `temps-sandbox-node:{version}` name; that name has never
        // been produced by the provider, so the post-build inspect would
        // always fail.
        let local_image = image_name_for_runtime("node");

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

        // Image should now exist locally under its GHCR-qualified name.
        assert!(docker.inspect_image(&local_image).await.is_ok());
    }

    #[tokio::test]
    async fn test_kill_processes_term_and_kill() {
        // Serialize against other tests that build/delete the shared
        // sandbox image; see `docker_image_lock`.
        let _guard = docker_image_lock().lock().await;
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
            owner_user_id: None,
            run_id,
            container_name_override: None,
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            disk_size_mb: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
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
            owner_user_id: None,
            run_id,
            container_name_override: Some(label.to_string()),
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: None,
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(256),
            pids_limit: None,
            disk_size_mb: None,
            network_mode: Some("none".to_string()),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
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

    /// Docker-level proof that destroying a *standalone* sandbox actually
    /// frees its home volume.
    ///
    /// This is the disk-fill regression: standalone sandboxes name their
    /// container after the `public_id` label, and destroy used to
    /// reconstruct the volume name from that label while create had named
    /// it after the numeric `run_id`. Every create/destroy cycle therefore
    /// stranded one volume — with `~/.npm`, `~/.cache`, and the sandbox
    /// home in it — and a host churning sandboxes filled its disk with
    /// storage no API could reach. Asserting on the volume (not the
    /// container) is the point: the container was always removed correctly,
    /// which is exactly why this went unnoticed.
    #[tokio::test]
    async fn destroy_frees_the_home_volume_of_a_standalone_sandbox() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping volume purge test");
                return;
            }
        };
        let docker = Arc::new(docker);
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping volume purge test");
            return;
        }

        let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());
        if provider.ensure_image().await.is_err() {
            println!("Cannot build sandbox image, skipping volume purge test");
            return;
        }

        // Standalone naming: opaque label suffix, numeric run_id. The two
        // deliberately disagree — that divergence is what the bug rode on.
        let label = "purgetestc0ffee01";
        let run_id = 99993;
        let work_dir = std::env::temp_dir().join(format!("sandbox-purge-test-{}", run_id));
        let _ = std::fs::create_dir_all(&work_dir);

        // A `temps serve` on this host reaps sandbox containers whose run
        // isn't in its DB, which would kill this test mid-flight. Same guard
        // the sibling e2e tests use — and this repo's dev workflow runs
        // several slots, so it fires in practice.
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
                "temps serve is managing {} sandbox(es) — skipping volume purge test",
                existing.len()
            );
            return;
        }

        let volume_name = format!("{}{}{}", HOME_VOLUME_PREFIX, HOME_VOLUME_SCHEME, label);
        let legacy_volume_name = format!("{}{}", HOME_VOLUME_PREFIX, run_id);
        // Clear both names first: a leftover from an older build would make
        // the label assertion below fail for a reason that isn't this code.
        for name in [&volume_name, &legacy_volume_name] {
            let _ = docker
                .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
                .await;
        }

        let handle = provider
            .create(SandboxCreateConfig {
                owner_user_id: None,
                run_id,
                container_name_override: Some(label.to_string()),
                host_work_dir: work_dir.clone(),
                workspace_volume: None,
                image: None,
                cpu_limit: Some(1.0),
                memory_limit_mb: Some(256),
                pids_limit: None,
                disk_size_mb: None,
                network_mode: Some("none".to_string()),
                env_vars: HashMap::new(),
                idle_timeout: Duration::from_secs(60),
                backend: None,
            })
            .await
            .expect("create sandbox");

        // Gather every observation BEFORE tearing down, then assert after —
        // an assertion that fires between create and destroy would leak the
        // container, its volume, and the work dir onto the host.
        let volume = docker.inspect_volume(&volume_name).await;
        let legacy_volume = docker.inspect_volume(&legacy_volume_name).await;
        let container = docker
            .inspect_container(
                &handle.sandbox_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await;

        provider.destroy(&handle, true).await.expect("destroy");

        let after_destroy = docker.inspect_volume(&volume_name).await;
        let _ = std::fs::remove_dir_all(&work_dir);

        // 1. The volume exists while the sandbox does, and is labelled so an
        //    operator can find it if a later destroy ever fails to.
        let volume = volume.expect("home volume should exist while the sandbox does");
        assert!(
            volume.labels.contains_key(HOME_VOLUME_LABEL),
            "home volume {} must carry {} — it is the only handle an \
             operator has for reclaiming stranded volumes; labels were {:?}",
            volume_name,
            HOME_VOLUME_LABEL,
            volume.labels
        );

        // 2. The container actually mounts that volume at /home/temps.
        //    Without this, the assertion above passes on its own merits —
        //    `create` pre-creates the volume, so its existence no longer
        //    proves the bind exists, and dropping the bind entirely would
        //    go unnoticed.
        let mounts = container
            .expect("container should exist")
            .mounts
            .unwrap_or_default();
        assert!(
            mounts.iter().any(|m| {
                m.name.as_deref() == Some(volume_name.as_str())
                    && m.destination.as_deref() == Some(SANDBOX_HOME)
            }),
            "container must mount {} at {} — mounts were {:?}",
            volume_name,
            SANDBOX_HOME,
            mounts
        );

        // 3. The run_id-keyed name is the one the old code would have made.
        //    Assert it was never created, so this test cannot pass by
        //    accident on a host where both happen to exist.
        assert!(
            legacy_volume.is_err(),
            "home volume must be keyed on the container label, not run_id"
        );

        // 4. And destroy actually frees it — the leak this PR exists for.
        assert!(
            after_destroy.is_err(),
            "destroy must remove the sandbox home volume {} — leaving it \
             behind is the leak that fills the host disk after enough \
             create/destroy cycles",
            volume_name
        );
    }

    // ---- egress filter tests (no Docker / iptables required) ----------------

    /// Verify the DROP-range list covers all five required CIDR blocks.
    /// This test does NOT invoke iptables — it validates the command set that
    /// `apply_sandbox_egress_filter` would dispatch.
    #[test]
    fn test_sandbox_egress_filter_covers_all_private_ranges() {
        let ranges: Vec<&str> = sandbox_egress_drop_ranges()
            .into_iter()
            .map(|(_, cidr)| cidr)
            .collect();

        // RFC-1918
        assert!(
            ranges.contains(&"10.0.0.0/8"),
            "must DROP 10.0.0.0/8 (RFC-1918 class A)"
        );
        assert!(
            ranges.contains(&"172.16.0.0/12"),
            "must DROP 172.16.0.0/12 (RFC-1918 class B)"
        );
        assert!(
            ranges.contains(&"192.168.0.0/16"),
            "must DROP 192.168.0.0/16 (RFC-1918 class C)"
        );
        // Link-local / cloud metadata
        assert!(
            ranges.contains(&"169.254.0.0/16"),
            "must DROP 169.254.0.0/16 (link-local / cloud metadata)"
        );
        // Loopback
        assert!(
            ranges.contains(&"127.0.0.0/8"),
            "must DROP 127.0.0.0/8 (loopback)"
        );

        // All five ranges — no extras, no gaps.
        assert_eq!(
            ranges.len(),
            5,
            "expected exactly 5 DROP ranges, got {}",
            ranges.len()
        );
    }

    /// Every entry in the range list must reference the dedicated chain.
    #[test]
    fn test_sandbox_egress_filter_uses_dedicated_chain() {
        for (chain, cidr) in sandbox_egress_drop_ranges() {
            assert_eq!(
                chain, "TEMPS_SANDBOX_EGRESS",
                "range {} must be appended to the TEMPS_SANDBOX_EGRESS chain, not '{}'",
                cidr, chain
            );
        }
    }

    // ── Credential-scrubbing tests (ADR-037 §4) ───────────────────────────────
    //
    // These tests verify the scrubbing helpers in isolation — no Docker daemon
    // is required. The security invariant is: after applying the change
    // instructions produced by `build_env_scrub_changes`, every sensitive key
    // in the committed image has an empty value, and
    // `find_surviving_sensitive_keys` returns an empty list.

    #[test]
    fn is_sensitive_env_key_detects_anthropic_api_key() {
        assert!(is_sensitive_env_key("ANTHROPIC_API_KEY"));
        assert!(is_sensitive_env_key("anthropic_api_key")); // case-insensitive
        assert!(is_sensitive_env_key("MY_ANTHROPIC_API_KEY")); // substring match
    }

    #[test]
    fn is_sensitive_env_key_detects_github_token() {
        assert!(is_sensitive_env_key("GITHUB_TOKEN"));
        assert!(is_sensitive_env_key("GITHUB_TOKEN_READONLY")); // variant
    }

    #[test]
    fn is_sensitive_env_key_detects_temps_prefix() {
        // Every TEMPS_ var is treated as potentially sensitive — the daemon
        // env file, internal tokens, and credential paths all use this namespace.
        assert!(is_sensitive_env_key("TEMPS_DATA_DIR"));
        assert!(is_sensitive_env_key("TEMPS_CREDENTIAL_TOKEN"));
    }

    #[test]
    fn is_sensitive_env_key_passes_safe_keys() {
        assert!(!is_sensitive_env_key("PATH"));
        assert!(!is_sensitive_env_key("HOME"));
        assert!(!is_sensitive_env_key("USER"));
        assert!(!is_sensitive_env_key("LANG"));
        assert!(!is_sensitive_env_key("DEBIAN_FRONTEND"));
        assert!(!is_sensitive_env_key("NODE_VERSION"));
    }

    #[test]
    fn build_env_scrub_changes_returns_change_instructions_for_sensitive_keys() {
        // build_env_scrub_changes must produce `ENV KEY=` Dockerfile-style
        // change instructions for each sensitive key — these are passed to
        // `docker commit --change` which zeroes the values in the committed
        // image. Non-sensitive keys must not appear in the output.
        let env = vec![
            "PATH=/usr/bin:/bin".to_string(),
            "ANTHROPIC_API_KEY=sk-ant-secret".to_string(),
            "HOME=/home/temps".to_string(),
            "GITHUB_TOKEN=ghp_supersecret".to_string(),
            "NODE_VERSION=20".to_string(),
        ];

        let changes = build_env_scrub_changes(&env);

        // Must produce exactly one change instruction per sensitive key.
        assert!(
            changes.contains(&"ENV ANTHROPIC_API_KEY=".to_string()),
            "expected ENV ANTHROPIC_API_KEY= change instruction; got: {:?}",
            changes
        );
        assert!(
            changes.contains(&"ENV GITHUB_TOKEN=".to_string()),
            "expected ENV GITHUB_TOKEN= change instruction; got: {:?}",
            changes
        );

        // Safe keys must NOT appear in the changes list.
        assert!(
            !changes.iter().any(|c| c.contains("PATH")),
            "PATH must not appear in change instructions: {:?}",
            changes
        );
        assert!(
            !changes.iter().any(|c| c.contains("HOME")),
            "HOME must not appear in change instructions: {:?}",
            changes
        );
        assert!(
            !changes.iter().any(|c| c.contains("NODE_VERSION")),
            "NODE_VERSION must not appear in change instructions: {:?}",
            changes
        );

        // Exactly 2 sensitive keys → 2 change instructions.
        assert_eq!(
            changes.len(),
            2,
            "expected 2 change instructions; got {:?}",
            changes
        );
    }

    #[test]
    fn build_env_scrub_changes_empty_env_returns_no_changes() {
        let changes = build_env_scrub_changes(&[]);
        assert!(changes.is_empty());
    }

    #[test]
    fn build_env_scrub_changes_no_sensitive_keys_returns_no_changes() {
        let env = vec!["PATH=/usr/bin".to_string(), "HOME=/home/temps".to_string()];
        let changes = build_env_scrub_changes(&env);
        assert!(
            changes.is_empty(),
            "no sensitive keys → no change instructions; got: {:?}",
            changes
        );
    }

    #[test]
    fn find_surviving_sensitive_keys_returns_empty_for_clean_env() {
        // A committed image with no sensitive keys at all — clean.
        let env = vec![
            "PATH=/usr/bin:/bin".to_string(),
            "HOME=/home/temps".to_string(),
        ];

        let survivors = find_surviving_sensitive_keys(&env);
        assert!(
            survivors.is_empty(),
            "expected no surviving keys, got: {:?}",
            survivors
        );
    }

    #[test]
    fn find_surviving_sensitive_keys_zeroed_key_is_not_a_survivor() {
        // A zeroed entry (KEY=) has been successfully scrubbed by the Docker
        // `--change "ENV KEY="` mechanism. An empty value is the expected
        // post-commit state and must NOT be flagged as a survivor.
        let env = vec![
            "PATH=/usr/bin".to_string(),
            "GITHUB_TOKEN=".to_string(), // zeroed — successfully scrubbed
            "ANTHROPIC_API_KEY=".to_string(), // zeroed — successfully scrubbed
        ];

        let survivors = find_surviving_sensitive_keys(&env);
        assert!(
            survivors.is_empty(),
            "zeroed sensitive keys (KEY=) must not be flagged as survivors; got: {:?}",
            survivors
        );
    }

    #[test]
    fn find_surviving_sensitive_keys_catches_non_empty_secret_value() {
        // A committed image where scrubbing failed — the key has a real value.
        let env = vec![
            "PATH=/usr/bin".to_string(),
            "ANTHROPIC_API_KEY=sk-ant-secret".to_string(), // not zeroed
        ];

        let survivors = find_surviving_sensitive_keys(&env);
        assert_eq!(
            survivors,
            vec!["ANTHROPIC_API_KEY"],
            "a sensitive key with a non-empty value must be flagged as a survivor"
        );
    }

    #[test]
    fn find_surviving_sensitive_keys_mixed_zeroed_and_live() {
        // Mix of zeroed (scrubbed) and still-live (failure case) sensitive keys.
        let env = vec![
            "PATH=/usr/bin".to_string(),
            "GITHUB_TOKEN=".to_string(),                   // zeroed — OK
            "ANTHROPIC_API_KEY=sk-ant-secret".to_string(), // live — failure
        ];

        let survivors = find_surviving_sensitive_keys(&env);
        assert!(
            survivors.contains(&"ANTHROPIC_API_KEY".to_string()),
            "live sensitive key must be flagged; got: {:?}",
            survivors
        );
        assert!(
            !survivors.contains(&"GITHUB_TOKEN".to_string()),
            "zeroed sensitive key must not be flagged; got: {:?}",
            survivors
        );
    }

    #[test]
    fn scrub_then_verify_leaves_no_survivors() {
        // End-to-end property: after applying the change instructions produced
        // by `build_env_scrub_changes`, simulating Docker zeroing the values,
        // `find_surviving_sensitive_keys` must return an empty list.
        let env = vec![
            "PATH=/usr/bin:/bin".to_string(),
            "ANTHROPIC_API_KEY=sk-ant-secret".to_string(),
            "OPENAI_API_KEY=sk-openai-value".to_string(),
            "GITHUB_TOKEN=ghp_supersecret".to_string(),
            "MY_DB_PASSWORD=hunter2".to_string(),
            "HOME=/home/temps".to_string(),
            "TEMPS_CREDENTIAL_TOKEN=tok_internal".to_string(),
        ];

        // Get the change instructions that would be passed to docker commit.
        let changes = build_env_scrub_changes(&env);

        // Simulate what Docker does when applying `ENV KEY=` change instructions:
        // it overwrites the matching env entry's value to an empty string.
        let mut simulated_env = env.clone();
        for change in &changes {
            // Each change is "ENV KEY=" — strip the "ENV " prefix and the
            // trailing "=" to get the key name.
            if let Some(rest) = change.strip_prefix("ENV ") {
                let key = rest.trim_end_matches('=');
                for entry in simulated_env.iter_mut() {
                    if entry.starts_with(&format!("{}=", key)) {
                        *entry = format!("{}=", key);
                    }
                }
            }
        }

        let survivors = find_surviving_sensitive_keys(&simulated_env);
        assert!(
            survivors.is_empty(),
            "after applying change instructions, find_surviving_sensitive_keys \
             must return empty; found survivors: {:?}",
            survivors
        );
    }

    // ── C2 regression: CLAUDE_CODE_OAUTH_TOKEN coverage ──────────────────────

    /// Regression test for C2: CLAUDE_CODE_OAUTH_TOKEN must be classified
    /// sensitive. It is injected by executor.rs (~line 470) and trigger.rs
    /// (~line 768) for Claude subscription auth and would have been committed
    /// into every subscription user's snapshot without this fix.
    #[test]
    fn is_sensitive_env_key_detects_claude_code_oauth_token() {
        assert!(
            is_sensitive_env_key("CLAUDE_CODE_OAUTH_TOKEN"),
            "CLAUDE_CODE_OAUTH_TOKEN must be classified sensitive (regression for C2)"
        );
    }

    #[test]
    fn is_sensitive_env_key_detects_oauth_token_variants() {
        // The pattern "OAUTH_TOKEN" catches any OAuth token variant.
        assert!(is_sensitive_env_key("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(is_sensitive_env_key("MY_OAUTH_TOKEN"));
        assert!(is_sensitive_env_key("oauth_token")); // case-insensitive
    }

    /// Regression: all env var names that are known to be injected into sandboxes
    /// at create time must be classified as sensitive. Keep this list in sync with:
    ///   - crates/temps-agents/src/services/executor.rs  (CLAUDE_CODE_OAUTH_TOKEN)
    ///   - crates/temps-agents/src/handlers/trigger.rs   (CLAUDE_CODE_OAUTH_TOKEN)
    ///
    /// If a new credential injection site is added without updating
    /// SENSITIVE_ENV_PATTERNS, this test fails and blocks the merge.
    #[test]
    fn all_known_injected_credential_env_vars_are_classified_sensitive() {
        let injected_credentials = [
            // Injected by executor.rs for Claude subscription auth
            "CLAUDE_CODE_OAUTH_TOKEN",
            // Injected by executor.rs / sandbox_injector for API keys
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "OPENCODE_API_KEY",
            // Injected for git provider tokens
            "GITHUB_TOKEN",
            "GITLAB_TOKEN",
            "BITBUCKET_TOKEN",
            // Injected via TEMPS_ prefix (credential daemon, internal tokens)
            "TEMPS_GIT_CREDENTIAL_TOKEN",
        ];

        for key in &injected_credentials {
            assert!(
                is_sensitive_env_key(key),
                "injected credential env var '{}' is NOT classified as sensitive — \
                 add a matching pattern to SENSITIVE_ENV_PATTERNS",
                key
            );
        }
    }

    #[test]
    fn sensitive_patterns_list_covers_required_keys() {
        // Ensure the pattern list includes every key the ADR explicitly names.
        // This is a static test — adding a required pattern to the ADR but
        // forgetting to add it to the list will fail here.
        let required = [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "GITLAB_TOKEN",
            "BITBUCKET_TOKEN",
            "API_KEY",
            "SECRET",
            "PASSWORD",
            "CREDENTIAL",
            "AWS_SECRET",
            "AWS_ACCESS_KEY",
            "AZURE_CLIENT_SECRET",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "OAUTH_TOKEN", // catches CLAUDE_CODE_OAUTH_TOKEN and any future OAuth tokens
            "TEMPS_",
        ];
        for required_key in required {
            assert!(
                SENSITIVE_ENV_PATTERNS.contains(&required_key),
                "SENSITIVE_ENV_PATTERNS is missing required key '{}'",
                required_key
            );
        }
    }

    // ── Real-Docker regression: credential-scrub mechanism ────────────────────
    //
    // This test exercises `take_snapshot` end-to-end against a real running
    // Docker daemon. It would have caught the bug that shipped undetected
    // through multiple review rounds: the old implementation passed sensitive
    // env vars to `docker commit` via the `ContainerConfig` body's `env`
    // field, which the Docker Engine silently ignores — the committed image
    // retained the original secret values unchanged. The fix switches to
    // `CommitContainerOptionsBuilder::changes()` ("ENV KEY=" Dockerfile
    // instructions), which is the only API mechanism that actually zeroes
    // values in the committed image's `Config.Env`.
    //
    // The absence of this test (only unit tests on standalone functions, no
    // real Docker integration) was the root reason the bug shipped.

    /// Regression test: `take_snapshot` must zero all sensitive env-var values
    /// in the committed Docker image via `--change 'ENV KEY='`.
    ///
    /// This test creates a real container with credential-shaped env vars
    /// (`ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `SAFE_VAR`), calls
    /// the real `DockerSandboxProvider::take_snapshot`, then inspects the
    /// committed image's `Config.Env` to assert that sensitive keys are
    /// present with an **empty** value (`KEY=`) and the safe key survived
    /// unchanged.
    ///
    /// The exact assertion (`ANTHROPIC_API_KEY=`) is what the old broken code
    /// failed: it produced `ANTHROPIC_API_KEY=sk-ant-...` in the committed
    /// image, meaning the secret was baked into every snapshot. A test that
    /// only checked "the key isn't present at all" would have missed the bug
    /// because Docker's commit never removes keys — it can only overwrite them.
    ///
    /// Skips gracefully (prints a message and returns) when Docker is not
    /// available in the test environment, per CLAUDE.md convention.
    #[tokio::test]
    async fn test_take_snapshot_scrubs_credentials_against_real_docker() {
        // Connect to Docker — skip gracefully if unavailable (CI without Docker,
        // macOS without Docker Desktop running, etc.).
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

        // alpine:3.20 — small image that is always available. No provisioned
        // `temps` user or AI CLI needed: this test only exercises the env-var
        // scrubbing path, not the full workspace boot sequence.
        let base_image = "alpine:3.20";
        if docker.inspect_image(base_image).await.is_err() {
            let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
                .from_image(base_image)
                .build();
            let mut stream = docker.create_image(Some(options), None, None);
            while let Some(result) = stream.next().await {
                if let Err(e) = result {
                    println!("Cannot pull {}, skipping test: {}", base_image, e);
                    return;
                }
            }
        }

        // Use a fixed container name so stale containers from a previous
        // interrupted run are cleaned up automatically.
        let container_name = "temps-snapshot-scrub-regression-test";

        // Best-effort removal of any leftover from a previous test run.
        let _ = docker
            .remove_container(
                container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        // Create a container with env vars that mimic real injected credentials.
        // The values are synthetic but structurally real: the assertion below
        // checks that these specific non-empty values are replaced with "".
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(base_image.to_string()),
            cmd: Some(vec!["sleep".to_string(), "60".to_string()]),
            env: Some(vec![
                // Sensitive: injected by executor.rs for Anthropic API auth
                "ANTHROPIC_API_KEY=sk-ant-real-secret-must-be-scrubbed".to_string(),
                // Sensitive: injected by executor.rs/trigger.rs for Claude subscription auth
                "CLAUDE_CODE_OAUTH_TOKEN=tok-oauth-real-secret-must-be-scrubbed".to_string(),
                // Non-sensitive: must survive in the snapshot unchanged
                "SAFE_VAR=keep-me".to_string(),
            ]),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .expect("failed to create regression test container");

        let container_id = container.id.clone();

        // Start the container so it has a fully-initialized state that Docker
        // can commit (an un-started container has no `Config.Env` snapshot).
        docker
            .start_container(
                &container_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("failed to start regression test container");

        // Stop it — take_snapshot documents that the container must be stopped
        // before commit so the filesystem is in a consistent state.
        docker
            .stop_container(
                &container_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(5),
                    signal: None,
                }),
            )
            .await
            .expect("failed to stop regression test container");

        // Construct a SandboxHandle pointing at the container.
        // The same construction the throwaway harness used (which caught the
        // original bug on first manual run and motivated this permanent test).
        let handle = SandboxHandle {
            sandbox_id: container_id.clone(),
            sandbox_name: container_name.to_string(),
            work_dir: "/home/temps/workspace".into(),
            backend: crate::sandbox::SandboxBackend::Docker,
            image: base_image.to_string(),
        };

        let provider = DockerSandboxProvider::new(docker.clone(), DockerSandboxConfig::default());

        // Call the real take_snapshot — the path that was broken by the
        // ContainerConfig.env body (silently ignored by the Docker Engine)
        // and fixed by switching to CommitContainerOptionsBuilder::changes().
        let artifact = provider
            .take_snapshot(&handle, Some("scrub-regression-test".to_string()))
            .await
            .expect("take_snapshot failed");

        // Capture what we need to clean up before asserting, so we know what
        // to remove even if an assertion panics.
        let snapshot_image_ref = artifact.image_ref.clone();
        let snapshot_path = artifact.content_path.clone();

        // ── Core assertion: inspect the committed image's Config.Env ─────────
        // This is the exact check that would have caught the bug. The old
        // code using ContainerConfig.env left ANTHROPIC_API_KEY with its
        // original "sk-ant-..." value. The fix produces "ANTHROPIC_API_KEY="
        // (zeroed, empty value) via `ENV KEY=` Dockerfile change instructions.

        let image_ref_str = snapshot_image_ref
            .as_deref()
            .expect("take_snapshot must populate image_ref for Docker backend");

        let inspect = docker
            .inspect_image(image_ref_str)
            .await
            .expect("failed to inspect committed snapshot image");

        let committed_env: Vec<String> = inspect
            .config
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .map(|env| env.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        // ANTHROPIC_API_KEY must be present with an EMPTY value — this is
        // the "zeroed" post-commit state that the `--change 'ENV KEY='`
        // mechanism produces. The original value must not appear anywhere.
        assert!(
            committed_env.contains(&"ANTHROPIC_API_KEY=".to_string()),
            "ANTHROPIC_API_KEY must be zeroed ('KEY=') in the committed image — \
             the old ContainerConfig.env path left the original secret intact. \
             committed_env = {:?}",
            committed_env
        );

        // CLAUDE_CODE_OAUTH_TOKEN must be zeroed — this key is what subscription
        // users have injected, making the scrub coverage especially critical.
        assert!(
            committed_env.contains(&"CLAUDE_CODE_OAUTH_TOKEN=".to_string()),
            "CLAUDE_CODE_OAUTH_TOKEN must be zeroed ('KEY=') in the committed image. \
             committed_env = {:?}",
            committed_env
        );

        // SAFE_VAR must survive unchanged — the scrubber must not strip safe keys.
        assert!(
            committed_env.contains(&"SAFE_VAR=keep-me".to_string()),
            "SAFE_VAR=keep-me must survive in the committed image unchanged. \
             committed_env = {:?}",
            committed_env
        );

        // Belt-and-suspenders: the literal original secret values must not
        // appear anywhere in the committed env (as a substring of any entry).
        let leaked: Vec<_> = committed_env
            .iter()
            .filter(|e| {
                e.contains("sk-ant-real-secret-must-be-scrubbed")
                    || e.contains("tok-oauth-real-secret-must-be-scrubbed")
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "original secret values must not appear in the committed image; leaked: {:?}",
            leaked
        );

        // ── Cleanup ───────────────────────────────────────────────────────────
        // Best-effort: don't panic on cleanup failure, but do attempt it so
        // repeated test runs don't accumulate test images and containers.
        let _ = docker
            .remove_container(
                &container_id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if let Some(ref img) = snapshot_image_ref {
            let _ = docker
                .remove_image(
                    img,
                    Some(
                        bollard::query_parameters::RemoveImageOptionsBuilder::new()
                            .force(true)
                            .build(),
                    ),
                    None,
                )
                .await;
        }

        // Remove the snapshot tarball written by take_snapshot to avoid
        // accumulating test artifacts in ~/.temps/snapshots/.
        let _ = tokio::fs::remove_file(&snapshot_path).await;
    }
}
