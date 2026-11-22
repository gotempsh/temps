// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a control-plane process dials a standalone managed service.
//!
//! A managed service is stored with a loopback `host` and the host `port` its
//! container publishes. That pair is only an address for a process that
//! shares a network namespace with the Docker host the container runs on. It
//! is wrong in two cases, and each has its own answer:
//!
//! - the service runs on a worker node: the worker's agent publishes the port
//!   on the node's data address, so that address replaces the host;
//! - the control plane itself runs in a container: its loopback is its own,
//!   so it reaches a service on its Docker daemon over the network they share,
//!   by container name and the port the engine listens on inside the
//!   container. The published host port plays no part.
//!
//! Every reader of stored service parameters that opens a connection goes
//! through [`retarget_parameters`] (or [`control_plane_dial_target`] when it
//! holds a parsed host and port), so the rule lives in one place rather than
//! in each reader.

use crate::node_address::{is_local_service_host, url_host};
use crate::runtime::ExecutionEnvironment;

/// A managed service's container as seen from the network it is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEndpoint {
    /// The container's name, which Docker's embedded DNS resolves for every
    /// other container on the same user-defined network.
    pub name: String,
    /// The port the engine listens on inside the container.
    pub port: u16,
}

/// Which rule produced a dial target. Callers log it, and some vet a target
/// that did not come from the service's own parameters differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialTargetSource {
    /// The stored host and port, unchanged: a native control plane dialling a
    /// service on its own machine, or a host the operator set explicitly.
    Stored,
    /// The data address of the worker node the service is placed on, with the
    /// stored (published) port.
    WorkerNode,
    /// The service's container name and in-container port, over the network a
    /// containerized control plane shares with it.
    ContainerNetwork,
}

/// The host and port a control-plane process dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedServiceDialTarget {
    /// Bare host: a name, an IPv4 literal, or an unbracketed IPv6 literal.
    pub host: String,
    pub port: u16,
    pub source: DialTargetSource,
}

impl ManagedServiceDialTarget {
    /// The host ready to embed in a URL authority (IPv6 literals bracketed).
    pub fn url_host(&self) -> String {
        url_host(&self.host)
    }
}

/// How a retargeted host is written back into stored parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostForm {
    /// As a bare host, for clients that take host and port separately.
    Bare,
    /// Bracketed when it is an IPv6 literal, for clients that embed the host
    /// in a URL authority.
    UrlAuthority,
}

/// Supplies the container endpoint of a standalone managed service.
///
/// Deriving it needs the engine a service runs on (its naming scheme and its
/// in-container port), which lives in the providers layer. Crates below that
/// layer take this as an injected capability.
pub trait ContainerEndpointResolver: Send + Sync {
    /// The endpoint of the container behind `service_name`, or `None` when
    /// the service type or its parameters name no engine this build knows.
    fn container_endpoint(
        &self,
        service_type: &str,
        service_name: &str,
        parameters: &serde_json::Value,
    ) -> Option<ContainerEndpoint>;
}

enum Decision<'a> {
    Stored,
    WorkerNode(&'a str),
    ContainerNetwork(&'a ContainerEndpoint),
}

fn decide<'a>(
    execution_environment: ExecutionEnvironment,
    stored_host: &str,
    worker_data_address: Option<&'a str>,
    container: Option<&'a ContainerEndpoint>,
) -> Decision<'a> {
    // A host the operator set explicitly names a real address; it is never
    // replaced.
    if !is_local_service_host(stored_host) {
        return Decision::Stored;
    }
    // Placement comes first: a service on a worker is not on this process's
    // Docker daemon, whatever this process runs in.
    if let Some(address) = worker_data_address {
        return Decision::WorkerNode(address);
    }
    match execution_environment {
        ExecutionEnvironment::Host => Decision::Stored,
        ExecutionEnvironment::Docker => match container {
            Some(container) => Decision::ContainerNetwork(container),
            None => Decision::Stored,
        },
    }
}

