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
/// exists. Any other daemon error is returned unchanged.
pub async fn ensure_bridge_network(
    docker: &Docker,
    name: &str,
) -> Result<NetworkEnsured, DockerError> {
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
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

async fn network_exists(docker: &Docker, name: &str) -> Result<bool, DockerError> {
    let networks = docker.list_networks(None::<ListNetworksOptions>).await?;
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

    /// Many concurrent callers racing to create the same network must all
    /// succeed, and exactly one of them creates it. Needs a Docker daemon;
    /// skipped when none is reachable.
    #[tokio::test]
    async fn concurrent_callers_all_succeed_and_one_creates() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            eprintln!("skipping: no Docker client");
            return;
        };
        if docker.ping().await.is_err() {
            eprintln!("skipping: Docker daemon not reachable");
            return;
        }
        let name = format!(
            "temps-test-ensure-net-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );

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
