// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What waking and sleeping a function mean to the activator: start or
//! stop its `ScaleToZero` workload through the workload plane.
//!
//! The activator calls this at most once at a time per function (it
//! serialises wakes), and only for a function that is not awake, so the
//! one database read per wake is off the request path of an awake
//! function.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_entities::functions;
use temps_workload_activator::{ActivationBackend, ActivationKey, Woken};
use temps_workload_plane::SharedPlane;

pub struct FunctionActivationBackend {
    db: Arc<DatabaseConnection>,
    plane: Arc<SharedPlane>,
}

impl FunctionActivationBackend {
    pub fn new(db: Arc<DatabaseConnection>, plane: Arc<SharedPlane>) -> Self {
        Self { db, plane }
    }

    async fn workload_of(&self, key: ActivationKey) -> Result<(i64, String), String> {
        let function = functions::Entity::find_by_id(key.id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("function {}: reading its row failed: {e}", key.id))?
            .ok_or_else(|| format!("function {} no longer exists", key.id))?;
        let workload_id = function.workload_id.ok_or_else(|| {
            format!(
                "function '{}' (id {}) has no workload yet",
                function.name, function.id
            )
        })?;
        Ok((workload_id, function.name))
    }
}

#[async_trait]
impl ActivationBackend for FunctionActivationBackend {
    async fn wake(&self, key: ActivationKey) -> Result<Woken, String> {
        let plane = self.plane.get().ok_or_else(|| {
            format!(
                "function {}: the workload plane is not running in this process",
                key.id
            )
        })?;
        let (workload_id, name) = self.workload_of(key).await?;
        let awake = plane
            .wake_workload(workload_id)
            .await
            .map_err(|e| format!("function '{name}' (id {}): {e}", key.id))?;
        Ok(Woken {
            address: Some(awake.address),
        })
    }

    async fn sleep(&self, key: ActivationKey) -> Result<bool, String> {
        let plane = self.plane.get().ok_or_else(|| {
            format!(
                "function {}: the workload plane is not running in this process",
                key.id
            )
        })?;
        let (workload_id, name) = self.workload_of(key).await?;
        plane
            .sleep_workload(workload_id)
            .await
            .map_err(|e| format!("function '{name}' (id {}): {e}", key.id))
    }
}
