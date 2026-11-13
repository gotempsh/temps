// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The steps of continuous archiving and WAL-G restore that depend on where
//! PostgreSQL runs: the container recreates that turn `archive_mode` on, and
//! that start a restored data directory (or put the original back).
//!
//! On the control plane the provider builds the replacement from its own
//! specification. A worker's container was created by its agent (ports on
//! the node's address, the node's DNS, the agent's labels, its restart
//! policy), so there the replacement is the running container's own
//! inspected specification with only `archive_mode` changed.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bollard::models::ContainerInspectResponse;
use temps_providers_core::workload::{
    runtime, DatabaseEngine, DatabaseIdentity, DatabaseProcess, DatabaseWorkloadPlan,
    NodeAttachment,
};

use super::*;

/// Runs once between creating a replacement container and starting it, with
/// its id (the agent joins it to the overlay there).
pub type NodeAfterCreate =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// How a worker node's agent recreates the PostgreSQL container it runs.
#[derive(Clone)]
pub struct NodeRecreate {
    pub service_id: i32,
    pub node_id: i32,
    pub attachment: NodeAttachment,
    pub after_create: NodeAfterCreate,
}

impl std::fmt::Debug for NodeRecreate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeRecreate")
            .field("service_id", &self.service_id)
            .field("node_id", &self.node_id)
            .field("attachment", &self.attachment)
            .finish_non_exhaustive()
    }
}

/// The replacement of a worker's PostgreSQL container: its own admitted
/// specification, restart policy included, with `archive_mode` set as asked.
pub(super) struct NodeContainerPlan {
    plan: DatabaseWorkloadPlan,
    existing: ContainerInspectResponse,
    secrets: BTreeMap<String, String>,
}

/// What a replacement does with the running container's `archive_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeArchiveMode {
    /// Turn archiving on (a backup or a re-point).
    On,
    /// Start with archiving off (a restored cluster, see `restore_from_walg`).
    Off,
    /// The running container's own setting (putting the original back).
    Keep,
}

/// `command` with `archive_mode` set to `on`: see [`command_with_archive_mode`].
#[cfg(test)]
pub(super) fn command_with_archive_mode_on(command: Vec<String>) -> Vec<String> {
    command_with_archive_mode(command, "on")
}

/// `command` with `archive_mode` set to `value`: an existing setting in any
/// of PostgreSQL's spellings (`-c archive_mode=off`, `-carchive_mode=off`,
/// `--archive_mode=off`, `--archive-mode=off`) is changed in place, and one
/// is added when there is none. Every other argument is kept as it is.
pub(super) fn command_with_archive_mode(command: Vec<String>, value: &str) -> Vec<String> {
    fn is_archive_mode(key: &str) -> bool {
        key.replace('-', "_").eq_ignore_ascii_case("archive_mode")
    }
    let mut found = false;
    let mut command: Vec<String> = command
        .into_iter()
        .map(|part| {
            let (prefix, setting) = if let Some(rest) = part.strip_prefix("--") {
                ("--", rest)
            } else if let Some(rest) = part.strip_prefix("-c") {
                ("-c", rest)
            } else {
                ("", part.as_str())
            };
            match setting.split_once('=') {
                Some((key, _)) if is_archive_mode(key) => {
                    found = true;
                    format!("{prefix}{key}={value}")
                }
                _ => part,
            }
        })
        .collect();
    if !found {
        command.extend(["-c".to_string(), format!("archive_mode={value}")]);
    }
    command
}

impl PostgresService {
    /// The recreate a worker's agent would run to turn archiving on, built
    /// from the running container. Read-only.
    pub(super) async fn node_archive_plan(
        &self,
        node: &NodeRecreate,
        container_name: &str,
    ) -> Result<NodeContainerPlan> {
        self.node_container_plan(node, container_name, NodeArchiveMode::On)
            .await
    }

    /// The running container's own specification as a plan to create it
    /// again, `archive_mode` set by `archive`. Read-only.
    pub(super) async fn node_container_plan(
        &self,
        node: &NodeRecreate,
        container_name: &str,
        archive: NodeArchiveMode,
    ) -> Result<NodeContainerPlan> {
        let snapshot = runtime::snapshot(&self.docker, container_name)
            .await
            .map_err(|e| EngineError::failed(&self.name, e.to_string()))?;
        let Some(existing) = snapshot.inspect.clone().filter(|_| snapshot.found) else {
            return Err(EngineError::failed(
                &self.name,
                format!(
                    "the container '{container_name}' of PostgreSQL service {} does not exist \
                     on node {}",
                    node.service_id, node.node_id
                ),
            ));
        };
        let mut process = DatabaseProcess::from_inspect(
            DatabaseEngine::Postgres,
            &existing,
            &snapshot.image_env,
            snapshot.image_cmd.as_deref(),
        )
        .map_err(|reason| {
            EngineError::failed(
                &self.name,
                format!(
                    "the container '{container_name}' of PostgreSQL service {} on node {} \
                     cannot be described as a workload to recreate it: {reason}",
                    node.service_id, node.node_id
                ),
            )
        })?;
        let value = match archive {
            NodeArchiveMode::On => Some("on"),
            NodeArchiveMode::Off => Some("off"),
            NodeArchiveMode::Keep => None,
        };
        if let Some(value) = value {
            let command = process
                .command
                .take()
                .or_else(|| snapshot.image_cmd.clone())
                .filter(|command| !command.is_empty())
                .ok_or_else(|| {
                    EngineError::failed(
                        &self.name,
                        format!(
                            "the container '{container_name}' of PostgreSQL service {} on node {} \
                             has no command to set archive_mode={value} in",
                            node.service_id, node.node_id
                        ),
                    )
                })?;
            process.command = Some(command_with_archive_mode(command, value));
        }
        let mut plan = DatabaseWorkloadPlan::admit(
            &DatabaseIdentity {
                service_id: node.service_id,
                service_name: self.name.clone(),
                engine: DatabaseEngine::Postgres,
                node_id: Some(node.node_id),
                container_name: container_name.to_string(),
            },
            &process,
        )
        .map_err(|e| EngineError::failed(&self.name, e.to_string()))?;
        // The agent created it `unless-stopped`; the spec's default would
        // turn that into `always`.
        plan.keep_restart_policy(&existing);
        // This process has none of the control plane's healthcheck overrides;
        // the container was created with them.
        plan.keep_healthcheck(&existing);
        Ok(NodeContainerPlan {
            plan,
            secrets: process.secret_values(),
            existing,
        })
    }

