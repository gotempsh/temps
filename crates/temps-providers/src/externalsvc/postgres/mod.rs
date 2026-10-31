// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::ServiceResourceLimits;
use anyhow::Result;
use bollard::Docker;
use sea_orm::{prelude::*, *};
use std::sync::Arc;
use tokio::sync::RwLock;
use urlencoding;

mod backup;
mod config;
mod lifecycle;
mod restore;
mod service_impl;
mod walg;

pub use config::*;
pub use lifecycle::*;
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
}

impl PostgresService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
        }
    }
}

/// Importing data from an external PostgreSQL server (see `crate::data_import`).
mod data_import;
