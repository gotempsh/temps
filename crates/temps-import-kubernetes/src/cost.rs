//! Cost + overprovisioning analysis
//!
//! Turns a [`ClusterObservation`] into the [`CostAnalysis`] attached to the
//! import plan: what the cluster costs today, how little of it is used, and
//! what the same workloads would cost on a Hetzner server running temps.
//!
//! # Pricing honesty
//!
//! All prices are static estimates: cloud prices are approximate on-demand
//! list prices (mid-2026, region-averaged); Hetzner prices are the CPX-line
//! monthly caps. Unknown instance types are NOT guessed — they show up as
//! unpriced nodes with an explicit note. The EUR→USD comparison treats the
//! two roughly at par and says so.

use temps_import_types::cost::{
    CloudProvider, ClusterCapacity, CostAnalysis, NodeCostInfo, OverprovisioningAssessment,
    OverprovisioningVerdict, ResourceFootprint, TargetRecommendation, UsageSource,
};

use crate::cluster::ClusterObservation;

/// Approximate on-demand monthly USD prices for common instance types.
/// Deliberately small: covers the shapes small clusters actually run on.
const INSTANCE_PRICES_USD: &[(&str, f64)] = &[
    // AWS (us-east-1-ish on-demand)
    ("t3.small", 15.0),
    ("t3.medium", 30.0),
    ("t3.large", 60.0),
    ("t3.xlarge", 120.0),
    ("t3a.medium", 27.0),
    ("t3a.large", 55.0),
    ("m5.large", 70.0),
    ("m5.xlarge", 140.0),
    ("m5.2xlarge", 280.0),
    ("m6i.large", 70.0),
    ("m6i.xlarge", 140.0),
    ("m7i.large", 73.0),
    ("m7i.xlarge", 146.0),
    ("c5.large", 62.0),
    ("c5.xlarge", 124.0),
    ("r5.large", 92.0),
    ("r5.xlarge", 184.0),
    // GCP
    ("e2-small", 13.0),
    ("e2-medium", 25.0),
    ("e2-standard-2", 49.0),
    ("e2-standard-4", 98.0),
    ("e2-standard-8", 196.0),
    ("n2-standard-2", 71.0),
    ("n2-standard-4", 142.0),
    ("n2-standard-8", 284.0),
    // Azure
    ("Standard_B2s", 30.0),
    ("Standard_B2ms", 60.0),
    ("Standard_D2s_v3", 70.0),
    ("Standard_D4s_v3", 140.0),
    ("Standard_D2s_v5", 70.0),
    ("Standard_D4s_v5", 140.0),
    // DigitalOcean (DOKS node slugs)
    ("s-2vcpu-2gb", 18.0),
    ("s-2vcpu-4gb", 24.0),
    ("s-4vcpu-8gb", 48.0),
    ("s-8vcpu-16gb", 96.0),
    // Hetzner (already ~EUR, treated at par)
    ("cpx11", 5.0),
    ("cpx12", 14.0),
    ("cpx21", 9.0),
    ("cpx22", 24.0),
    ("cpx31", 16.0),
    ("cpx32", 47.0),
    ("cpx41", 30.0),
    ("cpx42", 94.0),
    ("cpx51", 65.0),
    ("cax11", 4.0),
    ("cax21", 7.0),
    ("cax31", 14.0),
];

/// Managed control-plane fee (EKS and GKE standard both charge ~$0.10/h)
const MANAGED_CONTROL_PLANE_USD: f64 = 73.0;

/// Hetzner CPX target ladder: (server_type, vcpus, memory_gb, monthly_eur).
/// cpx12/cpx22 are the migration-lab-verified monthly caps; larger sizes are
/// linear extrapolations, flagged as estimates in the notes.
const HETZNER_LADDER: &[(&str, i32, i32, f64)] = &[
    ("cpx22", 2, 4, 23.58),
    ("cpx32", 4, 8, 47.20),
    ("cpx42", 8, 16, 94.30),
    ("cpx52", 16, 32, 188.60),
];

