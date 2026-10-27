// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which deployments on this host were created without the cluster resolver.
//!
//! A container's nameservers are fixed when it is created, so turning cluster
//! DNS on reaches new containers only. The ones already running keep
//! Docker's embedded DNS and cannot resolve `*.temps.local` until they are
//! redeployed. This module finds them so the console can say which ones.
//!
//! Only deployment containers are checked. A standalone database never
//! connects to another service by name, and the members of a cluster that
//! spans nodes can only be created while cluster DNS is on, so they always
//! have the resolver.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::IpAddr;

/// Label every deployment container carries, holding its deployment id.
pub const DEPLOY_ID_LABEL: &str = "sh.temps.deploy_id";

/// Most deployment ids reported in one pass. Bounds the heartbeat and the
/// status response on a host with an unusual number of stale containers.
pub const MAX_REPORTED_DEPLOYMENTS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    deployment_id: i64,
    nameservers: Vec<String>,
}

/// Remembers each running deployment container's nameservers, so a pass
/// inspects only the containers it has not seen before.
#[derive(Debug, Default)]
pub struct ResolverCoverage {
    seen: HashMap<String, Seen>,
}

impl ResolverCoverage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deployment ids with a running container whose nameservers do not
    /// include `resolver`, sorted and capped at
    /// [`MAX_REPORTED_DEPLOYMENTS`].
    ///
    /// One `list_containers` per pass, plus one inspect per container seen
    /// for the first time; a container that disappears is forgotten.
    pub async fn deployments_without(
        &mut self,
        docker: &bollard::Docker,
        resolver: IpAddr,
    ) -> Result<Vec<i64>, bollard::errors::Error> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("label".to_string(), vec![DEPLOY_ID_LABEL.to_string()])]);
        let options = bollard::query_parameters::ListContainersOptionsBuilder::new()
            .filters(&filters)
            .build();
        let containers = docker.list_containers(Some(options)).await?;
        let running: HashSet<String> = containers.iter().filter_map(|c| c.id.clone()).collect();
        self.seen.retain(|id, _| running.contains(id));

        for container in &containers {
            let Some(id) = container.id.as_deref() else {
                continue;
            };
            if self.seen.contains_key(id) {
                continue;
            }
            let Some(deployment_id) = container
                .labels
                .as_ref()
                .and_then(|labels| labels.get(DEPLOY_ID_LABEL))
                .and_then(|value| value.parse::<i64>().ok())
            else {
                continue;
            };
            let inspected = match docker
                .inspect_container(
                    id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(inspected) => inspected,
                // Gone between the list and the inspect: next pass.
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => continue,
                Err(error) => return Err(error),
            };
            let nameservers = inspected
                .host_config
                .and_then(|host| host.dns)
                .unwrap_or_default();
            self.seen.insert(
                id.to_string(),
                Seen {
                    deployment_id,
                    nameservers,
                },
            );
        }
        Ok(uncovered(self.seen.values(), resolver))
    }
}

fn uncovered<'a>(seen: impl Iterator<Item = &'a Seen>, resolver: IpAddr) -> Vec<i64> {
    let resolver = resolver.to_string();
    seen.filter(|entry| !entry.nameservers.iter().any(|ns| ns == &resolver))
        .map(|entry| entry.deployment_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_REPORTED_DEPLOYMENTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(deployment_id: i64, nameservers: &[&str]) -> Seen {
        Seen {
            deployment_id,
            nameservers: nameservers.iter().map(|ns| ns.to_string()).collect(),
        }
    }

    #[test]
    fn containers_created_before_the_resolver_are_reported_once_per_deployment() {
        let resolver: IpAddr = "10.202.1.1".parse().unwrap();
        let entries = [
            // Created while cluster DNS was on.
            seen(15, &["10.202.1.1", "1.1.1.1", "8.8.8.8"]),
            // Created before: Docker's embedded DNS.
            seen(7, &[]),
            // A second replica of the same stale deployment.
            seen(7, &[]),
            // Created while another resolver address was current.
            seen(9, &["10.202.9.1", "1.1.1.1"]),
        ];

        assert_eq!(uncovered(entries.iter(), resolver), vec![7, 9]);
    }

    #[test]
    fn the_report_is_bounded() {
        let resolver: IpAddr = "10.202.1.1".parse().unwrap();
        let entries: Vec<Seen> = (0..(MAX_REPORTED_DEPLOYMENTS as i64 + 50))
            .map(|id| seen(id, &[]))
            .collect();

        let reported = uncovered(entries.iter(), resolver);

        assert_eq!(reported.len(), MAX_REPORTED_DEPLOYMENTS);
        assert_eq!(reported.first(), Some(&0));
    }
}
