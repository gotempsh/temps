// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workload spec (ADR-055 §3): what a product asks the plane to run.
//!
//! A spec is stored (`workloads.spec`) and sent to nodes, so it never holds
//! a secret value: secrets are [`SecretRef`]s, resolved only into the
//! transient [`crate::InstanceSpec`] a driver receives.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Resources, WorkloadRef};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub owner: WorkloadRef,
    pub artifact: Artifact,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub process: Process,
    #[serde(default)]
    pub env: EnvSpec,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub network: NetworkSpec,
    #[serde(default)]
    pub probes: Probes,
    #[serde(default)]
    pub placement: PlacementSpec,
    pub lifetime: Lifetime,
    /// Set by admission (§4), never by a product. A product that sets it is
    /// overruled: admission chooses the profile from the kind and the
    /// Docker socket grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityProfile>,
    /// Labels a product adds for its own discovery (`sh.temps.project_id`
    /// and friends). The plane adds its own (`crate::labels`).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// What the instance runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Artifact {
    OciImage {
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    WasmComponent {
        blob: String,
        digest: String,
    },
    Rootfs {
        digest: String,
    },
}

impl Artifact {
    pub fn oci(reference: impl Into<String>) -> Self {
        Artifact::OciImage {
            reference: reference.into(),
            digest: None,
        }
    }
}

/// What contains the code. Named for the boundary, not the technology.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Inside the node runtime process (WASM). An escape lands in the agent.
    InHost,
    /// A container sharing the host kernel.
    #[default]
    Process,
    /// A microVM with its own kernel.
    MicroVm,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    /// Replaces the image's command. `None` keeps the image's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Replaces the image's entrypoint. `None` keeps the image's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// How variables reach the process (ADR-052 §3). A property of the kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Set at creation; secrets as read-only files under `/run/secrets`.
    #[default]
    Baked,
    /// Resolved for every command and never stored in the instance.
    PerExec,
}

/// A secret, by name. Its value is resolved by the plane at create time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SecretRef {
    /// The variable or file name the workload sees.
    pub name: String,
    /// Where the value is stored, e.g. `env_var:12` (an encrypted column).
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSpec {
    /// Non-secret variables, in their final (assembled) values.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Secrets, delivered per `delivery`.
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    #[serde(default)]
    pub delivery: Delivery,
    /// Sources whose every variable the workload receives, resolved only at
    /// create time like `secrets`: a job imports the environment of the
    /// deployment it runs beside (`deployment_container:<id>`). Lower
    /// precedence than `secrets` and `vars`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    /// The `Volume` resource's name (`temps-workload-volumes`).
    pub volume: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl PortProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            PortProtocol::Tcp => "tcp",
            PortProtocol::Udp => "udp",
        }
    }
}

/// What speaks on a port; decides the DNS record mode (§10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppProtocol {
    #[default]
    Http,
    Tcp,
    Udp,
}

/// A named port. Never a host port: the plane allocates host bindings
/// where a driver needs them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortSpec {
    pub name: String,
    pub container_port: u16,
    #[serde(default)]
    pub protocol: PortProtocol,
    #[serde(default)]
    pub app_protocol: AppProtocol,
}

/// What a workload may reach (§9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressProfile {
    /// Anything: today's behaviour, the default for deployments.
    #[default]
    Open,
    /// Linked services, plus allowed CIDRs and hostnames.
    Restricted,
    /// Nothing.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSpec {
    /// `None` takes the kind's default (admission, §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressProfile>,
    /// Extra networks the instance must join before it starts (a
    /// self-hosted dependency's Compose network).
    #[serde(default)]
    pub extra_networks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProbeKind {
    Http {
        path: String,
        port: u16,
    },
    Tcp {
        port: u16,
    },
    /// The process is running.
    Process,
    Exec {
        command: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub kind: ProbeKind,
    #[serde(default = "Probe::default_period_secs")]
    pub period_secs: u32,
    #[serde(default = "Probe::default_timeout_secs")]
    pub timeout_secs: u32,
    #[serde(default = "Probe::default_failure_threshold")]
    pub failure_threshold: u32,
}

impl Probe {
    fn default_period_secs() -> u32 {
        10
    }
    fn default_timeout_secs() -> u32 {
        5
    }
    fn default_failure_threshold() -> u32 {
        3
    }
}

/// Startup, readiness, liveness (§13). Liveness has no default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<Probe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Probe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Probe>,
}