/// Overhead the temps platform itself needs on the target server
/// (control plane, Postgres/TimescaleDB, proxy — sized from the cpx22
/// reference deployment).
const TEMPS_OVERHEAD_CPU_MILLIS: i64 = 500;
const TEMPS_OVERHEAD_MEMORY_MB: i64 = 1536;

/// Headroom multiplier applied to measured usage when sizing the target
const USAGE_HEADROOM: f64 = 2.0;

fn detect_provider(observation: &ClusterObservation) -> Option<CloudProvider> {
    observation.nodes.iter().find_map(|node| {
        let provider_id = node.provider_id.as_deref()?;
        if provider_id.starts_with("aws://") {
            Some(CloudProvider::Aws)
        } else if provider_id.starts_with("gce://") {
            Some(CloudProvider::Gcp)
        } else if provider_id.starts_with("azure://") {
            Some(CloudProvider::Azure)
        } else if provider_id.starts_with("hcloud://") {
            Some(CloudProvider::Hetzner)
        } else if provider_id.starts_with("digitalocean://") {
            Some(CloudProvider::DigitalOcean)
        } else {
            Some(CloudProvider::Other)
        }
    })
}

fn price_instance(instance_type: &str) -> Option<f64> {
    INSTANCE_PRICES_USD
        .iter()
        .find(|(name, _)| *name == instance_type)
        .map(|(_, price)| *price)
}

fn pct(part: i64, whole: i64) -> Option<f64> {
    if whole <= 0 {
        return None;
    }
    Some((part as f64 / whole as f64 * 1000.0).round() / 10.0)
}

fn ratio(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator <= 0 {
        return None;
    }
    Some((numerator as f64 / denominator as f64 * 10.0).round() / 10.0)
}

