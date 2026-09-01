// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! RAII sidecar container guard.
//!
//! Every backup engine that spins up a sidecar wraps it in
//! [`SidecarGuard`]. The guard force-removes the container on every
//! exit path — `Ok`, `Err`, early return, panic, and even task abort
//! (via the `tokio::spawn` in `Drop`). This makes the
//! "container leak when a panic happens at the wrong await point"
//! class of bug structurally impossible.
//!
//! ## Lifecycle
//!
//! ```ignore
//! let guard = SidecarGuard::start(
//!     &docker,
//!     SidecarSpec {
//!         image: "postgres:18",
//!         name: "temps-cp-backup-<uuid>",
//!         labels: …,
//!         entrypoint: vec!["/bin/sleep", "1800"],  // self-destruct TTL
//!         binds: vec!["/host/path:/backup:rw"],
//!         network: Some("host"),
//!         env: vec!["PGPASSWORD=…"],
//!     },
//! ).await?;
//!
//! // …use the guard.exec(...) helpers…
//!
//! // Guard drops at end of scope -> spawns force-remove.
//! ```
//!
//! ## Why a `tokio::spawn` in `Drop`?
//!
//! `Drop` is synchronous; Docker calls are async. We could block on a
//! current-thread runtime, but that risks deadlock if the parent task
//! is the only thing keeping the runtime alive. Instead, we capture
//! the docker handle and container name into a detached task that
//! force-removes asynchronously. Worst case: the parent task ends and
//! the spawn races process shutdown — the entrypoint's `sleep 1800`
//! is the backstop.

use bollard::query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptions};
use bollard::Docker;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Spec for a sidecar container. Engines pass this to [`SidecarGuard::start`].
///
/// All sidecars share the same label scheme so a future janitor can
/// reap them by selector — but the executor's RAII drop should reap
/// everything, so the labels are belt-and-braces.
pub struct SidecarSpec<'a> {
    /// `image:tag` form. Caller is responsible for `ensure_image_pulled`
    /// before calling `start` — the guard does not pull on its own.
    pub image: &'a str,

    /// Human-readable container name. Must be unique. Engines typically
    /// use `format!("temps-{engine}-backup-{uuid}", …)`.
    pub name: &'a str,

    /// Engine key (e.g. `"control_plane"`, `"postgres_pgdump"`) — used
    /// only to stamp the `sh.temps.engine` label.
    pub engine: &'a str,

    /// The `backup_id` this sidecar belongs to. Stamped as
    /// `sh.temps.backup_id` so a future operator can `docker ps` and
    /// see which backup is using each container.
    pub backup_id: i32,

    /// Entrypoint + cmd. Engines that need a long-lived sidecar (so
    /// `docker exec` can run repeatedly inside it) pass something like
    /// `vec!["/bin/sleep", "1800"]`. The 30-minute TTL is a safety
    /// net — the guard's `Drop` reaps the container much sooner in
    /// the happy path.
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,

    /// Environment variables in `KEY=VALUE` form.
    pub env: Vec<String>,

    /// Bind mounts in `"/host:/container[:opts]"` form.
    pub binds: Vec<String>,

    /// `Some("host")` for host networking, `Some("temps-app-network")`
    /// to join the user-defined bridge, `None` for default bridge.
    /// Engines should match whatever network the source container is on.
    pub network_mode: Option<String>,

    /// Run as this user inside the container. `Some("root")` is
    /// typical for sidecars that need to bind-mount a host-owned dir.
    pub user: Option<String>,
}

/// RAII handle to a running sidecar. Force-removes on drop.
pub struct SidecarGuard {
    docker: Docker,
    name: String,
    /// Whether `Drop` has work to do. Set to `false` if the engine
    /// explicitly disowns the guard (rare; only used by tests).
    armed: bool,
}

impl SidecarGuard {
    /// Create + start the container, returning a guard. The container
    /// has run-with-restart-policy `no` (the default) and `auto_remove`
    /// set to `true` so Docker also reaps it when the entrypoint exits
    /// — defence in depth alongside `Drop`.
    ///
    /// Any error path before `Ok(guard)` cleans up its own partial
    /// state: a failed `start_container` after a successful
    /// `create_container` triggers an immediate force-remove.
    pub async fn start(
        docker: &Docker,
        spec: SidecarSpec<'_>,
    ) -> Result<Self, bollard::errors::Error> {
        let mut labels: HashMap<String, String> = HashMap::new();
        labels.insert("sh.temps.kind".to_string(), "backup-sidecar".to_string());
        labels.insert("sh.temps.engine".to_string(), spec.engine.to_string());
        labels.insert("sh.temps.backup_id".to_string(), spec.backup_id.to_string());
        labels.insert(
            "sh.temps.born".to_string(),
            chrono::Utc::now().timestamp().to_string(),
        );

        let host_config = bollard::models::HostConfig {
            auto_remove: Some(true),
            // -500 makes us less likely to be the OOM victim than other
            // workloads but still kills us before the OS reaper.
            oom_score_adj: Some(-500),
            network_mode: spec.network_mode.clone(),
            binds: if spec.binds.is_empty() {
                None
            } else {
                Some(spec.binds.clone())
            },
            ..Default::default()
        };

        let config = bollard::models::ContainerCreateBody {
            image: Some(spec.image.to_string()),
            entrypoint: Some(spec.entrypoint.clone()),
            cmd: Some(spec.cmd.clone()),
            env: if spec.env.is_empty() {
                None
            } else {
                Some(spec.env.clone())
            },
            user: spec.user.clone(),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(spec.name).build()),
                config,
            )
            .await?;

        let guard = SidecarGuard {
            docker: docker.clone(),
            name: spec.name.to_string(),
            armed: true,
        };

        if let Err(e) = docker
            .start_container(
                spec.name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
            // Force-remove what we just created before returning the
            // error. The guard's Drop would also do this, but doing it
            // synchronously here lets the caller retry with the same
            // container name without waiting for the detached spawn.
            let _ = docker
                .remove_container(
                    spec.name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            // The guard hasn't been returned yet, so its Drop will be
            // called on the local. Disarm so we don't double-remove.
            let mut g = guard;
            g.armed = false;
            return Err(e);
        }

        debug!(container = %spec.name, engine = %spec.engine, "SidecarGuard: started");
        Ok(guard)
    }

    /// Container name (same as the `spec.name` passed at create time).
    /// Engines that need to call `docker.exec_create` against the
    /// sidecar use this.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrow the docker handle the guard was constructed with. Engines
    /// use this to issue `docker.exec_create` / `docker.start_exec`
    /// against `self.name()`.
    pub fn docker(&self) -> &Docker {
        &self.docker
    }

    /// Disarm the guard — `Drop` will not force-remove. Only for tests
    /// or when the engine has *already* removed the container itself
    /// and wants to avoid a redundant remove that would log a warning.
    #[allow(dead_code)]
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SidecarGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let docker = self.docker.clone();
        let name = self.name.clone();
        // Detached spawn so we don't block `Drop`. The container's own
        // `auto_remove: true` + `sleep 1800` entrypoint is the second
        // line of defence if this task is racing process shutdown.
        tokio::spawn(async move {
            match docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(()) => debug!(container = %name, "SidecarGuard::Drop: removed"),
                Err(e) => {
                    warn!(container = %name, error = %e, "SidecarGuard::Drop: remove failed (non-fatal)")
                }
            }
        });
    }
}
