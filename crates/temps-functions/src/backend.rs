// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What waking and sleeping a function mean to the activator: start or
//! stop its `ScaleToZero` workload through the workload plane.
//!
//! The activator calls this at most once at a time per function (it
//! serialises wakes), and only for a function that is not awake, so the
//! one database read per wake is off the request path of an awake
//! function.
//!
//! A function whose row is gone (deleted outside [`crate::FunctionService`],
//! e.g. by a cascade nobody retired it before) is retired here rather than
//! failing every sweep forever: its workloads and ingress endpoints are
//! deleted and its key and hostname are unregistered, so it stops serving.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_entities::functions;
use temps_workload_activator::{ActivationBackend, ActivationKey, Activator, Woken};
use temps_workload_plane::{SharedPlane, WorkloadPlane};
use tracing::warn;

use crate::service::{delete_function_endpoints, retire_function_workloads};

pub struct FunctionActivationBackend {
    db: Arc<DatabaseConnection>,
    plane: Arc<SharedPlane>,
    /// Weak: the activator owns this backend.
    activator: Weak<Activator>,
}

/// What the activator's key names.
enum Target {
    Function {
        workload_id: i64,
        name: String,
    },
    /// The function's row no longer exists.
    Gone,
}

impl FunctionActivationBackend {
    pub fn new(
        db: Arc<DatabaseConnection>,
        plane: Arc<SharedPlane>,
        activator: &Arc<Activator>,
    ) -> Self {
        Self {
            db,
            plane,
            activator: Arc::downgrade(activator),
        }
    }

    fn plane(&self, key: ActivationKey) -> Result<Arc<WorkloadPlane>, String> {
        self.plane.get().ok_or_else(|| {
            format!(
                "function {}: the workload plane is not running in this process",
                key.id
            )
        })
    }

    async fn target(&self, key: ActivationKey) -> Result<Target, String> {
        let Some(function) = functions::Entity::find_by_id(key.id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("function {}: reading its row failed: {e}", key.id))?
        else {
            return Ok(Target::Gone);
        };
        let workload_id = function.workload_id.ok_or_else(|| {
            format!(
                "function '{}' (id {}) has no workload yet",
                function.name, function.id
            )
        })?;
        Ok(Target::Function {
            workload_id,
            name: function.name,
        })
    }

    /// Retire what is left of a function whose row is gone: its workloads
    /// and endpoints, then its activator key and hostname. Idempotent.
    async fn retire_gone(&self, plane: &WorkloadPlane, key: ActivationKey) -> Result<(), String> {
        let label = format!("function {} (row deleted)", key.id);
        let retired = retire_function_workloads(self.db.as_ref(), plane, key.id, &label, None)
            .await
            .map_err(|e| format!("function {}: retiring its leftover workloads: {e}", key.id))?;
        delete_function_endpoints(self.db.as_ref(), key.id)
            .await
            .map_err(|e| {
                format!(
                    "function {}: deleting its leftover ingress endpoints: {e}",
                    key.id
                )
            })?;
        if let Some(activator) = self.activator.upgrade() {
            activator.unregister(key);
        }
        warn!(
            function_id = key.id,
            retired_workloads = retired,
            "a function's row was gone while it was still registered; retired its workloads and hostname"
        );
        Ok(())
    }
}

#[async_trait]
impl ActivationBackend for FunctionActivationBackend {
    async fn wake(&self, key: ActivationKey) -> Result<Woken, String> {
        let plane = self.plane(key)?;
        let (workload_id, name) = match self.target(key).await? {
            Target::Function { workload_id, name } => (workload_id, name),
            Target::Gone => {
                self.retire_gone(&plane, key).await?;
                return Err(format!("function {} no longer exists", key.id));
            }
        };
        let awake = plane
            .wake_workload(workload_id)
            .await
            .map_err(|e| format!("function '{name}' (id {}): {e}", key.id))?;
        Ok(Woken {
            address: Some(awake.address),
        })
    }

    async fn sleep(&self, key: ActivationKey) -> Result<bool, String> {
        let plane = self.plane(key)?;
        let (workload_id, name) = match self.target(key).await? {
            Target::Function { workload_id, name } => (workload_id, name),
            Target::Gone => {
                // Stopped for good: the sweep marks it asleep, and it is no
                // longer registered, so nothing wakes it again.
                self.retire_gone(&plane, key).await?;
                return Ok(true);
            }
        };
        plane
            .sleep_workload(workload_id)
            .await
            .map_err(|e| format!("function '{name}' (id {}): {e}", key.id))
    }
}
