//! Cluster cost & rightsizing analysis types
//!
//! Produced by cluster-level importers (currently Kubernetes) that can observe
//! both what a cluster *costs* (nodes, instance types, managed control plane)
//! and what it *actually uses* (the metrics API). The analysis answers the
//! three questions a user asks before migrating:
//!
//! 1. **What am I paying today?** — node inventory priced from a static table
//!    of common instance types, plus managed control-plane fees (EKS/GKE).
//! 2. **How overprovisioned am I?** — resource requests vs. capacity vs.
//!    measured usage.
//! 3. **What would this cost on temps?** — the smallest Hetzner server that
//!    fits the measured load with headroom, and the resulting savings.
//!
//! # Honesty rules
//!
//! Every number here is an **estimate** and must be labelled as such in the
//! UI. When something could not be measured (no metrics-server, unknown
//! instance type), the corresponding field is `None` and a human-readable
//! explanation is appended to [`CostAnalysis::notes`] — never silently
//! omitted. See `usage_source` for how the usage numbers were obtained.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Full cluster cost + rightsizing analysis attached to an import plan.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CostAnalysis {
    /// Detected cloud provider (from node `providerID` prefixes)
    pub provider: Option<CloudProvider>,
    /// Per-node inventory with price estimates where the instance type is known
    pub nodes: Vec<NodeCostInfo>,
    /// Estimated total infrastructure cost per month in USD (compute nodes +
    /// control-plane fee). `None` when no node could be priced.
    pub current_monthly_usd: Option<f64>,
    /// Managed control-plane fee included in `current_monthly_usd` (EKS/GKE
    /// charge ~$73/mo per cluster). `None` when not applicable/unknown.
    pub control_plane_monthly_usd: Option<f64>,
    /// Total cluster capacity (sum of node allocatable resources)
    pub capacity: ClusterCapacity,
    /// Sum of pod resource *requests* across running pods — what the
    /// scheduler has reserved, i.e. what the cluster is sized for.
    pub requested: ResourceFootprint,
    /// Measured usage from the metrics API (`metrics.k8s.io`).
    /// `None` when metrics-server is not installed.
    pub actual_usage: Option<ResourceFootprint>,
    /// How the usage numbers were obtained (drives UI wording)
    pub usage_source: UsageSource,
    /// Requests-vs-capacity-vs-usage assessment
    pub overprovisioning: OverprovisioningAssessment,
    /// The temps/Hetzner target sizing and savings estimate
    pub recommendation: TargetRecommendation,
    /// Honesty notes: what could not be measured, which numbers are
    /// estimates, and any assumptions made. Always shown to the user.
    pub notes: Vec<String>,
}

/// Cloud provider detected from node metadata
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    Aws,
    Gcp,
    Azure,
    Hetzner,
    DigitalOcean,
    Other,
}

impl std::fmt::Display for CloudProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudProvider::Aws => write!(f, "AWS"),
            CloudProvider::Gcp => write!(f, "Google Cloud"),
            CloudProvider::Azure => write!(f, "Azure"),
            CloudProvider::Hetzner => write!(f, "Hetzner"),
            CloudProvider::DigitalOcean => write!(f, "DigitalOcean"),
            CloudProvider::Other => write!(f, "Unknown provider"),
        }
    }
}

/// One cluster node with capacity and (when priceable) a cost estimate
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NodeCostInfo {
    /// Node name
    pub name: String,
    /// Instance type from `node.kubernetes.io/instance-type` (e.g. "m5.xlarge")
    pub instance_type: Option<String>,
    /// Region from `topology.kubernetes.io/region`
    pub region: Option<String>,
    /// CPU capacity in millicores
    pub cpu_millis: i64,
    /// Memory capacity in MB
    pub memory_mb: i64,
    /// Estimated on-demand monthly price in USD. `None` when the instance
    /// type is unknown or not in the price table.
    pub monthly_usd: Option<f64>,
}

/// Total cluster capacity (sum of node allocatable resources)
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ClusterCapacity {
    /// Number of nodes
    pub node_count: usize,
    /// Total allocatable CPU in millicores
    pub cpu_millis: i64,
    /// Total allocatable memory in MB
    pub memory_mb: i64,
}

/// A CPU + memory footprint (requests or measured usage)
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ResourceFootprint {
    /// CPU in millicores
    pub cpu_millis: i64,
    /// Memory in MB
    pub memory_mb: i64,
}

/// How the "actual usage" numbers were obtained
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UsageSource {
    /// Live measurements from `metrics.k8s.io` (metrics-server). These are
    /// point-in-time samples, not long-term percentiles.
    MetricsApi,
    /// metrics-server unavailable — sizing fell back to resource requests,
    /// which are typically inflated.
    RequestsOnly,
    /// Neither metrics nor requests available (no pods observed)
    Unavailable,
}

/// Requests-vs-capacity-vs-usage assessment
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OverprovisioningAssessment {
    /// Measured CPU usage as % of cluster capacity (`None` without metrics)
    pub cpu_utilization_pct: Option<f64>,
    /// Measured memory usage as % of cluster capacity (`None` without metrics)
    pub memory_utilization_pct: Option<f64>,
    /// Requested CPU as % of cluster capacity
    pub cpu_requested_pct: Option<f64>,
    /// Requested memory as % of cluster capacity
    pub memory_requested_pct: Option<f64>,
    /// Ratio of requested CPU to measured CPU usage (e.g. 40.0 = requests
    /// reserve 40× what the workloads actually use). `None` without metrics.
    pub cpu_request_inflation_ratio: Option<f64>,
    /// Ratio of requested memory to measured memory usage
    pub memory_request_inflation_ratio: Option<f64>,
    /// Overall verdict
    pub verdict: OverprovisioningVerdict,
    /// Human-readable explanation of the verdict, e.g. "Cluster capacity is
    /// 8 vCPU but measured usage is 0.3 vCPU (3.7%) — severely overprovisioned"
    pub explanation: String,
}

/// Overall overprovisioning verdict
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OverprovisioningVerdict {
    /// Measured utilization below 25% of capacity
    Severe,
    /// Measured utilization between 25% and 60% of capacity
    Moderate,
    /// Measured utilization above 60% of capacity
    Reasonable,
    /// Could not be assessed (no metrics and no requests)
    Unknown,
}

/// The temps/Hetzner target sizing and savings estimate
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TargetRecommendation {
    /// Recommended Hetzner server type (e.g. "cpx32")
    pub server_type: String,
    /// vCPUs of the recommended server
    pub vcpus: i32,
    /// Memory (GB) of the recommended server
    pub memory_gb: i32,
    /// Estimated monthly price of the recommended server in EUR
    pub monthly_eur: f64,
    /// Estimated monthly savings in USD (current cost minus target cost,
    /// treating EUR≈USD for the rough comparison — disclaimed in `notes`).
    /// `None` when the current cost is unknown.
    pub monthly_savings_usd: Option<f64>,
    /// `monthly_savings_usd × 12`
    pub yearly_savings_usd: Option<f64>,
    /// Whether the workloads fit a single recommended server. When `false`,
    /// the rationale explains the multi-node option (temps worker nodes).
    pub fits_single_node: bool,
    /// What the sizing was based on, e.g. "2× measured usage + temps
    /// platform overhead" or "resource requests (no metrics available)"
    pub sizing_basis: String,
    /// Human-readable recommendation summary
    pub rationale: String,
}
