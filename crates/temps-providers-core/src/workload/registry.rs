// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which control-plane database containers are created from their workload
//! spec (`workload_mode = 'enforce'`), and the create call the engines make.
//!
//! The engines are constructed in many places (the manager's factory, the
//! upgrade service, restores, imports) from a name and a Docker handle, and
//! every one of them must honour a database's mode, so the mode is looked up
//! by container name here rather than threaded through each constructor.
//! The table is loaded from `external_services` at startup and updated by
//! the mode API; it is read once per container creation (a control-plane
//! operation, never a request path) and is empty unless an administrator
//! moved a database to `enforce`, so every engine keeps its legacy create
//! request byte for byte by default.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use bollard::models::ContainerCreateBody;
use bollard::Docker;
use temps_workload_docker::DockerDriver;
use tracing::info;

use crate::workload::spec::{
    database_instance, DatabaseEngine, DatabaseIdentity, DatabaseProcess, DatabaseWorkloadPlan,
    NodeAttachment, SecretValues,
};
use crate::workload::ServiceWorkloadError;

/// A control-plane database in `enforce` mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcedDatabase {
    pub service_id: i32,
    pub service_name: String,
    pub engine: DatabaseEngine,
}

static ENFORCED: LazyLock<RwLock<HashMap<String, EnforcedDatabase>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Mark `container_name` as created from its spec.
pub fn set_enforced(container_name: &str, database: EnforcedDatabase) {
    let mut table = ENFORCED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table.insert(container_name.to_string(), database);
}

/// Return every container of `service_id` to its engine's own request.
pub fn clear_service(service_id: i32) {
    let mut table = ENFORCED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table.retain(|_, database| database.service_id != service_id);
}

/// Replace the whole table (startup load).
pub fn replace_all(entries: impl IntoIterator<Item = (String, EnforcedDatabase)>) {
    let mut table = ENFORCED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *table = entries.into_iter().collect();
}

pub fn enforced(container_name: &str) -> Option<EnforcedDatabase> {
    ENFORCED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(container_name)
        .cloned()
}

/// The host interface and network a control-plane engine request uses.
fn node_of(body: &ContainerCreateBody) -> NodeAttachment {
    let host_ip = body
        .host_config
        .as_ref()
        .and_then(|h| h.port_bindings.as_ref())
        .and_then(|bindings| bindings.values().flatten().flatten().next())
        .and_then(|binding| binding.host_ip.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let network = body
        .networking_config
        .as_ref()
        .and_then(|n| n.endpoints_config.as_ref())
        .and_then(|endpoints| {
            let mut names: Vec<&String> = endpoints.keys().collect();
            names.sort();
            names.first().map(|name| (*name).clone())
        })
        .or_else(|| {
            body.host_config
                .as_ref()
                .and_then(|h| h.network_mode.clone())
        });
    NodeAttachment {
        host_ip,
        network,
        dns: body.host_config.as_ref().and_then(|h| h.dns.clone()),
    }
}

/// The plan an engine request becomes in `enforce` mode.
pub fn enforced_plan(
    database: &EnforcedDatabase,
    container_name: &str,
    body: &ContainerCreateBody,
) -> Result<(DatabaseWorkloadPlan, DatabaseProcess, NodeAttachment), ServiceWorkloadError> {
    let process = DatabaseProcess::from_create_body(database.engine, body).map_err(|reason| {
        ServiceWorkloadError::Process {
            service_id: database.service_id,
            container: container_name.to_string(),
            reason,
        }
    })?;
    let identity = DatabaseIdentity {
        service_id: database.service_id,
        service_name: database.service_name.clone(),
        engine: database.engine,
        node_id: None,
        container_name: container_name.to_string(),
    };
    let plan = DatabaseWorkloadPlan::admit(&identity, &process)?;
    Ok((plan, process, node_of(body)))
}

/// Create a control-plane engine's database container.
///
/// `None`: the database is not in `enforce` mode, and the engine sends its
/// own request exactly as before. Otherwise the engine's request is read
/// for the database's process, turned into its admitted spec, and the
/// container is created from that through the Docker driver; the result is
/// the new container's id.
pub async fn enforced_create(
    docker: &Docker,
    container_name: &str,
    body: &ContainerCreateBody,
) -> Option<Result<String, ServiceWorkloadError>> {
    let database = enforced(container_name)?;
    Some(create_enforced(docker, &database, container_name, body).await)
}

async fn create_enforced(
    docker: &Docker,
    database: &EnforcedDatabase,
    container_name: &str,
    body: &ContainerCreateBody,
) -> Result<String, ServiceWorkloadError> {
    let (plan, process, node) = enforced_plan(database, container_name, body)?;
    let values = process.secret_values();
    let instance = database_instance(&plan, SecretValues::Resolved(&values), &node)?;
    let handle = DockerDriver::new(docker.clone())
        .create_data_with_binds(
            &instance.spec,
            &instance.probes,
            &instance.extras,
            &instance.attach,
            Vec::new(),
        )
        .await
        .map_err(|e| ServiceWorkloadError::Docker {
            operation: "create",
            container: container_name.to_string(),
            reason: e.to_string(),
        })?;
    info!(
        service_id = database.service_id,
        container = container_name,
        engine = %database.engine,
        workload_mode = "enforce",
        "Created database container from its admitted workload spec"
    );
    Ok(handle.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_keyed_by_container_and_cleared_by_service() {
        let database = EnforcedDatabase {
            service_id: 9_001,
            service_name: "registry-test".into(),
            engine: DatabaseEngine::Redis,
        };
        set_enforced("redis-registry-test", database.clone());
        assert_eq!(enforced("redis-registry-test"), Some(database));
        assert_eq!(enforced("redis-registry-other"), None);
        clear_service(9_001);
        assert_eq!(enforced("redis-registry-test"), None);
    }
}
