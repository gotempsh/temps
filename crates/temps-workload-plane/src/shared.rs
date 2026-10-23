// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Handing the one plane of a process to the products that use it.
//!
//! The plane is built and started by the deployments plugin while plugins
//! initialise; a product registered beside it (functions) cannot receive it
//! by constructor. [`SharedPlane`] is registered empty when services
//! register and filled once the plane starts, so a product resolves it when
//! it needs it and reports "the workload plane is not running" with why,
//! instead of holding an `Option` it cannot explain.
//!
//! [`LocalDrivers`] goes the other way: runtime drivers a binary adds next
//! to the node driver on the control plane (the WASM driver, behind the
//! `wasm` cargo feature of `temps-cli`). The plugin that builds the plane
//! attaches them with [`crate::WorkloadPlane::with_local_driver`].

use std::sync::{Arc, OnceLock};

use temps_workload::RuntimeDriver;

use crate::plane::WorkloadPlane;

/// The process's started plane, once there is one.
#[derive(Debug, Default)]
pub struct SharedPlane {
    plane: OnceLock<Arc<WorkloadPlane>>,
}

impl SharedPlane {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the started plane. A second call is ignored and returns
    /// `false`: one process runs one plane.
    pub fn set(&self, plane: Arc<WorkloadPlane>) -> bool {
        self.plane.set(plane).is_ok()
    }

    pub fn get(&self) -> Option<Arc<WorkloadPlane>> {
        self.plane.get().cloned()
    }

    /// Names of the runtime drivers the plane runs on the control plane,
    /// node driver first. Empty while no plane runs.
    pub fn local_driver_names(&self) -> Vec<String> {
        self.plane
            .get()
            .map(|plane| plane.local_driver_names_list())
            .unwrap_or_default()
    }
}

/// Extra control-plane runtime drivers for the plane (the WASM driver).
#[derive(Default)]
pub struct LocalDrivers {
    drivers: Vec<Arc<dyn RuntimeDriver>>,
}

impl std::fmt::Debug for LocalDrivers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(
                self.drivers
                    .iter()
                    .map(|driver| driver.capabilities().name.clone()),
            )
            .finish()
    }
}

impl LocalDrivers {
    pub fn new(drivers: Vec<Arc<dyn RuntimeDriver>>) -> Self {
        Self { drivers }
    }

    pub fn drivers(&self) -> &[Arc<dyn RuntimeDriver>] {
        &self.drivers
    }

    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_handle_reports_no_plane_and_no_drivers() {
        let shared = SharedPlane::new();
        assert!(shared.get().is_none());
        assert!(shared.local_driver_names().is_empty());
        assert!(LocalDrivers::default().is_empty());
    }
}
