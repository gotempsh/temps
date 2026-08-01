//! Cluster-wide observation for the cost/overprovisioning analysis
//!
//! Captured during `describe_project()` (the only async phase with cluster
//! access) and stashed as JSON in `ProjectSnapshot.source_metadata`, then
//! read back by the synchronous plan generation. Every fetch degrades
//! gracefully — a cluster without metrics-server still produces an
//! observation, just without the "actual usage" column.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

use crate::client::KubeClient;
use crate::error::KubernetesImportError;
use crate::model::{bytes_to_mb, parse_cpu_millis, parse_memory_bytes, Node, NodeMetrics, Pod};

/// Key under which the observation is stored in `ProjectSnapshot.source_metadata`
pub const CLUSTER_OBSERVATION_KEY: &str = "cluster_observation";

/// One observed cluster node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedNode {
    pub name: String,
    /// `node.kubernetes.io/instance-type` label
    pub instance_type: Option<String>,
    /// `topology.kubernetes.io/region` label
    pub region: Option<String>,
    /// `spec.providerID` (e.g. `aws:///us-east-1a/i-abc`, `hcloud://123`)
    pub provider_id: Option<String>,
    /// Managed-cluster hints from node labels
    pub is_eks: bool,
    pub is_gke: bool,
    pub is_ask: bool,
    pub allocatable_cpu_millis: i64,
    pub allocatable_memory_mb: i64,
    pub capacity_cpu_millis: i64,
    pub capacity_memory_mb: i64,
}

/// Cluster-wide snapshot of nodes, requests, and (optionally) live usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterObservation {
    pub nodes: Vec<ObservedNode>,
    /// Sum of container resource requests across running pods (millicores / MB)
    pub requested_cpu_millis: i64,
    pub requested_memory_mb: i64,
    /// Live usage summed from `metrics.k8s.io` node metrics. `None` when
    /// metrics-server is not installed.
    pub actual_cpu_millis: Option<i64>,
    pub actual_memory_mb: Option<i64>,
    /// Number of running pods observed
    pub running_pods: usize,
}

/// Observe the whole cluster: nodes, pod requests, and node metrics.
pub async fn observe_cluster(
    client: &KubeClient,
) -> Result<ClusterObservation, KubernetesImportError> {
    let nodes: Vec<Node> = client.list("/api/v1/nodes").await?;

    let observed_nodes: Vec<ObservedNode> = nodes
        .iter()
        .map(|node| {
            let labels = &node.metadata.labels;
            let status = node.status.clone().unwrap_or_default();
            let get_quantity = |map: &HashMap<String, String>, key: &str| -> (i64, i64) {
                let cpu = map
                    .get("cpu")
                    .and_then(|q| parse_cpu_millis(q))
                    .unwrap_or(0);
                let mem = map
                    .get(key)
                    .and_then(|q| parse_memory_bytes(q))
                    .map(bytes_to_mb)
                    .unwrap_or(0);
                (cpu, mem)
            };
            let (allocatable_cpu, allocatable_mem) = get_quantity(&status.allocatable, "memory");
            let (capacity_cpu, capacity_mem) = get_quantity(&status.capacity, "memory");
            ObservedNode {
                name: node.metadata.name.clone(),
                instance_type: labels.get("node.kubernetes.io/instance-type").cloned(),
                region: labels.get("topology.kubernetes.io/region").cloned(),
                provider_id: node.spec.as_ref().and_then(|s| s.provider_id.clone()),
                is_eks: labels.keys().any(|k| k.starts_with("eks.amazonaws.com/")),
                is_gke: labels.keys().any(|k| k.starts_with("cloud.google.com/gke")),
                is_ask: labels
                    .keys()
                    .any(|k| k.starts_with("kubernetes.azure.com/")),
                allocatable_cpu_millis: allocatable_cpu,
                allocatable_memory_mb: allocatable_mem,
                capacity_cpu_millis: capacity_cpu,
                capacity_memory_mb: capacity_mem,
            }
        })
        .collect();

    // Sum requests across running pods (cluster-wide, including system pods —
    // they are part of what the cluster is sized for).
    let pods: Vec<Pod> = client
        .list("/api/v1/pods?fieldSelector=status.phase%3DRunning")
        .await?;
    let mut requested_cpu = 0i64;
    let mut requested_mem_bytes = 0i64;
    for pod in &pods {
        if let Some(spec) = &pod.spec {
            for container in &spec.containers {
                if let Some(cpu) = container
                    .resources
                    .requests
                    .get("cpu")
                    .and_then(|q| parse_cpu_millis(q))
                {
                    requested_cpu += cpu;
                }
                if let Some(mem) = container
                    .resources
                    .requests
                    .get("memory")
                    .and_then(|q| parse_memory_bytes(q))
                {
                    requested_mem_bytes += mem;
                }
            }
        }
    }

    // Live usage — optional (metrics-server may be absent). A failed call is
    // degraded to None, not an error: the analysis still works from requests.
    let (actual_cpu, actual_mem) = match client
        .list::<NodeMetrics>("/apis/metrics.k8s.io/v1beta1/nodes")
        .await
    {
        Ok(metrics) if !metrics.is_empty() => {
            let cpu: i64 = metrics
                .iter()
                .filter_map(|m| m.usage.get("cpu").and_then(|q| parse_cpu_millis(q)))
                .sum();
            let mem_bytes: i64 = metrics
                .iter()
                .filter_map(|m| m.usage.get("memory").and_then(|q| parse_memory_bytes(q)))
                .sum();
            (Some(cpu), Some(bytes_to_mb(mem_bytes)))
        }
        Ok(_) => (None, None),
        Err(e) => {
            debug!(
                "metrics.k8s.io unavailable ({}) — cost analysis will use requests only",
                e
            );
            (None, None)
        }
    };

    Ok(ClusterObservation {
        nodes: observed_nodes,
        requested_cpu_millis: requested_cpu,
        requested_memory_mb: bytes_to_mb(requested_mem_bytes),
        actual_cpu_millis: actual_cpu,
        actual_memory_mb: actual_mem,
        running_pods: pods.len(),
    })
}