/// Build the full cost analysis from a cluster observation.
pub fn compute_cost_analysis(observation: &ClusterObservation) -> CostAnalysis {
    let mut notes = vec![
        "All prices are estimates: cloud prices are approximate on-demand list prices; Hetzner prices are CPX monthly caps. EUR and USD are compared roughly at par.".to_string(),
    ];

    let provider = detect_provider(observation);

    // -- Node inventory + current cost -------------------------------------
    let mut nodes = Vec::with_capacity(observation.nodes.len());
    let mut priced_total = 0.0f64;
    let mut any_priced = false;
    let mut unpriced: Vec<String> = Vec::new();

    for node in &observation.nodes {
        let monthly_usd = node.instance_type.as_deref().and_then(price_instance);
        match monthly_usd {
            Some(price) => {
                priced_total += price;
                any_priced = true;
            }
            None => unpriced.push(format!(
                "{} ({})",
                node.name,
                node.instance_type.as_deref().unwrap_or("no instance type")
            )),
        }
        nodes.push(NodeCostInfo {
            name: node.name.clone(),
            instance_type: node.instance_type.clone(),
            region: node.region.clone(),
            cpu_millis: node.capacity_cpu_millis,
            memory_mb: node.capacity_memory_mb,
            monthly_usd,
        });
    }
    if !unpriced.is_empty() {
        notes.push(format!(
            "Could not price {} node(s): {} — the current-cost estimate only covers priced nodes",
            unpriced.len(),
            unpriced.join(", ")
        ));
    }

    let is_managed = observation.nodes.iter().any(|n| n.is_eks || n.is_gke);
    let control_plane_monthly_usd = if is_managed {
        notes.push(
            "Managed control-plane fee (~$73/mo for EKS/GKE) included in the current cost"
                .to_string(),
        );
        Some(MANAGED_CONTROL_PLANE_USD)
    } else {
        None
    };

    let current_monthly_usd = if any_priced {
        Some(priced_total + control_plane_monthly_usd.unwrap_or(0.0))
    } else {
        notes.push(
            "No node instance type could be priced — enter your actual monthly bill to compare"
                .to_string(),
        );
        control_plane_monthly_usd
    };

    // -- Capacity / requested / actual -------------------------------------
    let capacity = ClusterCapacity {
        node_count: observation.nodes.len(),
        cpu_millis: observation
            .nodes
            .iter()
            .map(|n| n.allocatable_cpu_millis)
            .sum(),
        memory_mb: observation
            .nodes
            .iter()
            .map(|n| n.allocatable_memory_mb)
            .sum(),
    };
    let requested = ResourceFootprint {
        cpu_millis: observation.requested_cpu_millis,
        memory_mb: observation.requested_memory_mb,
    };
    let actual_usage = match (observation.actual_cpu_millis, observation.actual_memory_mb) {
        (Some(cpu), Some(memory)) => Some(ResourceFootprint {
            cpu_millis: cpu,
            memory_mb: memory,
        }),
        _ => None,
    };
    let usage_source = if actual_usage.is_some() {
        notes.push(
            "Usage numbers are point-in-time samples from metrics-server, not long-term percentiles — sustained peaks may be higher".to_string(),
        );
        UsageSource::MetricsApi
    } else if requested.cpu_millis > 0 || requested.memory_mb > 0 {
        notes.push(
            "metrics-server is not installed — sizing falls back to resource requests, which are typically inflated. Install metrics-server for measured-usage sizing".to_string(),
        );
        UsageSource::RequestsOnly
    } else {
        UsageSource::Unavailable
    };

    // -- Overprovisioning assessment ---------------------------------------
    let cpu_utilization_pct = actual_usage
        .as_ref()
        .and_then(|usage| pct(usage.cpu_millis, capacity.cpu_millis));
    let memory_utilization_pct = actual_usage
        .as_ref()
        .and_then(|usage| pct(usage.memory_mb, capacity.memory_mb));
    let cpu_requested_pct = pct(requested.cpu_millis, capacity.cpu_millis);
    let memory_requested_pct = pct(requested.memory_mb, capacity.memory_mb);
    let cpu_request_inflation_ratio = actual_usage
        .as_ref()
        .and_then(|usage| ratio(requested.cpu_millis, usage.cpu_millis));
    let memory_request_inflation_ratio = actual_usage
        .as_ref()
        .and_then(|usage| ratio(requested.memory_mb, usage.memory_mb));

    let (verdict, explanation) = match (cpu_utilization_pct, memory_utilization_pct) {
        (Some(cpu_pct), Some(mem_pct)) => {
            let effective = cpu_pct.max(mem_pct);
            let verdict = if effective < 25.0 {
                OverprovisioningVerdict::Severe
            } else if effective < 60.0 {
                OverprovisioningVerdict::Moderate
            } else {
                OverprovisioningVerdict::Reasonable
            };
            let explanation = format!(
                "Cluster capacity is {:.1} vCPU / {} MB across {} node(s); measured usage is {:.2} vCPU ({}%) and {} MB ({}%)",
                capacity.cpu_millis as f64 / 1000.0,
                capacity.memory_mb,
                capacity.node_count,
                actual_usage.as_ref().map(|u| u.cpu_millis).unwrap_or(0) as f64 / 1000.0,
                cpu_pct,
                actual_usage.as_ref().map(|u| u.memory_mb).unwrap_or(0),
                mem_pct,
            );
            (verdict, explanation)
        }
        _ => match (cpu_requested_pct, memory_requested_pct) {
            (Some(cpu_pct), Some(mem_pct)) => (
                OverprovisioningVerdict::Unknown,
                format!(
                    "No live metrics — requests reserve {}% CPU and {}% memory of capacity; actual usage is typically far lower",
                    cpu_pct, mem_pct
                ),
            ),
            _ => (
                OverprovisioningVerdict::Unknown,
                "Could not assess: no metrics and no resource requests observed".to_string(),
            ),
        },
    };

    let overprovisioning = OverprovisioningAssessment {
        cpu_utilization_pct,
        memory_utilization_pct,
        cpu_requested_pct,
        memory_requested_pct,
        cpu_request_inflation_ratio,
        memory_request_inflation_ratio,
        verdict,
        explanation,
    };

    // -- Target recommendation ---------------------------------------------
    let (required_cpu, required_mem, sizing_basis) = match &actual_usage {
        Some(usage) => (
            TEMPS_OVERHEAD_CPU_MILLIS + (usage.cpu_millis as f64 * USAGE_HEADROOM) as i64,
            TEMPS_OVERHEAD_MEMORY_MB + (usage.memory_mb as f64 * USAGE_HEADROOM) as i64,
            format!(
                "{}× measured usage + temps platform overhead",
                USAGE_HEADROOM
            ),
        ),
        None => (
            TEMPS_OVERHEAD_CPU_MILLIS + requested.cpu_millis,
            TEMPS_OVERHEAD_MEMORY_MB + requested.memory_mb,
            "resource requests + temps platform overhead (no metrics available — likely oversized)"
                .to_string(),
        ),
    };

    let fitting = HETZNER_LADDER.iter().find(|(_, vcpus, memory_gb, _)| {
        (*vcpus as i64) * 1000 >= required_cpu && (*memory_gb as i64) * 1024 >= required_mem
    });
    let (server_type, vcpus, memory_gb, monthly_eur, fits_single_node) = match fitting {
        Some((name, vcpus, memory_gb, price)) => (*name, *vcpus, *memory_gb, *price, true),
        None => {
            let largest = HETZNER_LADDER[HETZNER_LADDER.len() - 1];
            (largest.0, largest.1, largest.2, largest.3, false)
        }
    };

    let monthly_savings_usd = current_monthly_usd
        .map(|current| ((current - monthly_eur) * 100.0).round() / 100.0)
        .filter(|savings| *savings > 0.0);
    let yearly_savings_usd =
        monthly_savings_usd.map(|monthly| (monthly * 12.0 * 100.0).round() / 100.0);

    let rationale = if fits_single_node {
        match monthly_savings_usd {
            Some(savings) => format!(
                "Your workloads fit a single Hetzner {} ({} vCPU / {} GB) at ~€{:.2}/mo running temps — about ${:.0}/mo less than the current cluster",
                server_type, vcpus, memory_gb, monthly_eur, savings
            ),
            None => format!(
                "Your workloads fit a single Hetzner {} ({} vCPU / {} GB) at ~€{:.2}/mo running temps",
                server_type, vcpus, memory_gb, monthly_eur
            ),
        }
    } else {
        format!(
            "The workloads need more than a single {} ({} vCPU / {} GB) — temps supports multi-node setups with worker servers; size roughly one cpx-line server per {} vCPU of sustained usage",
            server_type, vcpus, memory_gb, vcpus
        )
    };

    if HETZNER_LADDER
        .iter()
        .any(|(name, _, _, _)| *name == server_type)
        && server_type != "cpx22"
    {
        notes.push(
            "Hetzner prices above cpx22 are linear extrapolations of the verified cpx22 monthly cap".to_string(),
        );
    }

    let recommendation = TargetRecommendation {
        server_type: server_type.to_string(),
        vcpus,
        memory_gb,
        monthly_eur,
        monthly_savings_usd,
        yearly_savings_usd,
        fits_single_node,
        sizing_basis,
        rationale,
    };

    CostAnalysis {
        provider,
        nodes,
        current_monthly_usd,
        control_plane_monthly_usd,
        capacity,
        requested,
        actual_usage,
        usage_source,
        overprovisioning,
        recommendation,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ObservedNode;

    fn node(
        name: &str,
        instance_type: Option<&str>,
        cpu_millis: i64,
        memory_mb: i64,
    ) -> ObservedNode {
        ObservedNode {
            name: name.to_string(),
            instance_type: instance_type.map(|s| s.to_string()),
            region: Some("us-east-1".to_string()),
            provider_id: Some("aws:///us-east-1a/i-abc123".to_string()),
            is_eks: true,
            is_gke: false,
            is_ask: false,
            allocatable_cpu_millis: cpu_millis,
            allocatable_memory_mb: memory_mb,
            capacity_cpu_millis: cpu_millis,
            capacity_memory_mb: memory_mb,
        }
    }

    #[test]
    fn test_severely_overprovisioned_eks_cluster() {
        // 3× m5.xlarge (4 vCPU / 16 GB each) idling at ~0.3 vCPU total
        let observation = ClusterObservation {
            nodes: vec![
                node("n1", Some("m5.xlarge"), 4000, 16384),
                node("n2", Some("m5.xlarge"), 4000, 16384),
                node("n3", Some("m5.xlarge"), 4000, 16384),
            ],
            requested_cpu_millis: 6000,
            requested_memory_mb: 12288,
            actual_cpu_millis: Some(300),
            actual_memory_mb: Some(2048),
            running_pods: 25,
        };

        let analysis = compute_cost_analysis(&observation);

        // 3 × $140 + $73 EKS fee
        assert_eq!(analysis.current_monthly_usd, Some(493.0));
        assert_eq!(analysis.control_plane_monthly_usd, Some(73.0));
        assert_eq!(analysis.provider, Some(CloudProvider::Aws));
        assert_eq!(
            analysis.overprovisioning.verdict,
            OverprovisioningVerdict::Severe
        );
        // usage 300m / 12000m = 2.5%
        assert_eq!(analysis.overprovisioning.cpu_utilization_pct, Some(2.5));
        // requests are 20x actual cpu
        assert_eq!(
            analysis.overprovisioning.cpu_request_inflation_ratio,
            Some(20.0)
        );
        // required: 500 + 600 = 1100m cpu, 1536 + 4096 = 5632 MB → cpx32 (4 vCPU / 8 GB)
        assert_eq!(analysis.recommendation.server_type, "cpx32");
        assert!(analysis.recommendation.fits_single_node);
        let savings = analysis.recommendation.monthly_savings_usd.unwrap();
        assert!(savings > 400.0, "savings was {}", savings);
        assert_eq!(analysis.usage_source, UsageSource::MetricsApi);
    }

    #[test]
    fn test_no_metrics_falls_back_to_requests() {
        let mut n = node("n1", Some("cpx22"), 2000, 3840);
        n.provider_id = Some("hcloud://12345".to_string());
        n.is_eks = false;
        let observation = ClusterObservation {
            nodes: vec![n],
            requested_cpu_millis: 1500,
            requested_memory_mb: 1536,
            actual_cpu_millis: None,
            actual_memory_mb: None,
            running_pods: 8,
        };

        let analysis = compute_cost_analysis(&observation);

        assert_eq!(analysis.usage_source, UsageSource::RequestsOnly);
        assert_eq!(analysis.provider, Some(CloudProvider::Hetzner));
        assert_eq!(analysis.control_plane_monthly_usd, None);
        assert_eq!(
            analysis.overprovisioning.verdict,
            OverprovisioningVerdict::Unknown
        );
        // required: 500+1500 = 2000m, 1536+1536 = 3072 MB → cpx32 (cpx22 memory
        // fits but cpu is exactly 2000 = 2 vCPU... 2*1000 >= 2000 → cpx22 fits)
        assert_eq!(analysis.recommendation.server_type, "cpx22");
        assert!(analysis
            .notes
            .iter()
            .any(|note| note.contains("metrics-server is not installed")));
    }

    #[test]
    fn test_unknown_instance_types_are_not_guessed() {
        let mut n = node("bare-metal-1", None, 16000, 65536);
        n.provider_id = None;
        n.is_eks = false;
        let observation = ClusterObservation {
            nodes: vec![n],
            requested_cpu_millis: 2000,
            requested_memory_mb: 4096,
            actual_cpu_millis: Some(900),
            actual_memory_mb: Some(3000),
            running_pods: 12,
        };

        let analysis = compute_cost_analysis(&observation);

        assert_eq!(analysis.current_monthly_usd, None);
        assert_eq!(analysis.provider, None);
        assert!(analysis.recommendation.monthly_savings_usd.is_none());
        assert!(analysis
            .notes
            .iter()
            .any(|note| note.contains("No node instance type could be priced")));
        // Sizing still works from usage: 500+1800 = 2300m, 1536+6000 = 7536 MB → cpx32
        assert_eq!(analysis.recommendation.server_type, "cpx32");
    }

    #[test]
    fn test_workload_too_big_for_single_node() {
        let observation = ClusterObservation {
            nodes: vec![node("n1", Some("m5.2xlarge"), 8000, 32768)],
            requested_cpu_millis: 30000,
            requested_memory_mb: 60000,
            actual_cpu_millis: Some(20000),
            actual_memory_mb: Some(50000),
            running_pods: 200,
        };

        let analysis = compute_cost_analysis(&observation);
        assert!(!analysis.recommendation.fits_single_node);
        assert!(analysis.recommendation.rationale.contains("multi-node"));
    }
}