    /// Refuse, before anything is written, a recreate whose replacement
    /// would not mount the data the running container uses.
    pub(super) async fn node_archive_preflight(
        &self,
        node: &NodeRecreate,
        container_name: &str,
    ) -> Result<()> {
        let planned = self.node_archive_plan(node, container_name).await?;
        let config = planned.existing.config.as_ref();
        let env = config.and_then(|c| c.env.clone()).unwrap_or_default();
        let cmd = config.and_then(|c| c.cmd.clone()).unwrap_or_default();
        crate::externalsvc::recreate_guard::preflight(
            &self.docker,
            crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
            &self.name,
            container_name,
            &runtime::replacement_storage(&planned.plan, &env, &cmd),
            "enabling WAL archiving",
        )
        .await?;
        Ok(())
    }

    /// Recreate the worker's container with archiving on, through the
    /// recreate guard (see [`runtime::recreate_from_plan`]).
    pub(super) async fn node_archive_recreate(
        &self,
        node: &NodeRecreate,
        container_name: &str,
    ) -> Result<()> {
        let planned = self.node_archive_plan(node, container_name).await?;
        let after_create = Arc::clone(&node.after_create);
        runtime::recreate_from_plan(
            &self.docker,
            &planned.plan,
            &self.name,
            &planned.existing,
            &planned.secrets,
            &node.attachment,
            move |id| after_create(id),
        )
        .await
        .map_err(|e| EngineError::failed(&self.name, e.to_string()))
    }

    /// Create and start the container `planned` describes, in place of
    /// whatever ran under its name. Nothing checks the data it finds: the
    /// restore that calls this verifies the server itself.
    pub(super) async fn node_create_from_plan(
        &self,
        node: &NodeRecreate,
        planned: &NodeContainerPlan,
    ) -> Result<()> {
        let after_create = Arc::clone(&node.after_create);
        runtime::create_and_start(
            &self.docker,
            &planned.plan,
            &planned.secrets,
            &node.attachment,
            move |id| after_create(id),
        )
        .await
        .map(|_| ())
        .map_err(|e| EngineError::failed(&self.name, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{command_with_archive_mode, command_with_archive_mode_on};

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    /// A worker's container is created with archiving off; only that value
    /// changes.
    #[test]
    fn a_worker_command_keeps_every_setting_but_archive_mode() {
        let command = args(&[
            "postgres",
            "-c",
            "max_connections=100",
            "-c",
            "wal_level=replica",
            "-c",
            "archive_mode=off",
            "-c",
            "archive_timeout=60",
        ]);
        assert_eq!(
            command_with_archive_mode_on(command),
            args(&[
                "postgres",
                "-c",
                "max_connections=100",
                "-c",
                "wal_level=replica",
                "-c",
                "archive_mode=on",
                "-c",
                "archive_timeout=60",
            ])
        );
    }

    #[test]
    fn every_spelling_of_archive_mode_is_turned_on() {
        assert_eq!(
            command_with_archive_mode_on(args(&[
                "postgres",
                "-carchive_mode=off",
                "--archive-mode=off",
                "--ARCHIVE_MODE=off",
            ])),
            args(&[
                "postgres",
                "-carchive_mode=on",
                "--archive-mode=on",
                "--ARCHIVE_MODE=on",
            ])
        );
    }

    /// A restored cluster starts with archiving off, whatever the original
    /// ran with.
    #[test]
    fn a_restored_command_turns_archiving_off() {
        assert_eq!(
            command_with_archive_mode(
                args(&[
                    "postgres",
                    "-c",
                    "archive_mode=on",
                    "-c",
                    "archive_timeout=60"
                ]),
                "off"
            ),
            args(&[
                "postgres",
                "-c",
                "archive_mode=off",
                "-c",
                "archive_timeout=60"
            ])
        );
        assert_eq!(
            command_with_archive_mode(args(&["postgres"]), "off"),
            args(&["postgres", "-c", "archive_mode=off"])
        );
    }

    #[test]
    fn a_command_without_archive_mode_gets_it() {
        assert_eq!(
            command_with_archive_mode_on(args(&["postgres", "-c", "max_connections=5"])),
            args(&[
                "postgres",
                "-c",
                "max_connections=5",
                "-c",
                "archive_mode=on"
            ])
        );
    }

    #[test]
    fn other_settings_named_like_archive_mode_are_left_alone() {
        let command = args(&[
            "postgres",
            "-c",
            "archive_timeout=60",
            "-c",
            "archive_library=",
        ]);
        let mut expected = command.clone();
        expected.extend(args(&["-c", "archive_mode=on"]));
        assert_eq!(command_with_archive_mode_on(command), expected);
    }
}
