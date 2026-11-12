// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The healthcheck overrides the engines read when they create a container.
//!
//! Like `crate::workload::registry`, and for the same reason: the engines
//! are constructed in many places (the manager's factory, the upgrade
//! service, restores, imports, the KV and Blob plugins) from a name and a
//! Docker handle, so what a new container gets is looked up here rather than
//! threaded through every constructor. Engines on the control plane look a
//! service up by its container name (unique on one daemon); the workload
//! spec, which also describes worker databases, looks it up by service id.
//!
//! The table holds the instance default and only the services that override
//! something, so it is empty on a default install. It is loaded at startup
//! and updated by the healthcheck API; it is read once per container
//! creation, a control-plane operation and never a request path.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use super::HealthcheckOverrides;

#[derive(Debug, Default)]
struct Table {
    instance_default: HealthcheckOverrides,
    by_service: HashMap<i32, HealthcheckOverrides>,
    /// Control-plane container name to service id.
    by_container: HashMap<String, i32>,
}

impl Table {
    fn remove_service(&mut self, service_id: i32) {
        self.by_service.remove(&service_id);
        self.by_container.retain(|_, id| *id != service_id);
    }

    fn insert_service(&mut self, entry: ServiceHealthcheckEntry) {
        self.remove_service(entry.service_id);
        if entry.overrides.is_empty() {
            return;
        }
        if let Some(container) = entry.container_name {
            self.by_container.insert(container, entry.service_id);
        }
        self.by_service.insert(entry.service_id, entry.overrides);
    }
}

static TABLE: LazyLock<RwLock<Table>> = LazyLock::new(|| RwLock::new(Table::default()));

fn read<T>(f: impl FnOnce(&Table) -> T) -> T {
    f(&TABLE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

fn write<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    f(&mut TABLE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// One service's overrides, as the table keeps them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHealthcheckEntry {
    pub service_id: i32,
    /// The container's name when it runs on the control plane, so its
    /// engine can find it. `None` for a worker database, whose container is
    /// created from its workload spec (looked up by id).
    pub container_name: Option<String>,
    pub overrides: HealthcheckOverrides,
}

/// The instance default every service inherits.
pub fn instance_default() -> HealthcheckOverrides {
    read(|t| t.instance_default)
}

pub fn set_instance_default(overrides: HealthcheckOverrides) {
    write(|t| t.instance_default = overrides);
}

/// Record (or, with empty overrides, forget) one service's overrides.
pub fn set_service(entry: ServiceHealthcheckEntry) {
    write(|t| t.insert_service(entry));
}

/// Forget a service's overrides (it was deleted).
pub fn clear_service(service_id: i32) {
    write(|t| t.remove_service(service_id));
}

/// Replace every service's overrides and the instance default (startup).
pub fn replace_all(
    instance: HealthcheckOverrides,
    services: impl IntoIterator<Item = ServiceHealthcheckEntry>,
) {
    let mut table = Table {
        instance_default: instance,
        ..Table::default()
    };
    for entry in services {
        table.insert_service(entry);
    }
    write(|t| *t = table);
}

/// The overrides of the service whose control-plane container this is.
pub fn for_container(container_name: &str) -> HealthcheckOverrides {
    read(|t| {
        t.by_container
            .get(container_name)
            .and_then(|id| t.by_service.get(id))
            .copied()
            .unwrap_or_default()
    })
}

/// The overrides of `service_id`.
pub fn for_service(service_id: i32) -> HealthcheckOverrides {
    read(|t| t.by_service.get(&service_id).copied().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(interval: u32) -> HealthcheckOverrides {
        HealthcheckOverrides {
            interval_secs: Some(interval),
            ..Default::default()
        }
    }

    fn entry(
        service_id: i32,
        container: Option<&str>,
        o: HealthcheckOverrides,
    ) -> ServiceHealthcheckEntry {
        ServiceHealthcheckEntry {
            service_id,
            container_name: container.map(str::to_string),
            overrides: o,
        }
    }

    /// On a table of its own: the process-wide one is shared with every test
    /// that resolves a healthcheck, and an instance default set here would
    /// leak into them.
    #[test]
    fn a_table_keeps_the_instance_default_and_only_services_that_override() {
        let mut table = Table {
            instance_default: overrides(45),
            ..Table::default()
        };
        table.insert_service(entry(1, Some("rustfs-a"), overrides(60)));
        table.insert_service(entry(2, None, overrides(90)));
        table.insert_service(entry(3, Some("redis-c"), HealthcheckOverrides::default()));
        assert_eq!(table.instance_default, overrides(45));
        assert_eq!(table.by_service.len(), 2, "an empty override is not kept");
        assert_eq!(table.by_container.get("rustfs-a"), Some(&1));
        assert!(!table.by_container.contains_key("redis-c"));
        table.remove_service(1);
        assert!(!table.by_container.contains_key("rustfs-a"));
        assert!(!table.by_service.contains_key(&1));
    }

    #[test]
    fn services_are_found_by_container_and_id_and_forgotten_when_empty() {
        // Ids and names no other test uses.
        set_service(entry(8_101, Some("rustfs-registry-test"), overrides(60)));
        set_service(entry(8_102, None, overrides(90)));
        assert_eq!(for_container("rustfs-registry-test"), overrides(60));
        assert_eq!(for_service(8_101), overrides(60));
        assert_eq!(for_service(8_102), overrides(90));
        assert!(for_container("rustfs-registry-unknown").is_empty());

        // A container renamed by a re-set is not found under the old name.
        set_service(entry(8_101, Some("rustfs-registry-renamed"), overrides(61)));
        assert!(for_container("rustfs-registry-test").is_empty());
        assert_eq!(for_container("rustfs-registry-renamed"), overrides(61));

        set_service(entry(
            8_101,
            Some("rustfs-registry-renamed"),
            HealthcheckOverrides::default(),
        ));
        assert!(for_container("rustfs-registry-renamed").is_empty());
        assert!(for_service(8_101).is_empty());
        clear_service(8_102);
        assert!(for_service(8_102).is_empty());
    }
}
