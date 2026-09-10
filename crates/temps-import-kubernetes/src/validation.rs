// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-flight validation rules for Kubernetes imports

use temps_import_types::{
    ImportPlan, ImportValidationRule, NetworkMode, ValidationLevel, ValidationResult,
    WorkloadSnapshot,
};

/// Registries whose images are pullable without credentials (well-known public)
const PUBLIC_REGISTRIES: &[&str] = &[
    "docker.io",
    "ghcr.io",
    "quay.io",
    "gcr.io",
    "registry.k8s.io",
    "public.ecr.aws",
    "mcr.microsoft.com",
];

pub struct KubernetesValidationRules;

impl KubernetesValidationRules {
    pub fn all_rules() -> Vec<Box<dyn ImportValidationRule>> {
        vec![
            Box::new(ImagePresentRule),
            Box::new(PrivateRegistryRule),
            Box::new(MultiReplicaRule),
            Box::new(SidecarRule),
            Box::new(HostNetworkRule),
        ]
    }
}

fn metadata_u64(snapshot: &WorkloadSnapshot, key: &str) -> u64 {
    snapshot
        .source_metadata
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

fn metadata_str_list(snapshot: &WorkloadSnapshot, key: &str) -> Vec<String> {
    snapshot
        .source_metadata
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The workload must reference a container image — temps deploys images.
struct ImagePresentRule;

impl ImportValidationRule for ImagePresentRule {
    fn rule_id(&self) -> &str {
        "k8s-image-present"
    }
    fn rule_name(&self) -> &str {
        "Container image present"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Critical
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let passed = snapshot.image.as_deref().is_some_and(|i| !i.is_empty());
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                format!(
                    "Workload references image '{}'",
                    snapshot.image.as_deref().unwrap_or_default()
                )
            } else {
                format!(
                    "Workload '{}' has no container image — cannot import",
                    snapshot.id
                )
            },
            remediation: (!passed)
                .then(|| "Ensure the pod template's first container has an image".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Images from private registries need registry credentials configured in temps.
struct PrivateRegistryRule;

impl ImportValidationRule for PrivateRegistryRule {
    fn rule_id(&self) -> &str {
        "k8s-private-registry"
    }
    fn rule_name(&self) -> &str {
        "Image registry accessibility"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let image = snapshot.image.as_deref().unwrap_or_default();
        // A registry host is present when the first path segment contains a
        // dot or colon ("registry.example.com/app" vs "nginx" / "library/nginx")
        let registry = image
            .split('/')
            .next()
            .filter(|first| (first.contains('.') || first.contains(':')) && image.contains('/'));
        let is_private = registry
            .map(|reg| {
                let host = reg.split(':').next().unwrap_or(reg);
                !PUBLIC_REGISTRIES.contains(&host)
            })
            .unwrap_or(false);
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: !is_private,
            message: if is_private {
                format!(
                    "Image '{}' comes from a private registry '{}' — temps needs pull access",
                    image,
                    registry.unwrap_or_default()
                )
            } else {
                "Image comes from a public registry".to_string()
            },
            remediation: is_private.then(|| {
                "Configure the registry credentials in temps before deploying, or push the image to a registry temps can reach".to_string()
            }),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Multi-replica workloads collapse to a single container per temps environment.
struct MultiReplicaRule;

impl ImportValidationRule for MultiReplicaRule {
    fn rule_id(&self) -> &str {
        "k8s-multi-replica"
    }
    fn rule_name(&self) -> &str {
        "Replica count"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let replicas = metadata_u64(snapshot, "replicas");
        let passed = replicas <= 1;
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                "Single-replica workload".to_string()
            } else {
                format!(
                    "Workload runs {} replicas — temps deploys one container per environment",
                    replicas
                )
            },
            remediation: (!passed).then(|| {
                "One container usually suffices once requests match real usage; for horizontal scale, temps supports worker nodes".to_string()
            }),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// Sidecar containers (service mesh, log shippers) are not imported.
struct SidecarRule;

impl ImportValidationRule for SidecarRule {
    fn rule_id(&self) -> &str {
        "k8s-sidecars"
    }
    fn rule_name(&self) -> &str {
        "Sidecar containers"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let sidecars = metadata_str_list(snapshot, "sidecar_containers");
        let passed = sidecars.is_empty();
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed,
            message: if passed {
                "No sidecar containers".to_string()
            } else {
                format!(
                    "Pod has {} extra container(s) that will NOT be imported: {}",
                    sidecars.len(),
                    sidecars.join(", ")
                )
            },
            remediation: (!passed).then(|| {
                "Only the primary container is imported. Service-mesh sidecars are unnecessary on temps; other sidecars may need separate deployments".to_string()
            }),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}

/// hostNetwork pods depend on node networking that temps handles differently.
struct HostNetworkRule;

impl ImportValidationRule for HostNetworkRule {
    fn rule_id(&self) -> &str {
        "k8s-host-network"
    }
    fn rule_name(&self) -> &str {
        "Host networking"
    }
    fn level(&self) -> ValidationLevel {
        ValidationLevel::Warning
    }
    fn validate(&self, snapshot: &WorkloadSnapshot, _plan: &ImportPlan) -> ValidationResult {
        let uses_host = matches!(snapshot.network.mode, NetworkMode::Host);
        ValidationResult {
            rule_id: self.rule_id().to_string(),
            rule_name: self.rule_name().to_string(),
            level: self.level(),
            passed: !uses_host,
            message: if uses_host {
                "Pod uses hostNetwork — port collisions possible on the temps host".to_string()
            } else {
                "Standard pod networking".to_string()
            },
            remediation: uses_host
                .then(|| "Verify the workload works behind the temps proxy instead".to_string()),
            affected_resources: vec![snapshot.id.to_string()],
        }
    }
}
