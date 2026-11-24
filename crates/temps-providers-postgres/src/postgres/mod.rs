// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::ServiceResourceLimits;
use crate::externalsvc::error::{EngineError, Result};
use bollard::Docker;
use sea_orm::{prelude::*, *};
use std::sync::Arc;
use tokio::sync::RwLock;
use urlencoding;

mod backup;
mod config;
mod lifecycle;
mod node_recreate;
mod restore;
mod restore_swap;
pub mod sandbox;
mod service_impl;
mod walg;

pub use config::*;
pub use lifecycle::*;
pub use node_recreate::{NodeAfterCreate, NodeRecreate};
use restore::*;
pub use walg::*;

/// Splits a discovered WAL-G stop-sentinel key into its repository and exact base backup name.
pub fn walg_snapshot(location: &str) -> Option<(&str, &str)> {
    let (root, file) = location.rsplit_once("/basebackups_005/")?;
    let name = file.strip_suffix("_backup_stop_sentinel.json")?;
    (root.ends_with("/walg")
        && name.starts_with("base_")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
    .then_some((root, name))
}

/// The environment variable a WAL-G fetch script reads the selected backup's
/// user data from. Deliberately not `WALG_FETCH_TARGET_USER_DATA`: WAL-G reads
/// that name as its own setting, and then refuses a fetch that also names a
/// backup ("Specify target backup name OR target userdata, not both").
pub const FETCH_TARGET_USER_DATA_ENV: &str = "TEMPS_FETCH_TARGET_USER_DATA";

/// How a `wal-g backup-fetch` picks its backup: exactly one of the exact base
/// backup, the backup tagged with the given user data, or the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalgFetchSelector<'a> {
    /// An exact base backup name, already validated by [`walg_snapshot`].
    Snapshot(&'a str),
    /// The backup whose user data equals this value.
    UserData(&'a str),
    Latest,
}

impl<'a> WalgFetchSelector<'a> {
    /// An exact snapshot wins over user data: both identify the same backup,
    /// and WAL-G accepts only one of them.
    pub fn new(snapshot: Option<&'a str>, target_user_data: Option<&'a str>) -> Self {
        match (snapshot, target_user_data) {
            (Some(name), _) => Self::Snapshot(name),
            (None, Some(user_data)) => Self::UserData(user_data),
            (None, None) => Self::Latest,
        }
    }

    /// The `backup-fetch` argument after the destination directory. User
    /// data is read from [`FETCH_TARGET_USER_DATA_ENV`], never interpolated.
    pub fn fetch_argument(&self) -> String {
        match self {
            Self::Snapshot(name) => (*name).to_string(),
            Self::UserData(_) => format!("--target-user-data \"${FETCH_TARGET_USER_DATA_ENV}\""),
            Self::Latest => "LATEST".to_string(),
        }
    }

    /// The `NAME=value` environment entry the fetch argument reads, if any,
    /// for an exec environment (no shell quoting).
    pub fn env_entry(&self) -> Option<String> {
        self.user_data()
            .map(|user_data| format!("{FETCH_TARGET_USER_DATA_ENV}={user_data}"))
    }

    /// The user data the fetch argument reads from the environment, if any.
    pub fn user_data(&self) -> Option<&'a str> {
        match self {
            Self::UserData(user_data) => Some(user_data),
            Self::Snapshot(_) | Self::Latest => None,
        }
    }
}

pub struct PostgresService {
    name: String,
    config: Arc<RwLock<Option<PostgresConfig>>>,
    /// Resource limits captured at init time and reused by `start()` when
    /// recreating a container that was removed externally. Defaults to
    /// unlimited; populated from the `resources` block in the
    /// `ServiceConfig::parameters` JSON.
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
    /// Set when this service runs on a worker node and this instance is its
    /// agent's: a recreate rebuilds the running container from its own
    /// inspected specification instead of the provider's control-plane one.
    node: Option<NodeRecreate>,
}

impl PostgresService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
            node: None,
        }
    }

    /// This service, as the agent of the worker node running it sees it:
    /// see [`NodeRecreate`].
    pub fn on_node(mut self, node: NodeRecreate) -> Self {
        self.node = Some(node);
        self
    }
}

/// Importing data from an external PostgreSQL server (see `crate::data_import`).
mod data_import;
