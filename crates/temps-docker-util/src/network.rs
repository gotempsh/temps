// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Idempotent creation of a named Docker bridge network.
//!
//! Every place that needs the shared Temps network (single-container
//! deployments, compose stacks, external services) used to carry its own copy
//! of "list networks, create it if missing". That is a check-then-act: two
//! deployments starting together both see the network missing, both create it,
//! and the daemon answers the second with `409 Conflict`. Reporting that as a
//! failure failed a deployment in exactly the case where the network it asked
//! for exists. This module is the one implementation, so the race is handled
//! once for every caller.

use bollard::errors::Error as DockerError;
use bollard::models::NetworkCreateRequest;
use bollard::query_parameters::ListNetworksOptions;
use bollard::Docker;

use crate::error::DockerUtilError;

/// What [`ensure_bridge_network`] found or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkEnsured {
    /// The network was already there.
    Existing,
    /// This call created it.
    Created,
    /// Another caller created it between our lookup and our create.
    CreatedConcurrently,
}

/// Ensures a bridge network named `name` exists, creating it if needed.
///
/// Safe to call concurrently from any number of tasks or processes: losing
/// the creation race is success, because the network the caller asked for
/// exists. Any other daemon error is returned. An existing network is taken
/// as it is: this does not compare its settings with a plain bridge.
pub async fn ensure_bridge_network(
    docker: &Docker,
    name: &str,
) -> Result<NetworkEnsured, DockerUtilError> {
    if network_exists(docker, name).await? {
        return Ok(NetworkEnsured::Existing);
    }

    let created = docker
        .create_network(NetworkCreateRequest {
            name: name.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        })
        .await;

    match created {
        Ok(_) => Ok(NetworkEnsured::Created),
        Err(error) if is_conflict(&error) => {
            // A 409 only says the name is taken. Confirm it is taken by a
            // network before calling this a success, so a conflict for any
            // other reason still surfaces as the error it is.
            if network_exists(docker, name).await? {
                Ok(NetworkEnsured::CreatedConcurrently)
            } else {
                Err(create_failed(name, error))
            }
        }
        Err(error) => Err(create_failed(name, error)),
    }
}

fn create_failed(name: &str, source: DockerError) -> DockerUtilError {
    DockerUtilError::Network {
        name: name.to_string(),
        operation: "create the network",
        source,
    }
}

async fn network_exists(docker: &Docker, name: &str) -> Result<bool, DockerUtilError> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .map_err(|source| DockerUtilError::Network {
            name: name.to_string(),
            operation: "list networks",
            source,
        })?;
    Ok(networks.iter().any(|n| n.name.as_deref() == Some(name)))
}

/// The daemon's answer when a resource with the requested name already exists.
fn is_conflict(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 409,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{docker_or_skip, unique_name};

    fn server_error(status_code: u16) -> DockerError {
        DockerError::DockerResponseServerError {
            status_code,
            message: "network with name temps already exists".to_string(),
        }
    }

    #[test]
    fn only_a_409_counts_as_a_conflict() {
        assert!(is_conflict(&server_error(409)));
        // A daemon failure must never read as "already exists".
        assert!(!is_conflict(&server_error(500)));
        assert!(!is_conflict(&server_error(404)));
    }

    /// An unreachable daemon fails with the network's name, and is not read as
    /// "already exists".
    #[tokio::test]
    async fn a_daemon_failure_is_a_typed_error_naming_the_network() {
        let docker =
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .expect("building the client does not connect");
        let error = ensure_bridge_network(&docker, "app-net")
            .await
            .expect_err("no daemon answers on that port");
        assert!(
            matches!(
                &error,
                DockerUtilError::Network { name, operation: "list networks", .. } if name == "app-net"
            ),
            "{error:?}"
        );
    }

    /// A network that already exists is reported as such and left alone, even
    /// when it is not what this call would have created.
    #[tokio::test]
    async fn an_existing_network_is_left_as_it_is() {
        let Some(docker) = docker_or_skip("an_existing_network_is_left_as_it_is").await else {
            return;
        };
        let name = unique_name("existing-net");
        let created = docker
            .create_network(NetworkCreateRequest {
                name: name.clone(),
                driver: Some("bridge".to_string()),
                internal: Some(true),
                ..Default::default()
            })
            .await;
        if let Err(error) = created {
            panic!("create the test network: {error}");
        }

        let outcome = ensure_bridge_network(&docker, &name).await;
        let inspected = docker
            .inspect_network(
                &name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await;
        let _ = docker.remove_network(&name).await;

        assert_eq!(outcome.expect("ensure"), NetworkEnsured::Existing);
        assert_eq!(inspected.expect("inspect").internal, Some(true));
    }

    /// What a new network gets created with is part of the contract: a plain
    /// bridge with no labels or options of its own.
    #[tokio::test]
    async fn a_new_network_is_a_plain_bridge() {
        let Some(docker) = docker_or_skip("a_new_network_is_a_plain_bridge").await else {
            return;
        };
        let name = unique_name("new-net");
        let outcome = ensure_bridge_network(&docker, &name).await;
        let inspected = docker
            .inspect_network(
                &name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await;
        let _ = docker.remove_network(&name).await;

        assert_eq!(outcome.expect("ensure"), NetworkEnsured::Created);
        let network = inspected.expect("inspect");
        assert_eq!(network.driver.as_deref(), Some("bridge"));
        assert_eq!(network.internal, Some(false));
        assert!(network.labels.unwrap_or_default().is_empty());
    }

    /// Many concurrent callers racing to create the same network must all
    /// succeed, and exactly one of them creates it. Needs a Docker daemon;
    /// skipped when none is reachable.
    #[tokio::test]
    async fn concurrent_callers_all_succeed_and_one_creates() {
        let Some(docker) = docker_or_skip("concurrent_callers_all_succeed").await else {
            return;
        };
        let name = unique_name("concurrent-net");

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let docker = docker.clone();
                let name = name.clone();
                tokio::spawn(async move { ensure_bridge_network(&docker, &name).await })
            })
            .collect();
        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.expect("task panicked"));
        }
        let _ = docker.remove_network(&name).await;

        let outcomes: Vec<NetworkEnsured> = outcomes
            .into_iter()
            .map(|r| r.expect("every concurrent caller must succeed"))
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == NetworkEnsured::Created)
                .count(),
            1,
            "exactly one caller creates the network: {outcomes:?}"
        );
    }
}