/// Resolve where a control-plane process dials a managed service from its
/// parsed stored address.
///
/// `worker_data_address` is the data address of the node the service is
/// placed on, `None` for a service on the control plane's own Docker daemon.
/// `container` is the service's container endpoint when it is known.
pub fn control_plane_dial_target(
    execution_environment: ExecutionEnvironment,
    stored_host: &str,
    stored_port: u16,
    worker_data_address: Option<&str>,
    container: Option<&ContainerEndpoint>,
) -> ManagedServiceDialTarget {
    match decide(
        execution_environment,
        stored_host,
        worker_data_address,
        container,
    ) {
        Decision::Stored => ManagedServiceDialTarget {
            host: stored_host_or_loopback(stored_host),
            port: stored_port,
            source: DialTargetSource::Stored,
        },
        Decision::WorkerNode(address) => ManagedServiceDialTarget {
            host: address.to_string(),
            port: stored_port,
            source: DialTargetSource::WorkerNode,
        },
        Decision::ContainerNetwork(container) => ManagedServiceDialTarget {
            host: container.name.clone(),
            port: container.port,
            source: DialTargetSource::ContainerNetwork,
        },
    }
}

/// Rewrite the `host` and `port` of decrypted service parameters in place so
/// a control-plane process can dial them, and report which rule applied.
///
/// The same decision as [`control_plane_dial_target`], for readers that hand
/// the parameter map on to an engine or build a connection string from it.
/// Parameters that are not a JSON object are left alone.
///
/// The result is for opening a connection only. It must never be persisted,
/// and never be given to an operation that creates or recreates the
/// container: there `port` is the host port to publish.
pub fn retarget_parameters(
    execution_environment: ExecutionEnvironment,
    parameters: &mut serde_json::Value,
    worker_data_address: Option<&str>,
    container: Option<&ContainerEndpoint>,
    host_form: HostForm,
) -> DialTargetSource {
    let Some(map) = parameters.as_object_mut() else {
        return DialTargetSource::Stored;
    };
    let stored_host = map
        .get("host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let written_host = |host: &str| match host_form {
        HostForm::Bare => host.to_string(),
        HostForm::UrlAuthority => url_host(host),
    };
    match decide(
        execution_environment,
        &stored_host,
        worker_data_address,
        container,
    ) {
        Decision::Stored => DialTargetSource::Stored,
        Decision::WorkerNode(address) => {
            map.insert(
                "host".to_string(),
                serde_json::Value::String(written_host(address)),
            );
            DialTargetSource::WorkerNode
        }
        Decision::ContainerNetwork(container) => {
            map.insert(
                "host".to_string(),
                serde_json::Value::String(written_host(&container.name)),
            );
            // Engines read the port as whichever JSON type it was stored in.
            let port = match map.get("port") {
                Some(serde_json::Value::Number(_)) => serde_json::Value::from(container.port),
                _ => serde_json::Value::String(container.port.to_string()),
            };
            map.insert("port".to_string(), port);
            DialTargetSource::ContainerNetwork
        }
    }
}

