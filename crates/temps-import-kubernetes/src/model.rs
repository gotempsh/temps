// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal Kubernetes API object model
//!
//! Hand-rolled serde structs for exactly the fields the importer reads —
//! deliberately NOT `k8s-openapi`/`kube-rs`, which would add a very heavy
//! dependency for what is a read-only subset of ~10 resource kinds.
//! Unknown fields are ignored by serde's default behaviour, so this stays
//! compatible across Kubernetes versions.

use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Common
// ---------------------------------------------------------------------------

/// Generic Kubernetes list response
#[derive(Debug, Clone, Deserialize)]
pub struct List<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub metadata: ListMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListMeta {
    /// Pagination continue token
    #[serde(rename = "continue")]
    pub continue_token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMeta {
    #[serde(default)]
    pub name: String,
    pub namespace: Option<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub annotations: HashMap<String, String>,
    pub creation_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub owner_references: Vec<OwnerReference>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerReference {
    pub kind: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Workloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Deployment {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<DeploymentSpec>,
    pub status: Option<WorkloadStatusInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSpec {
    pub replicas: Option<i32>,
    pub template: PodTemplateSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatefulSet {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<StatefulSetSpec>,
    pub status: Option<WorkloadStatusInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSetSpec {
    pub replicas: Option<i32>,
    pub template: PodTemplateSpec,
    #[serde(default)]
    pub volume_claim_templates: Vec<PersistentVolumeClaim>,
}

/// Shared status shape for Deployments and StatefulSets (subset)
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadStatusInfo {
    pub ready_replicas: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CronJob {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<CronJobSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobSpec {
    pub schedule: String,
    pub job_template: JobTemplateSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobTemplateSpec {
    pub spec: Option<JobSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobSpec {
    pub template: PodTemplateSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PodTemplateSpec {
    pub metadata: Option<ObjectMeta>,
    pub spec: Option<PodSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSpec {
    #[serde(default)]
    pub containers: Vec<Container>,
    #[serde(default)]
    pub volumes: Vec<Volume>,
    #[serde(default)]
    pub host_network: bool,
    pub node_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name: String,
    pub image: Option<String>,
    pub command: Option<Vec<String>>,
    pub args: Option<Vec<String>>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub ports: Vec<ContainerPort>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub env_from: Vec<EnvFromSource>,
    #[serde(default)]
    pub resources: ResourceRequirements,
    #[serde(default)]
    pub volume_mounts: Vec<ContainerVolumeMount>,
    pub readiness_probe: Option<Probe>,
    pub liveness_probe: Option<Probe>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerPort {
    pub container_port: i32,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: Option<String>,
    pub value_from: Option<EnvVarSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarSource {
    pub config_map_key_ref: Option<KeySelector>,
    pub secret_key_ref: Option<KeySelector>,
    pub field_ref: Option<serde_json::Value>,
    pub resource_field_ref: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeySelector {
    pub name: Option<String>,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFromSource {
    pub prefix: Option<String>,
    pub config_map_ref: Option<NameRef>,
    pub secret_ref: Option<NameRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NameRef {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResourceRequirements {
    #[serde(default)]
    pub requests: HashMap<String, String>,
    #[serde(default)]
    pub limits: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerVolumeMount {
    pub name: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub name: String,
    pub persistent_volume_claim: Option<PvcVolumeSource>,
    pub host_path: Option<HostPathVolumeSource>,
    pub empty_dir: Option<EmptyDirVolumeSource>,
    pub config_map: Option<serde_json::Value>,
    pub secret: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PvcVolumeSource {
    pub claim_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostPathVolumeSource {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmptyDirVolumeSource {
    pub medium: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersistentVolumeClaim {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub http_get: Option<HttpGetAction>,
    pub initial_delay_seconds: Option<u32>,
    pub period_seconds: Option<u32>,
    pub timeout_seconds: Option<u32>,
    pub failure_threshold: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpGetAction {
    pub path: Option<String>,
    /// int-or-string in the API
    pub port: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Networking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Service {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<ServiceSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(default)]
    pub selector: HashMap<String, String>,
    #[serde(default)]
    pub ports: Vec<ServicePort>,
    #[serde(rename = "type")]
    pub service_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    pub port: i32,
    pub target_port: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ingress {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<IngressSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngressSpec {
    #[serde(default)]
    pub rules: Vec<IngressRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngressRule {
    pub host: Option<String>,
    pub http: Option<HttpIngressRuleValue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpIngressRuleValue {
    #[serde(default)]
    pub paths: Vec<HttpIngressPath>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpIngressPath {
    pub backend: IngressBackend,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngressBackend {
    pub service: Option<IngressServiceBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngressServiceBackend {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigMap {
    #[serde(default)]
    pub metadata: ObjectMeta,
    #[serde(default)]
    pub data: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Secret {
    #[serde(default)]
    pub metadata: ObjectMeta,
    /// Values are base64-encoded — the importer only needs the KEYS
    #[serde(default)]
    pub data: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Namespace {
    #[serde(default)]
    pub metadata: ObjectMeta,
}

// ---------------------------------------------------------------------------
// Cluster / metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<NodeSpec>,
    pub status: Option<NodeStatus>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSpec {
    #[serde(rename = "providerID")]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub allocatable: HashMap<String, String>,
    #[serde(default)]
    pub capacity: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pod {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<PodSpec>,
    pub status: Option<PodStatus>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PodStatus {
    pub phase: Option<String>,
}

/// `metrics.k8s.io/v1beta1` NodeMetrics item
#[derive(Debug, Clone, Deserialize)]
pub struct NodeMetrics {
    #[serde(default)]
    pub metadata: ObjectMeta,
    #[serde(default)]
    pub usage: HashMap<String, String>,
}

/// `autoscaling/v2` HorizontalPodAutoscaler (subset)
#[derive(Debug, Clone, Deserialize)]
pub struct HorizontalPodAutoscaler {
    #[serde(default)]
    pub metadata: ObjectMeta,
    pub spec: Option<HpaSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HpaSpec {
    pub scale_target_ref: ScaleTargetRef,
    pub min_replicas: Option<i32>,
    pub max_replicas: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScaleTargetRef {
    pub kind: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Quantity parsing
// ---------------------------------------------------------------------------

/// Parse a Kubernetes CPU quantity into millicores.
///
/// Accepts: `"100m"` → 100, `"1"` → 1000, `"2.5"` → 2500, and the
/// nano/micro forms the metrics API emits (`"12345678n"`, `"250u"`).
/// Returns `None` for unparsable input — callers treat that as "unknown".
pub fn parse_cpu_millis(quantity: &str) -> Option<i64> {
    let q = quantity.trim();
    if q.is_empty() {
        return None;
    }
    let (num, factor): (&str, f64) = if let Some(stripped) = q.strip_suffix('n') {
        (stripped, 1e-6) // nanocores → millicores
    } else if let Some(stripped) = q.strip_suffix('u') {
        (stripped, 1e-3) // microcores → millicores
    } else if let Some(stripped) = q.strip_suffix('m') {
        (stripped, 1.0) // millicores
    } else {
        (q, 1000.0) // cores → millicores
    };
    let value: f64 = num.parse().ok()?;
    Some((value * factor).round() as i64)
}

/// Parse a Kubernetes memory quantity into bytes.
///
/// Accepts binary suffixes (`Ki`, `Mi`, `Gi`, `Ti`, `Pi`), decimal suffixes
/// (`k`, `M`, `G`, `T`, `P`), and plain byte counts. Returns `None` for
/// unparsable input.
pub fn parse_memory_bytes(quantity: &str) -> Option<i64> {
    let q = quantity.trim();
    if q.is_empty() {
        return None;
    }
    const BINARY: [(&str, f64); 5] = [
        ("Ki", 1024.0),
        ("Mi", 1024.0 * 1024.0),
        ("Gi", 1024.0 * 1024.0 * 1024.0),
        ("Ti", 1024.0 * 1024.0 * 1024.0 * 1024.0),
        ("Pi", 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0),
    ];
    const DECIMAL: [(&str, f64); 5] =
        [("k", 1e3), ("M", 1e6), ("G", 1e9), ("T", 1e12), ("P", 1e15)];
    for (suffix, factor) in BINARY {
        if let Some(stripped) = q.strip_suffix(suffix) {
            let value: f64 = stripped.parse().ok()?;
            return Some((value * factor).round() as i64);
        }
    }
    for (suffix, factor) in DECIMAL {
        if let Some(stripped) = q.strip_suffix(suffix) {
            let value: f64 = stripped.parse().ok()?;
            return Some((value * factor).round() as i64);
        }
    }
    // Plain number (possibly scientific notation like "1e9")
    let value: f64 = q.parse().ok()?;
    Some(value.round() as i64)
}

/// Convert bytes to MB (MiB divisor, consistent with the Docker importer)
pub fn bytes_to_mb(bytes: i64) -> i64 {
    bytes / 1_048_576
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cpu_millis() {
        assert_eq!(parse_cpu_millis("100m"), Some(100));
        assert_eq!(parse_cpu_millis("1"), Some(1000));
        assert_eq!(parse_cpu_millis("2.5"), Some(2500));
        assert_eq!(parse_cpu_millis("0"), Some(0));
        // metrics-server nanocores: 12345678n ≈ 12.3m
        assert_eq!(parse_cpu_millis("12345678n"), Some(12));
        assert_eq!(parse_cpu_millis("250000u"), Some(250));
        assert_eq!(parse_cpu_millis(""), None);
        assert_eq!(parse_cpu_millis("garbage"), None);
    }

    #[test]
    fn test_parse_memory_bytes() {
        assert_eq!(parse_memory_bytes("128Mi"), Some(128 * 1024 * 1024));
        assert_eq!(parse_memory_bytes("1Gi"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory_bytes("512Ki"), Some(512 * 1024));
        assert_eq!(parse_memory_bytes("1G"), Some(1_000_000_000));
        assert_eq!(parse_memory_bytes("500M"), Some(500_000_000));
        assert_eq!(parse_memory_bytes("1024"), Some(1024));
        assert_eq!(parse_memory_bytes("1e9"), Some(1_000_000_000));
        assert_eq!(parse_memory_bytes("nope"), None);
    }

    #[test]
    fn test_deserialize_deployment_subset() {
        let json = serde_json::json!({
            "metadata": {
                "name": "storefront",
                "namespace": "shop",
                "labels": {"app": "storefront"},
                "creationTimestamp": "2026-07-25T09:00:00Z"
            },
            "spec": {
                "replicas": 2,
                "template": {
                    "metadata": {"labels": {"app": "storefront"}},
                    "spec": {
                        "containers": [{
                            "name": "storefront",
                            "image": "traefik/whoami:latest",
                            "ports": [{"containerPort": 80}],
                            "env": [
                                {"name": "PLAN", "value": "pro"},
                                {"name": "SECRET", "valueFrom": {"secretKeyRef": {"name": "s", "key": "SECRET"}}}
                            ],
                            "resources": {
                                "requests": {"cpu": "800m", "memory": "768Mi"},
                                "limits": {"cpu": "1", "memory": "1Gi"}
                            }
                        }]
                    }
                }
            },
            "status": {"readyReplicas": 2, "unknownField": true}
        });
        let d: Deployment = serde_json::from_value(json).unwrap();
        assert_eq!(d.metadata.name, "storefront");
        assert_eq!(d.spec.as_ref().unwrap().replicas, Some(2));
        let container = &d
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0];
        assert_eq!(container.image.as_deref(), Some("traefik/whoami:latest"));
        assert_eq!(
            parse_cpu_millis(&container.resources.requests["cpu"]),
            Some(800)
        );
        assert!(container.env[1]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .is_some());
    }
}