/// The value a [`LabelSelector`] key requires: one value, or any of a set
/// (the `target_labels` array form, an OR).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LabelValues {
    One(String),
    AnyOf(Vec<String>),
}

impl LabelValues {
    /// Whether a node's label value satisfies this requirement.
    pub fn matches(&self, value: &str) -> bool {
        match self {
            LabelValues::One(required) => required == value,
            LabelValues::AnyOf(required) => required.iter().any(|r| r == value),
        }
    }
}

/// Node label requirements: every key must match (AND), and a key with
/// several values matches any of them (OR). The `target_labels` semantics.
pub type LabelSelector = BTreeMap<String, LabelValues>;

/// Where instances may run (§8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementSpec {
    /// Required node set; `None` = any eligible node. Strict: when no node
    /// of the set is eligible the workload is not scheduled anywhere else.
    /// Node `0` is the control plane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<i32>>,
    /// Stacks with `nodes`, with `target_labels` semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<LabelSelector>,
    /// Replicas on different nodes.
    #[serde(default)]
    pub spread: bool,
    #[serde(default = "PlacementSpec::one")]
    pub replicas: u32,
}

impl PlacementSpec {
    fn one() -> u32 {
        1
    }

    /// The pinned node set, if any. An empty list names no node, so it is
    /// no constraint (`target_nodes: []`).
    pub fn pinned_nodes(&self) -> Option<&[i32]> {
        self.nodes.as_deref().filter(|nodes| !nodes.is_empty())
    }

    /// The label selector, if it requires anything.
    pub fn label_selector(&self) -> Option<&LabelSelector> {
        self.labels.as_ref().filter(|labels| !labels.is_empty())
    }

    /// Whether a node with `node_labels` satisfies the label selector.
    pub fn labels_match(&self, node_labels: &BTreeMap<String, String>) -> bool {
        self.label_selector().is_none_or(|selector| {
            selector.iter().all(|(key, required)| {
                node_labels
                    .get(key)
                    .is_some_and(|value| required.matches(value))
            })
        })
    }
}

impl Default for PlacementSpec {
    fn default() -> Self {
        Self {
            nodes: None,
            labels: None,
            spread: false,
            replicas: 1,
        }
    }
}

/// The driver-level restart policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    Always,
    #[default]
    OnFailure,
    UnlessStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Lifetime {
    LongRunning {
        #[serde(default)]
        restart: RestartPolicy,
    },
    /// Runs until the process exits (a job, a cron run). Kubernetes Job
    /// semantics: the run is killed and failed `timeout_secs` after it was
    /// created (`activeDeadlineSeconds`; `0` takes the kind's default), a
    /// failed attempt is retried up to `retry_limit` more times
    /// (`backoffLimit`), and a finished run is removed
    /// `ttl_secs_after_finished` after it finished (`None` takes the kind's
    /// default).
    RunToCompletion {
        timeout_secs: u32,
        #[serde(default)]
        retry_limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_secs_after_finished: Option<u32>,
    },
    /// Stopped after `idle_secs` without traffic, woken by the activator.
    ScaleToZero { idle_secs: u32 },
}

/// Pod-Security-style profiles (§4), chosen by admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityProfile {
    /// ADR-045 granted projects only: the host Docker socket is mounted.
    HostPrivileged,
    /// Deployments: today's hardened host config.
    Trusted,
    /// Databases: `Trusted`, plus a fixed restart policy, an engine
    /// healthcheck and a recreate guard; the pids cap is per engine.
    Data,
    /// Sandboxes, functions: `Trusted`, plus egress at most `restricted`,
    /// no `Administrator` credentials, and safety caps.
    Untrusted,
}