/// An absent stored host means loopback everywhere the parameters are read.
fn stored_host_or_loopback(stored_host: &str) -> String {
    let host = stored_host.trim();
    if host.is_empty() {
        "localhost".to_string()
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn redis_container() -> ContainerEndpoint {
        ContainerEndpoint {
            name: "redis-cache".to_string(),
            port: 6379,
        }
    }

    #[test]
    fn test_host_control_plane_keeps_the_stored_loopback_address() {
        let target = control_plane_dial_target(
            ExecutionEnvironment::Host,
            "localhost",
            7043,
            None,
            Some(&redis_container()),
        );

        assert_eq!(target.host, "localhost");
        assert_eq!(target.port, 7043);
        assert_eq!(target.source, DialTargetSource::Stored);
    }

    #[test]
    fn test_containerized_control_plane_dials_container_name_and_engine_port() {
        let target = control_plane_dial_target(
            ExecutionEnvironment::Docker,
            "localhost",
            7043,
            None,
            Some(&redis_container()),
        );

        assert_eq!(target.host, "redis-cache");
        assert_eq!(target.port, 6379);
        assert_eq!(target.source, DialTargetSource::ContainerNetwork);
    }

    #[test]
    fn test_worker_placement_wins_over_the_execution_environment() {
        for environment in [ExecutionEnvironment::Host, ExecutionEnvironment::Docker] {
            let target = control_plane_dial_target(
                environment,
                "127.0.0.1",
                7043,
                Some("10.0.0.7"),
                Some(&redis_container()),
            );

            assert_eq!(target.host, "10.0.0.7", "{environment}");
            assert_eq!(target.port, 7043, "{environment}");
            assert_eq!(target.source, DialTargetSource::WorkerNode, "{environment}");
        }
    }

    #[test]
    fn test_operator_set_host_is_never_replaced() {
        for environment in [ExecutionEnvironment::Host, ExecutionEnvironment::Docker] {
            let target = control_plane_dial_target(
                environment,
                "db.internal.example",
                5432,
                Some("10.0.0.7"),
                Some(&redis_container()),
            );

            assert_eq!(target.host, "db.internal.example", "{environment}");
            assert_eq!(target.port, 5432, "{environment}");
            assert_eq!(target.source, DialTargetSource::Stored, "{environment}");
        }
    }

    #[test]
    fn test_unknown_container_in_docker_falls_back_to_stored_address() {
        let target =
            control_plane_dial_target(ExecutionEnvironment::Docker, "localhost", 7043, None, None);

        assert_eq!(target.host, "localhost");
        assert_eq!(target.port, 7043);
        assert_eq!(target.source, DialTargetSource::Stored);
    }

    #[test]
    fn test_empty_stored_host_is_loopback() {
        let target = control_plane_dial_target(ExecutionEnvironment::Host, "  ", 5432, None, None);

        assert_eq!(target.host, "localhost");
    }

    #[test]
    fn test_url_host_brackets_an_ipv6_worker_address() {
        let target = control_plane_dial_target(
            ExecutionEnvironment::Host,
            "localhost",
            6379,
            Some("fd00::7"),
            None,
        );

        assert_eq!(target.host, "fd00::7");
        assert_eq!(target.url_host(), "[fd00::7]");
    }

    #[test]
    fn test_retarget_parameters_in_docker_rewrites_host_and_port() {
        let mut parameters = json!({"host": "localhost", "port": "7043", "password": "pw"});

        let source = retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            None,
            Some(&redis_container()),
            HostForm::UrlAuthority,
        );

        assert_eq!(source, DialTargetSource::ContainerNetwork);
        assert_eq!(
            parameters,
            json!({"host": "redis-cache", "port": "6379", "password": "pw"})
        );
    }

    #[test]
    fn test_retarget_parameters_preserves_a_numeric_port() {
        let mut parameters = json!({"host": "127.0.0.1", "port": 7043});

        retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            None,
            Some(&redis_container()),
            HostForm::Bare,
        );

        assert_eq!(parameters, json!({"host": "redis-cache", "port": 6379}));
    }

    #[test]
    fn test_retarget_parameters_without_a_stored_host_treats_it_as_loopback() {
        let mut parameters = json!({"port": "7043"});

        let source = retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            None,
            Some(&redis_container()),
            HostForm::Bare,
        );

        assert_eq!(source, DialTargetSource::ContainerNetwork);
        assert_eq!(parameters, json!({"host": "redis-cache", "port": "6379"}));
    }

    #[test]
    fn test_retarget_parameters_on_a_worker_keeps_the_published_port() {
        let mut parameters = json!({"host": "localhost", "port": "7043"});

        let source = retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            Some("fd00::7"),
            Some(&redis_container()),
            HostForm::UrlAuthority,
        );

        assert_eq!(source, DialTargetSource::WorkerNode);
        assert_eq!(parameters, json!({"host": "[fd00::7]", "port": "7043"}));
    }

    #[test]
    fn test_retarget_parameters_on_host_leaves_parameters_untouched() {
        let original = json!({"host": "localhost", "port": "7043"});
        let mut parameters = original.clone();

        let source = retarget_parameters(
            ExecutionEnvironment::Host,
            &mut parameters,
            None,
            Some(&redis_container()),
            HostForm::Bare,
        );

        assert_eq!(source, DialTargetSource::Stored);
        assert_eq!(parameters, original);
    }

    #[test]
    fn test_retarget_parameters_keeps_an_operator_set_host() {
        let original = json!({"host": "db.internal.example", "port": "5432"});
        let mut parameters = original.clone();

        let source = retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            Some("10.0.0.7"),
            Some(&redis_container()),
            HostForm::Bare,
        );

        assert_eq!(source, DialTargetSource::Stored);
        assert_eq!(parameters, original);
    }

    #[test]
    fn test_retarget_parameters_ignores_non_object_parameters() {
        let mut parameters = json!("not-an-object");

        let source = retarget_parameters(
            ExecutionEnvironment::Docker,
            &mut parameters,
            None,
            Some(&redis_container()),
            HostForm::Bare,
        );

        assert_eq!(source, DialTargetSource::Stored);
        assert_eq!(parameters, json!("not-an-object"));
    }
}
