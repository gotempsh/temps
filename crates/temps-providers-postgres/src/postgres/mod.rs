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