impl SecurityProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            SecurityProfile::HostPrivileged => "host_privileged",
            SecurityProfile::Trusted => "trusted",
            SecurityProfile::Data => "data",
            SecurityProfile::Untrusted => "untrusted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkloadKind;

    fn job_spec() -> WorkloadSpec {
        WorkloadSpec {
            owner: WorkloadRef::new(WorkloadKind::Job, 3),
            artifact: Artifact::oci("registry.example.test/app:1"),
            isolation: Isolation::Process,
            process: Process {
                command: Some(vec!["bin/cleanup".into(), "--all".into()]),
                ..Default::default()
            },
            env: EnvSpec {
                vars: BTreeMap::from([("MODE".into(), "batch".into())]),
                secrets: vec![SecretRef {
                    name: "API_TOKEN".into(),
                    source: "env_var:12".into(),
                }],
                delivery: Delivery::Baked,
                imports: vec![],
            },
            resources: Resources::default(),
            volumes: vec![],
            ports: vec![],
            network: NetworkSpec::default(),
            probes: Probes::default(),
            placement: PlacementSpec::default(),
            lifetime: Lifetime::RunToCompletion {
                timeout_secs: 600,
                retry_limit: 2,
                ttl_secs_after_finished: Some(300),
            },
            security: None,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = job_spec();
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["lifetime"]["type"], "run_to_completion");
        assert_eq!(json["artifact"]["type"], "oci_image");
        assert_eq!(json["owner"]["kind"], "job");
        let back: WorkloadSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn a_run_to_completion_lifetime_stored_before_retries_takes_no_retries() {
        let lifetime: Lifetime = serde_json::from_value(serde_json::json!({
            "type": "run_to_completion", "timeout_secs": 60
        }))
        .unwrap();
        assert_eq!(
            lifetime,
            Lifetime::RunToCompletion {
                timeout_secs: 60,
                retry_limit: 0,
                ttl_secs_after_finished: None
            }
        );
    }

    #[test]
    fn a_minimal_stored_spec_takes_the_defaults() {
        let spec: WorkloadSpec = serde_json::from_value(serde_json::json!({
            "owner": { "kind": "deployment", "id": 1 },
            "artifact": { "type": "oci_image", "reference": "nginx:1" },
            "lifetime": { "type": "long_running" }
        }))
        .unwrap();
        assert_eq!(spec.isolation, Isolation::Process);
        assert_eq!(spec.placement.replicas, 1);
        assert_eq!(
            spec.lifetime,
            Lifetime::LongRunning {
                restart: RestartPolicy::OnFailure
            }
        );
        assert_eq!(spec.network.egress, None);
        assert!(spec.security.is_none());
    }

    #[test]
    fn a_label_selector_keeps_the_target_labels_or_and_and_semantics() {
        let placement: PlacementSpec = serde_json::from_value(serde_json::json!({
            "labels": { "region": ["us", "asia"], "gpu": "true" }
        }))
        .unwrap();
        let node = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert!(placement.labels_match(&node(&[("region", "asia"), ("gpu", "true")])));
        assert!(!placement.labels_match(&node(&[("region", "eu"), ("gpu", "true")])));
        assert!(!placement.labels_match(&node(&[("region", "us")])));
        assert!(PlacementSpec::default().labels_match(&BTreeMap::new()));
        let empty = PlacementSpec {
            nodes: Some(vec![]),
            labels: Some(BTreeMap::new()),
            ..Default::default()
        };
        assert_eq!(empty.pinned_nodes(), None, "an empty list pins nothing");
        assert!(empty.label_selector().is_none());
        let back: PlacementSpec =
            serde_json::from_value(serde_json::to_value(&placement).unwrap()).unwrap();
        assert_eq!(back, placement);
    }

    #[test]
    fn a_spec_never_carries_a_secret_value() {
        // Only the reference is serialised; the snapshot a worker keeps on
        // disk (§7) is a list of specs.
        let json = serde_json::to_string(&job_spec()).unwrap();
        assert!(json.contains("\"source\":\"env_var:12\""));
        assert!(!json.contains("value"));
    }
}
