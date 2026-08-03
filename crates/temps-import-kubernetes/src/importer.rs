//! Kubernetes workload importer implementation

use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tracing::{debug, info, warn};

use temps_import_types::{
    CredentialValidation, DataImplication, DataImplicationSeverity, DomainAction, DomainPlan,
    DomainSnapshot, ImportContext, ImportCredentials, ImportError, ImportOutcome, ImportPlan,
    ImportResult, ImportSelector, ImportServiceProvider, ImportSource, ImporterCapabilities,
    ManualAction, ManualActionTiming, NetworkInfo, NetworkMode, ProjectSnapshot, ResourceInfo,
    RestartPolicy, ServiceAction, ServicePlan, ServiceSnapshot, SnapshotServiceType, StepResult,
    UnsupportedFeature, VolumeMount as SnapshotVolumeMount, VolumeType, WorkloadDescriptor,
    WorkloadId, WorkloadImporter, WorkloadSnapshot, WorkloadStatus, WorkloadType,
};

use crate::client::KubeClient;
use crate::cluster::{observe_cluster, ClusterObservation, CLUSTER_OBSERVATION_KEY};
use crate::cost::compute_cost_analysis;
use crate::error::KubernetesImportError;
use crate::kubeconfig::{resolve_kubeconfig, ResolvedKubeconfig};
use crate::model::{
    bytes_to_mb, parse_cpu_millis, parse_memory_bytes, ConfigMap, Container, CronJob, Deployment,
    HorizontalPodAutoscaler, Ingress, PodTemplateSpec, Secret, StatefulSet,
};
use crate::validation::KubernetesValidationRules;

/// Namespaces that are cluster plumbing, not user workloads
const SYSTEM_NAMESPACES: &[&str] = &[
    "kube-system",
    "kube-public",
    "kube-node-lease",
    "kube-flannel",
    "local-path-storage",
];

/// Container names that are sidecars, not the primary workload
const SIDECAR_CONTAINERS: &[&str] = &[
    "istio-proxy",
    "linkerd-proxy",
    "envoy-sidecar",
    "vault-agent",
    "cloud-sql-proxy",
];

/// Cap on additional workloads described per project snapshot
const MAX_ADDITIONAL_WORKLOADS: usize = 10;

// ---------------------------------------------------------------------------
// Workload reference: "<namespace>/<kind>/<name>"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkloadKind {
    Deployment,
    StatefulSet,
    CronJob,
}

impl WorkloadKind {
    fn as_str(&self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "deployment",
            WorkloadKind::StatefulSet => "statefulset",
            WorkloadKind::CronJob => "cronjob",
        }
    }
}

#[derive(Debug, Clone)]
struct WorkloadRef {
    namespace: String,
    kind: WorkloadKind,
    name: String,
}

impl WorkloadRef {
    fn parse(id: &WorkloadId) -> Result<Self, KubernetesImportError> {
        let parts: Vec<&str> = id.as_str().split('/').collect();
        let invalid = || KubernetesImportError::InvalidWorkloadId {
            id: id.as_str().to_string(),
        };
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(invalid());
        }
        let kind = match parts[1] {
            "deployment" => WorkloadKind::Deployment,
            "statefulset" => WorkloadKind::StatefulSet,
            "cronjob" => WorkloadKind::CronJob,
            _ => return Err(invalid()),
        };
        Ok(Self {
            namespace: parts[0].to_string(),
            kind,
            name: parts[2].to_string(),
        })
    }

    fn id(&self) -> WorkloadId {
        WorkloadId::new(format!(
            "{}/{}/{}",
            self.namespace,
            self.kind.as_str(),
            self.name
        ))
    }
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

/// Kubernetes cluster importer
pub struct KubernetesImporter {
    version: String,
}

impl KubernetesImporter {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Default for KubernetesImporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract and resolve the kubeconfig from import credentials.
fn resolve_credentials(
    credentials: &ImportCredentials,
) -> Result<ResolvedKubeconfig, KubernetesImportError> {
    let yaml = if let Some(raw) = credentials.extra.get("kubeconfig") {
        raw.clone()
    } else if let Some(encoded) = credentials.extra.get("kubeconfig_base64") {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| KubernetesImportError::InvalidBase64 {
                field: "kubeconfig_base64".to_string(),
                reason: e.to_string(),
            })?;
        String::from_utf8(bytes).map_err(|e| KubernetesImportError::InvalidKubeconfig {
            reason: format!("kubeconfig_base64 is not valid UTF-8: {}", e),
        })?
    } else {
        return Err(KubernetesImportError::MissingKubeconfig);
    };
    resolve_kubeconfig(&yaml, credentials.extra.get("context").map(|s| s.as_str()))
}

async fn build_client(
    credentials: &ImportCredentials,
) -> Result<(KubeClient, ResolvedKubeconfig), KubernetesImportError> {
    let config = resolve_credentials(credentials)?;
    let client = KubeClient::new(&config).await?;
    Ok((client, config))
}

/// Detect a database image → (temps service type, version from image tag)
fn detect_database(image: &str) -> Option<(SnapshotServiceType, Option<String>)> {
    let (repo, tag) = match image.rsplit_once(':') {
        // "repo:tag" — but avoid splitting "registry:5000/repo"
        Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag.to_string())),
        _ => (image, None),
    };
    let basename = repo.rsplit('/').next().unwrap_or(repo).to_lowercase();
    let version = tag.filter(|t| t != "latest").map(|t| {
        // "16-alpine" → "16"
        t.split(['-', '.']).next().unwrap_or(t.as_str()).to_string()
    });
    match basename.as_str() {
        "postgres" | "postgresql" | "timescaledb" => Some((SnapshotServiceType::Postgres, version)),
        "mysql" | "mariadb" | "percona" => Some((SnapshotServiceType::Mysql, version)),
        "redis" | "valkey" | "keydb" => Some((SnapshotServiceType::Redis, version)),
        "mongo" | "mongodb" => Some((SnapshotServiceType::MongoDB, version)),
        "minio" => Some((SnapshotServiceType::S3, version)),
        _ => None,
    }
}

fn sanitize_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

/// Result of resolving a container's environment through ConfigMaps/Secrets
#[derive(Debug, Default)]
struct EnvResolution {
    values: BTreeMap<String, String>,
    /// key → human-readable source ("Secret 'shop-secrets'", "ConfigMap 'shop-config'")
    sources: BTreeMap<String, String>,
    secret_keys: BTreeSet<String>,
    /// env entries that could not be resolved (fieldRef etc.)
    skipped: Vec<String>,
}

impl KubernetesImporter {
    /// Resolve a container's env through ConfigMaps and Secrets.
    ///
    /// Secret VALUES are never read into the snapshot — only key names,
    /// redacted as `***REDACTED***`, so they can never leak through the plan
    /// API response. The user re-enters secrets in temps (a manual action in
    /// the plan).
    async fn resolve_env(
        &self,
        client: &KubeClient,
        namespace: &str,
        container: &Container,
    ) -> Result<EnvResolution, KubernetesImportError> {
        let mut resolution = EnvResolution::default();
        let mut configmap_cache: HashMap<String, Option<ConfigMap>> = HashMap::new();
        let mut secret_cache: HashMap<String, Option<Secret>> = HashMap::new();

        // envFrom first (whole ConfigMap/Secret), so explicit env entries win
        for env_from in &container.env_from {
            let prefix = env_from.prefix.as_deref().unwrap_or("");
            if let Some(name) = env_from
                .config_map_ref
                .as_ref()
                .and_then(|r| r.name.clone())
            {
                let path = format!("/api/v1/namespaces/{}/configmaps/{}", namespace, name);
                let entry = match configmap_cache.get(&name) {
                    Some(cached) => cached.clone(),
                    None => {
                        let fetched = client.get_opt::<ConfigMap>(&path).await?;
                        configmap_cache.insert(name.clone(), fetched.clone());
                        fetched
                    }
                };
                match entry {
                    Some(config_map) => {
                        for (key, value) in config_map.data {
                            let full_key = format!("{}{}", prefix, key);
                            resolution
                                .sources
                                .insert(full_key.clone(), format!("ConfigMap '{}'", name));
                            resolution.values.insert(full_key, value);
                        }
                    }
                    None => resolution
                        .skipped
                        .push(format!("envFrom ConfigMap '{}' not found", name)),
                }
            }
            if let Some(name) = env_from.secret_ref.as_ref().and_then(|r| r.name.clone()) {
                let path = format!("/api/v1/namespaces/{}/secrets/{}", namespace, name);
                let entry = match secret_cache.get(&name) {
                    Some(cached) => cached.clone(),
                    None => {
                        let fetched = client.get_opt::<Secret>(&path).await?;
                        secret_cache.insert(name.clone(), fetched.clone());
                        fetched
                    }
                };
                match entry {
                    Some(secret) => {
                        for key in secret.data.keys() {
                            let full_key = format!("{}{}", prefix, key);
                            resolution
                                .sources
                                .insert(full_key.clone(), format!("Secret '{}'", name));
                            resolution.secret_keys.insert(full_key.clone());
                            resolution
                                .values
                                .insert(full_key, "***REDACTED***".to_string());
                        }
                    }
                    None => resolution
                        .skipped
                        .push(format!("envFrom Secret '{}' not found", name)),
                }
            }
        }

        for env in &container.env {
            if let Some(value) = &env.value {
                resolution.values.insert(env.name.clone(), value.clone());
                resolution
                    .sources
                    .insert(env.name.clone(), "pod spec".to_string());
                continue;
            }
            let Some(source) = &env.value_from else {
                continue;
            };
            if let Some(config_ref) = &source.config_map_key_ref {
                let name = config_ref.name.clone().unwrap_or_default();
                let path = format!("/api/v1/namespaces/{}/configmaps/{}", namespace, name);
                let entry = match configmap_cache.get(&name) {
                    Some(cached) => cached.clone(),
                    None => {
                        let fetched = client.get_opt::<ConfigMap>(&path).await?;
                        configmap_cache.insert(name.clone(), fetched.clone());
                        fetched
                    }
                };
                match entry.and_then(|cm| cm.data.get(&config_ref.key).cloned()) {
                    Some(value) => {
                        resolution
                            .sources
                            .insert(env.name.clone(), format!("ConfigMap '{}'", name));
                        resolution.values.insert(env.name.clone(), value);
                    }
                    None => resolution.skipped.push(format!(
                        "env '{}' references missing ConfigMap key {}/{}",
                        env.name, name, config_ref.key
                    )),
                }
            } else if let Some(secret_ref) = &source.secret_key_ref {
                let name = secret_ref.name.clone().unwrap_or_default();
                resolution
                    .sources
                    .insert(env.name.clone(), format!("Secret '{}'", name));
                resolution.secret_keys.insert(env.name.clone());
                resolution
                    .values
                    .insert(env.name.clone(), "***REDACTED***".to_string());
            } else if source.field_ref.is_some() || source.resource_field_ref.is_some() {
                resolution.skipped.push(format!(
                    "env '{}' uses fieldRef/resourceFieldRef (pod introspection has no temps equivalent)",
                    env.name
                ));
            }
        }

        Ok(resolution)
    }

    /// Build a `WorkloadSnapshot` from a pod template.
    #[allow(clippy::too_many_arguments)]
    async fn snapshot_from_template(
        &self,
        client: &KubeClient,
        workload_ref: &WorkloadRef,
        template: &PodTemplateSpec,
        replicas: i32,
        ready_replicas: i32,
        created_at: Option<chrono::DateTime<chrono::Utc>>,
        labels: HashMap<String, String>,
        has_pvc: bool,
        cron_schedule: Option<String>,
    ) -> Result<WorkloadSnapshot, KubernetesImportError> {
        let empty_spec;
        let pod_spec = match template.spec.as_ref() {
            Some(spec) => spec,
            None => {
                empty_spec = crate::model::PodSpec {
                    containers: vec![],
                    volumes: vec![],
                    host_network: false,
                    node_name: None,
                };
                &empty_spec
            }
        };

        // Primary container = first non-sidecar
        let primary = pod_spec
            .containers
            .iter()
            .find(|c| !SIDECAR_CONTAINERS.contains(&c.name.as_str()))
            .or_else(|| pod_spec.containers.first());
        let sidecars: Vec<String> = pod_spec
            .containers
            .iter()
            .filter(|c| Some(c.name.as_str()) != primary.map(|p| p.name.as_str()))
            .map(|c| c.name.clone())
            .collect();

        let (
            env,
            sources,
            secret_keys,
            skipped_env,
            image,
            command,
            args,
            working_dir,
            ports,
            resources,
            health_check,
            volumes,
        ) = match primary {
            Some(container) => {
                let resolution = self
                    .resolve_env(client, &workload_ref.namespace, container)
                    .await?;

                let ports: HashMap<u16, Option<u16>> = container
                    .ports
                    .iter()
                    .filter(|p| p.protocol.as_deref().unwrap_or("TCP") == "TCP")
                    .filter_map(|p| u16::try_from(p.container_port).ok())
                    .map(|p| (p, None))
                    .collect();

                let resources = ResourceInfo {
                    cpu_limit: container
                        .resources
                        .limits
                        .get("cpu")
                        .and_then(|q| parse_cpu_millis(q))
                        .map(|millis| millis as f64 / 1000.0),
                    memory_limit: container
                        .resources
                        .limits
                        .get("memory")
                        .and_then(|q| parse_memory_bytes(q)),
                    memory_reservation: container
                        .resources
                        .requests
                        .get("memory")
                        .and_then(|q| parse_memory_bytes(q)),
                    cpu_shares: container
                        .resources
                        .requests
                        .get("cpu")
                        .and_then(|q| parse_cpu_millis(q))
                        .map(|millis| millis * 1024 / 1000),
                };

                // Normalized health-check JSON, consumed by plan generation
                let probe = container
                    .readiness_probe
                    .as_ref()
                    .or(container.liveness_probe.as_ref());
                let health_check = probe.and_then(|p| {
                    let http = p.http_get.as_ref()?;
                    let port = http
                        .port
                        .as_ref()
                        .and_then(|v| v.as_i64())
                        .and_then(|p| u16::try_from(p).ok())
                        .or_else(|| ports.keys().next().copied())?;
                    Some(serde_json::json!({
                        "http_path": http.path.clone().unwrap_or_else(|| "/".to_string()),
                        "port": port,
                        "interval": p.period_seconds.unwrap_or(30),
                        "timeout": p.timeout_seconds.unwrap_or(5),
                        "retries": p.failure_threshold.unwrap_or(3),
                    }))
                });

                // Volumes: join pod volumes with the container's mounts
                let volumes: Vec<SnapshotVolumeMount> = container
                    .volume_mounts
                    .iter()
                    .filter_map(|mount| {
                        let volume = pod_spec.volumes.iter().find(|v| v.name == mount.name)?;
                        let (source, volume_type) =
                            if let Some(pvc) = &volume.persistent_volume_claim {
                                (pvc.claim_name.clone(), VolumeType::Volume)
                            } else if let Some(host_path) = &volume.host_path {
                                (host_path.path.clone(), VolumeType::Bind)
                            } else if let Some(empty_dir) = &volume.empty_dir {
                                let volume_type = if empty_dir.medium.as_deref() == Some("Memory") {
                                    VolumeType::Tmpfs
                                } else {
                                    VolumeType::Volume
                                };
                                (volume.name.clone(), volume_type)
                            } else {
                                // configMap/secret/projected volumes are config, not data
                                return None;
                            };
                        Some(SnapshotVolumeMount {
                            source,
                            destination: mount.mount_path.clone(),
                            read_only: mount.read_only,
                            volume_type,
                        })
                    })
                    .collect();

                (
                    resolution.values,
                    resolution.sources,
                    resolution.secret_keys,
                    resolution.skipped,
                    container.image.clone(),
                    container.command.clone(),
                    container.args.clone(),
                    container.working_dir.clone(),
                    ports,
                    resources,
                    health_check,
                    volumes,
                )
            }
            None => (
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeSet::new(),
                vec![],
                None,
                None,
                None,
                None,
                HashMap::new(),
                ResourceInfo::default(),
                None,
                vec![],
            ),
        };

        let workload_type = if cron_schedule.is_some() {
            WorkloadType::CronJob
        } else if image.as_deref().and_then(detect_database).is_some() {
            WorkloadType::Database
        } else {
            WorkloadType::Container
        };

        let status = if cron_schedule.is_some() {
            WorkloadStatus::Deployed
        } else if ready_replicas > 0 {
            WorkloadStatus::Running
        } else {
            WorkloadStatus::Stopped
        };

        let source_metadata = serde_json::json!({
            "kind": workload_ref.kind.as_str(),
            "namespace": workload_ref.namespace,
            "replicas": replicas,
            "ready_replicas": ready_replicas,
            "secret_env_keys": secret_keys.iter().collect::<Vec<_>>(),
            "env_sources": sources,
            "skipped_env": skipped_env,
            "sidecar_containers": sidecars,
            "has_pvc": has_pvc,
            "cron_schedule": cron_schedule,
            "insecure_tls_skip_verify": client.skip_tls_verify(),
        });

        Ok(WorkloadSnapshot {
            id: workload_ref.id(),
            name: Some(workload_ref.name.clone()),
            workload_type,
            status,
            image,
            // Kubernetes `command` overrides the image entrypoint; `args` the CMD
            command: args,
            entrypoint: command,
            working_dir,
            env: env.into_iter().collect(),
            ports,
            volumes,
            network: NetworkInfo {
                mode: if pod_spec.host_network {
                    NetworkMode::Host
                } else {
                    NetworkMode::Bridge
                },
                networks: vec![],
                hostname: None,
                domain_name: None,
            },
            resources,
            labels,
            health_check,
            restart_policy: Some(RestartPolicy::Always),
            created_at: created_at.unwrap_or_else(chrono::Utc::now),
            source_metadata,
        })
    }

    /// Describe one workload with an existing client.
    async fn describe_internal(
        &self,
        client: &KubeClient,
        workload_ref: &WorkloadRef,
    ) -> Result<WorkloadSnapshot, KubernetesImportError> {
        let not_found = || KubernetesImportError::WorkloadNotFound {
            namespace: workload_ref.namespace.clone(),
            kind: workload_ref.kind.as_str().to_string(),
            name: workload_ref.name.clone(),
        };

        match workload_ref.kind {
            WorkloadKind::Deployment => {
                let path = format!(
                    "/apis/apps/v1/namespaces/{}/deployments/{}",
                    workload_ref.namespace, workload_ref.name
                );
                let deployment: Deployment = client.get_opt(&path).await?.ok_or_else(not_found)?;
                let spec = deployment.spec.as_ref().ok_or_else(not_found)?;
                let has_pvc = spec
                    .template
                    .spec
                    .as_ref()
                    .map(|s| {
                        s.volumes
                            .iter()
                            .any(|v| v.persistent_volume_claim.is_some())
                    })
                    .unwrap_or(false);
                self.snapshot_from_template(
                    client,
                    workload_ref,
                    &spec.template,
                    spec.replicas.unwrap_or(1),
                    deployment
                        .status
                        .as_ref()
                        .and_then(|s| s.ready_replicas)
                        .unwrap_or(0),
                    deployment.metadata.creation_timestamp,
                    deployment.metadata.labels.clone(),
                    has_pvc,
                    None,
                )
                .await
            }
            WorkloadKind::StatefulSet => {
                let path = format!(
                    "/apis/apps/v1/namespaces/{}/statefulsets/{}",
                    workload_ref.namespace, workload_ref.name
                );
                let statefulset: StatefulSet =
                    client.get_opt(&path).await?.ok_or_else(not_found)?;
                let spec = statefulset.spec.as_ref().ok_or_else(not_found)?;
                let has_pvc = !spec.volume_claim_templates.is_empty()
                    || spec
                        .template
                        .spec
                        .as_ref()
                        .map(|s| {
                            s.volumes
                                .iter()
                                .any(|v| v.persistent_volume_claim.is_some())
                        })
                        .unwrap_or(false);
                self.snapshot_from_template(
                    client,
                    workload_ref,
                    &spec.template,
                    spec.replicas.unwrap_or(1),
                    statefulset
                        .status
                        .as_ref()
                        .and_then(|s| s.ready_replicas)
                        .unwrap_or(0),
                    statefulset.metadata.creation_timestamp,
                    statefulset.metadata.labels.clone(),
                    has_pvc,
                    None,
                )
                .await
            }
            WorkloadKind::CronJob => {
                let path = format!(
                    "/apis/batch/v1/namespaces/{}/cronjobs/{}",
                    workload_ref.namespace, workload_ref.name
                );
                let cronjob: CronJob = client.get_opt(&path).await?.ok_or_else(not_found)?;
                let spec = cronjob.spec.as_ref().ok_or_else(not_found)?;
                let template = spec
                    .job_template
                    .spec
                    .as_ref()
                    .map(|s| &s.template)
                    .ok_or_else(not_found)?;
                self.snapshot_from_template(
                    client,
                    workload_ref,
                    template,
                    1,
                    0,
                    cronjob.metadata.creation_timestamp,
                    cronjob.metadata.labels.clone(),
                    false,
                    Some(spec.schedule.clone()),
                )
                .await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WorkloadImporter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl WorkloadImporter for KubernetesImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Kubernetes
    }

    fn name(&self) -> &str {
        "Kubernetes"
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn health_check(&self) -> ImportResult<bool> {
        // Cluster reachability depends entirely on per-request credentials,
        // which are not available here — the importer itself is always ready.
        Ok(true)
    }

    async fn validate_credentials(
        &self,
        credentials: &ImportCredentials,
    ) -> ImportResult<CredentialValidation> {
        let (client, config) = match build_client(credentials).await {
            Ok(pair) => pair,
            Err(e) => {
                return Ok(CredentialValidation {
                    valid: false,
                    account_name: None,
                    message: Some(e.to_string()),
                })
            }
        };
        match client.server_version().await {
            Ok(version) => Ok(CredentialValidation {
                valid: true,
                account_name: Some(format!("{} ({})", config.cluster_name, version)),
                message: Some(format!(
                    "Connected to {} as context '{}'",
                    config.server, config.context_name
                )),
            }),
            Err(e) => Ok(CredentialValidation {
                valid: false,
                account_name: None,
                message: Some(e.to_string()),
            }),
        }
    }

    async fn discover(
        &self,
        credentials: &ImportCredentials,
        selector: ImportSelector,
    ) -> ImportResult<Vec<WorkloadDescriptor>> {
        let (client, config) = build_client(credentials).await?;
        info!("Discovering Kubernetes workloads on {}", config.server);

        let deployments: Vec<Deployment> = client
            .list("/apis/apps/v1/deployments")
            .await
            .map_err(|e| ImportError::DiscoveryFailed(e.to_string()))?;
        let statefulsets: Vec<StatefulSet> = client
            .list("/apis/apps/v1/statefulsets")
            .await
            .map_err(|e| ImportError::DiscoveryFailed(e.to_string()))?;
        let cronjobs: Vec<CronJob> = client
            .list("/apis/batch/v1/cronjobs")
            .await
            .map_err(|e| ImportError::DiscoveryFailed(e.to_string()))?;

        let mut descriptors: Vec<WorkloadDescriptor> = Vec::new();

        let is_user_namespace = |namespace: &Option<String>| {
            namespace
                .as_deref()
                .map(|ns| !SYSTEM_NAMESPACES.contains(&ns))
                .unwrap_or(false)
        };

        for deployment in &deployments {
            if !is_user_namespace(&deployment.metadata.namespace) {
                continue;
            }
            let namespace = deployment.metadata.namespace.clone().unwrap_or_default();
            let image = deployment.spec.as_ref().and_then(|s| {
                s.template
                    .spec
                    .as_ref()
                    .and_then(|p| p.containers.first())
                    .and_then(|c| c.image.clone())
            });
            let ready = deployment
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            descriptors.push(WorkloadDescriptor {
                id: WorkloadId::new(format!(
                    "{}/deployment/{}",
                    namespace, deployment.metadata.name
                )),
                name: Some(deployment.metadata.name.clone()),
                workload_type: image
                    .as_deref()
                    .and_then(detect_database)
                    .map(|_| WorkloadType::Database)
                    .unwrap_or(WorkloadType::Container),
                status: if ready > 0 {
                    WorkloadStatus::Running
                } else {
                    WorkloadStatus::Stopped
                },
                image,
                created_at: deployment.metadata.creation_timestamp,
                labels: deployment.metadata.labels.clone(),
            });
        }

        for statefulset in &statefulsets {
            if !is_user_namespace(&statefulset.metadata.namespace) {
                continue;
            }
            let namespace = statefulset.metadata.namespace.clone().unwrap_or_default();
            let image = statefulset.spec.as_ref().and_then(|s| {
                s.template
                    .spec
                    .as_ref()
                    .and_then(|p| p.containers.first())
                    .and_then(|c| c.image.clone())
            });
            let ready = statefulset
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            descriptors.push(WorkloadDescriptor {
                id: WorkloadId::new(format!(
                    "{}/statefulset/{}",
                    namespace, statefulset.metadata.name
                )),
                name: Some(statefulset.metadata.name.clone()),
                workload_type: image
                    .as_deref()
                    .and_then(detect_database)
                    .map(|_| WorkloadType::Database)
                    .unwrap_or(WorkloadType::Container),
                status: if ready > 0 {
                    WorkloadStatus::Running
                } else {
                    WorkloadStatus::Stopped
                },
                image,
                created_at: statefulset.metadata.creation_timestamp,
                labels: statefulset.metadata.labels.clone(),
            });
        }

        for cronjob in &cronjobs {
            if !is_user_namespace(&cronjob.metadata.namespace) {
                continue;
            }
            let namespace = cronjob.metadata.namespace.clone().unwrap_or_default();
            let image = cronjob.spec.as_ref().and_then(|s| {
                s.job_template
                    .spec
                    .as_ref()
                    .and_then(|j| j.template.spec.as_ref())
                    .and_then(|p| p.containers.first())
                    .and_then(|c| c.image.clone())
            });
            descriptors.push(WorkloadDescriptor {
                id: WorkloadId::new(format!("{}/cronjob/{}", namespace, cronjob.metadata.name)),
                name: Some(cronjob.metadata.name.clone()),
                workload_type: WorkloadType::CronJob,
                status: WorkloadStatus::Deployed,
                image,
                created_at: cronjob.metadata.creation_timestamp,
                labels: cronjob.metadata.labels.clone(),
            });
        }

        // Apply selector
        if let Some(pattern) = &selector.name_pattern {
            let needle = pattern.to_lowercase();
            descriptors.retain(|d| {
                d.name
                    .as_deref()
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }
        if let Some(limit) = selector.limit {
            descriptors.truncate(limit);
        }

        info!("Discovered {} Kubernetes workloads", descriptors.len());
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let (client, _) = build_client(credentials).await?;
        let workload_ref = WorkloadRef::parse(workload_id)?;
        Ok(self.describe_internal(&client, &workload_ref).await?)
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let (client, _) = build_client(credentials).await?;
        let workload_ref = WorkloadRef::parse(workload_id)?;
        let namespace = workload_ref.namespace.clone();

        let primary_workload = self.describe_internal(&client, &workload_ref).await?;

        // -- Namespace siblings: databases become services, the rest become
        //    additional workloads (bounded) ----------------------------------
        let deployments: Vec<Deployment> = client
            .list(&format!(
                "/apis/apps/v1/namespaces/{}/deployments",
                namespace
            ))
            .await
            .map_err(KubernetesImportError::from_context("list deployments"))?;
        let statefulsets: Vec<StatefulSet> = client
            .list(&format!(
                "/apis/apps/v1/namespaces/{}/statefulsets",
                namespace
            ))
            .await
            .map_err(KubernetesImportError::from_context("list statefulsets"))?;
        let cronjobs: Vec<CronJob> = client
            .list(&format!("/apis/batch/v1/namespaces/{}/cronjobs", namespace))
            .await
            .map_err(KubernetesImportError::from_context("list cronjobs"))?;

        let mut services: Vec<ServiceSnapshot> = Vec::new();
        let mut sibling_refs: Vec<WorkloadRef> = Vec::new();

        let consider = |kind: WorkloadKind,
                        name: &str,
                        image: Option<&str>,
                        has_pvc: bool,
                        services: &mut Vec<ServiceSnapshot>,
                        sibling_refs: &mut Vec<WorkloadRef>| {
            if kind == workload_ref.kind && name == workload_ref.name {
                return; // the primary itself
            }
            if let Some(image_str) = image {
                if let Some((service_type, version)) = detect_database(image_str) {
                    services.push(ServiceSnapshot {
                        id: format!("{}/{}/{}", namespace, kind.as_str(), name),
                        name: name.to_string(),
                        service_type,
                        version,
                        connection_url: None,
                        env_vars: HashMap::new(),
                        has_data: has_pvc,
                        data_size_bytes: None,
                        metadata: serde_json::json!({ "image": image_str }),
                    });
                    return;
                }
            }
            sibling_refs.push(WorkloadRef {
                namespace: namespace.clone(),
                kind,
                name: name.to_string(),
            });
        };

        for deployment in &deployments {
            let image = deployment.spec.as_ref().and_then(|s| {
                s.template
                    .spec
                    .as_ref()
                    .and_then(|p| p.containers.first())
                    .and_then(|c| c.image.as_deref())
            });
            let has_pvc = deployment
                .spec
                .as_ref()
                .and_then(|s| s.template.spec.as_ref())
                .map(|p| {
                    p.volumes
                        .iter()
                        .any(|v| v.persistent_volume_claim.is_some())
                })
                .unwrap_or(false);
            consider(
                WorkloadKind::Deployment,
                &deployment.metadata.name,
                image,
                has_pvc,
                &mut services,
                &mut sibling_refs,
            );
        }
        for statefulset in &statefulsets {
            let image = statefulset.spec.as_ref().and_then(|s| {
                s.template
                    .spec
                    .as_ref()
                    .and_then(|p| p.containers.first())
                    .and_then(|c| c.image.as_deref())
            });
            let has_pvc = statefulset
                .spec
                .as_ref()
                .map(|s| !s.volume_claim_templates.is_empty())
                .unwrap_or(false);
            consider(
                WorkloadKind::StatefulSet,
                &statefulset.metadata.name,
                image,
                has_pvc,
                &mut services,
                &mut sibling_refs,
            );
        }
        for cronjob in cronjobs.iter() {
            consider(
                WorkloadKind::CronJob,
                &cronjob.metadata.name,
                None,
                false,
                &mut services,
                &mut sibling_refs,
            );
        }

        let mut additional_workloads = Vec::new();
        let truncated_siblings = sibling_refs.len().saturating_sub(MAX_ADDITIONAL_WORKLOADS);
        for sibling in sibling_refs.into_iter().take(MAX_ADDITIONAL_WORKLOADS) {
            match self.describe_internal(&client, &sibling).await {
                Ok(snapshot) => additional_workloads.push(snapshot),
                Err(e) => warn!(
                    "Failed to describe sibling workload {}: {}",
                    sibling.id(),
                    e
                ),
            }
        }

        // -- Ingress hosts → domains ---------------------------------------
        let ingresses: Vec<Ingress> = client
            .list(&format!(
                "/apis/networking.k8s.io/v1/namespaces/{}/ingresses",
                namespace
            ))
            .await
            .unwrap_or_else(|e| {
                debug!("Could not list ingresses in {}: {}", namespace, e);
                vec![]
            });
        let mut domains: Vec<DomainSnapshot> = Vec::new();
        let mut seen_hosts = BTreeSet::new();
        for ingress in &ingresses {
            let Some(spec) = &ingress.spec else { continue };
            for rule in &spec.rules {
                let Some(host) = &rule.host else { continue };
                if !seen_hosts.insert(host.clone()) {
                    continue;
                }
                domains.push(DomainSnapshot {
                    domain: host.clone(),
                    is_apex: host.matches('.').count() <= 1,
                    redirect_to: None,
                    redirect_status_code: None,
                    environment: Some("production".to_string()),
                    verified: true,
                });
            }
        }

        // -- HPAs (unsupported-feature reporting) ---------------------------
        let hpas: Vec<HorizontalPodAutoscaler> = client
            .list(&format!(
                "/apis/autoscaling/v2/namespaces/{}/horizontalpodautoscalers",
                namespace
            ))
            .await
            .unwrap_or_else(|e| {
                debug!("Could not list HPAs in {}: {}", namespace, e);
                vec![]
            });
        let hpa_summaries: Vec<String> = hpas
            .iter()
            .filter_map(|hpa| {
                let spec = hpa.spec.as_ref()?;
                Some(format!(
                    "{} → {}/{} ({}..{} replicas)",
                    hpa.metadata.name,
                    spec.scale_target_ref.kind,
                    spec.scale_target_ref.name,
                    spec.min_replicas.unwrap_or(1),
                    spec.max_replicas
                ))
            })
            .collect();

        // -- Cluster observation for the cost analysis ----------------------
        let observation = match observe_cluster(&client).await {
            Ok(observation) => Some(observation),
            Err(e) => {
                warn!("Cluster observation failed (cost analysis skipped): {}", e);
                None
            }
        };

        let mut source_metadata = serde_json::json!({
            "namespace": namespace,
            "hpas": hpa_summaries,
            "truncated_siblings": truncated_siblings,
        });
        if let Some(observation) = &observation {
            source_metadata[CLUSTER_OBSERVATION_KEY] =
                serde_json::to_value(observation).map_err(ImportError::SerializationError)?;
        }

        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name: workload_ref.name.clone(),
            primary_workload,
            additional_workloads,
            services,
            domains,
            git_info: None,
            detected_framework: None,
            source_metadata,
        })
    }

    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan> {
        let name = snapshot
            .name
            .clone()
            .unwrap_or_else(|| snapshot.id.as_str().replace('/', "-"));
        let source_id = snapshot.id.as_str().to_string();
        self.build_plan(&snapshot, &name, &source_id, PlanExtras::default())
    }

    fn generate_project_plan(&self, snapshot: ProjectSnapshot) -> ImportResult<ImportPlan> {
        let observation: Option<ClusterObservation> = snapshot
            .source_metadata
            .get(CLUSTER_OBSERVATION_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let hpas: Vec<String> = snapshot
            .source_metadata
            .get("hpas")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let extras = PlanExtras {
            services: snapshot.services.clone(),
            domains: snapshot.domains.clone(),
            additional_workloads: snapshot.additional_workloads.clone(),
            observation,
            hpas,
        };
        let source_id = snapshot.id.as_str().to_string();
        self.build_plan(
            &snapshot.primary_workload,
            &snapshot.name,
            &source_id,
            extras,
        )
    }

    fn validation_rules(&self) -> Vec<Box<dyn temps_import_types::ImportValidationRule>> {
        KubernetesValidationRules::all_rules()
    }

    async fn execute(
        &self,
        context: ImportContext,
        plan: ImportPlan,
        services: &dyn ImportServiceProvider,
    ) -> ImportResult<ImportOutcome> {
        execute_plan(self, context, plan, services).await
    }

    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            supports_volumes: true,
            supports_networks: false,
            supports_health_checks: true,
            supports_resource_limits: true,
            supports_build: false, // Kubernetes workloads are pre-built images
            supports_stacks: true, // a namespace is imported as one project
            supports_services: true,
            supports_domains: true,
            supports_project_snapshot: true,
            supports_cost_analysis: true,
        }
    }
}

impl KubernetesImportError {
    /// Helper to add operation context when converting to ImportError in closures
    fn from_context(operation: &'static str) -> impl Fn(KubernetesImportError) -> ImportError {
        move |e| ImportError::DiscoveryFailed(format!("{}: {}", operation, e))
    }
}

// ---------------------------------------------------------------------------
// Plan generation
// ---------------------------------------------------------------------------

/// Project-level inputs beyond the primary workload
#[derive(Debug, Default)]
struct PlanExtras {
    services: Vec<ServiceSnapshot>,
    domains: Vec<DomainSnapshot>,
    additional_workloads: Vec<WorkloadSnapshot>,
    observation: Option<ClusterObservation>,
    hpas: Vec<String>,
}

impl KubernetesImporter {
    fn build_plan(
        &self,
        snapshot: &WorkloadSnapshot,
        project_name: &str,
        source_id: &str,
        extras: PlanExtras,
    ) -> ImportResult<ImportPlan> {
        use temps_import_types::plan::{
            DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration,
            EnvironmentVariable, HealthCheckConfiguration, NetworkConfiguration, PlanComplexity,
            PlanMetadata, PortMapping, ProjectConfiguration, ProjectType, Protocol, ResourceLimits,
            VolumeMount as PlanVolumeMount, VolumeType as PlanVolumeType,
        };

        debug!("Generating Kubernetes import plan for {}", snapshot.id);

        let slug = sanitize_slug(project_name);
        let namespace = snapshot
            .source_metadata
            .get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        let secret_keys: BTreeSet<String> = snapshot
            .source_metadata
            .get("secret_env_keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env_sources: HashMap<String, String> = snapshot
            .source_metadata
            .get("env_sources")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let replicas = snapshot
            .source_metadata
            .get("replicas")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        let sidecars: Vec<String> = snapshot
            .source_metadata
            .get("sidecar_containers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let skipped_env: Vec<String> = snapshot
            .source_metadata
            .get("skipped_env")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let insecure_tls_skip_verify = snapshot
            .source_metadata
            .get("insecure_tls_skip_verify")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let project = ProjectConfiguration {
            name: project_name.to_string(),
            slug: slug.clone(),
            project_type: ProjectType::Docker,
            is_web_app: !snapshot.ports.is_empty(),
        };

        let resources = ResourceLimits {
            cpu_limit: snapshot.resources.cpu_limit.map(|c| (c * 1000.0) as i32),
            memory_limit: snapshot
                .resources
                .memory_limit
                .map(|m| bytes_to_mb(m) as i32),
            cpu_request: snapshot
                .resources
                .cpu_shares
                .map(|s| (s * 1000 / 1024) as i32),
            memory_request: snapshot
                .resources
                .memory_reservation
                .map(|m| bytes_to_mb(m) as i32),
        };

        let environment = EnvironmentConfiguration {
            name: "production".to_string(),
            subdomain: slug.clone(),
            resources: resources.clone(),
        };

        let env_vars: Vec<EnvironmentVariable> = snapshot
            .env
            .iter()
            .map(|(key, value)| {
                // Known secret sources beat the name heuristic
                let upper = key.to_uppercase();
                let is_secret = secret_keys.contains(key)
                    || upper.contains("SECRET")
                    || upper.contains("PASSWORD")
                    || upper.contains("TOKEN")
                    || upper.contains("API_KEY");
                EnvironmentVariable {
                    key: key.clone(),
                    value: if is_secret {
                        "***REDACTED***".to_string()
                    } else {
                        value.clone()
                    },
                    is_secret,
                    source_description: env_sources.get(key).cloned(),
                }
            })
            .collect();
        let env_var_count = env_vars.len();
        let secret_count = env_vars.iter().filter(|e| e.is_secret).count();

        let mut ports: Vec<PortMapping> = snapshot
            .ports
            .keys()
            .map(|port| PortMapping {
                container_port: *port,
                host_port: None,
                protocol: Protocol::Tcp,
                is_primary: false,
            })
            .collect();
        ports.sort_by_key(|p| p.container_port);
        if let Some(primary) = ports
            .iter_mut()
            .find(|p| matches!(p.container_port, 80 | 443 | 8080 | 8000 | 3000 | 5000))
        {
            primary.is_primary = true;
        } else if let Some(first) = ports.first_mut() {
            first.is_primary = true;
        }

        let volumes: Vec<PlanVolumeMount> = snapshot
            .volumes
            .iter()
            .map(|v| PlanVolumeMount {
                source: v.source.clone(),
                destination: v.destination.clone(),
                read_only: v.read_only,
                volume_type: match v.volume_type {
                    VolumeType::Bind => PlanVolumeType::Bind,
                    VolumeType::Volume => PlanVolumeType::Volume,
                    VolumeType::Tmpfs => PlanVolumeType::Tmpfs,
                },
            })
            .collect();

        let health_check = snapshot.health_check.as_ref().and_then(|hc| {
            Some(HealthCheckConfiguration {
                http_path: hc
                    .get("http_path")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                port: hc
                    .get("port")
                    .and_then(|v| v.as_u64())
                    .and_then(|p| u16::try_from(p).ok())?,
                interval: hc.get("interval").and_then(|v| v.as_u64()).unwrap_or(30) as u32,
                timeout: hc.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5) as u32,
                retries: hc.get("retries").and_then(|v| v.as_u64()).unwrap_or(3) as u32,
            })
        });

        let deployment = DeploymentConfiguration {
            image: snapshot.image.clone().unwrap_or_default(),
            build: None,
            strategy: DeploymentStrategy::Replace,
            env_vars,
            ports,
            volumes,
            network: NetworkConfiguration {
                mode: snapshot.network.mode.clone(),
                hostname: None,
                dns_servers: vec![],
            },
            resources,
            command: snapshot.command.clone(),
            entrypoint: snapshot.entrypoint.clone(),
            working_dir: snapshot.working_dir.clone(),
            health_check,
            git: None,
        };

        // -- Additional deployments (siblings) ------------------------------
        let additional_deployments: Vec<DeploymentConfiguration> = extras
            .additional_workloads
            .iter()
            .map(|workload| DeploymentConfiguration {
                image: workload.image.clone().unwrap_or_default(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: vec![],
                ports: workload
                    .ports
                    .keys()
                    .map(|port| PortMapping {
                        container_port: *port,
                        host_port: None,
                        protocol: Protocol::Tcp,
                        is_primary: false,
                    })
                    .collect(),
                volumes: vec![],
                network: NetworkConfiguration {
                    mode: NetworkMode::Bridge,
                    hostname: None,
                    dns_servers: vec![],
                },
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
                command: workload.command.clone(),
                entrypoint: workload.entrypoint.clone(),
                working_dir: None,
                health_check: None,
                git: None,
            })
            .collect();

        // -- Service plans ---------------------------------------------------
        let service_plans: Vec<ServicePlan> = extras
            .services
            .iter()
            .map(|service| {
                let mut implications = vec![];
                if service.has_data {
                    implications.push(DataImplication {
                        severity: DataImplicationSeverity::DataNotMigrated,
                        message: format!(
                            "'{}' has persistent data in the cluster that is NOT copied automatically",
                            service.name
                        ),
                        recommended_action: Some(match service.service_type {
                            SnapshotServiceType::Postgres => format!(
                                "After the temps service is running: kubectl exec <{}-pod> -- pg_dump -U <user> <db> | psql <temps connection url>",
                                service.name
                            ),
                            SnapshotServiceType::Mysql => "Dump with mysqldump from the cluster and restore into the temps service".to_string(),
                            _ => "Export the data from the cluster and restore it into the temps service".to_string(),
                        }),
                    });
                }
                ServicePlan {
                    name: service.name.clone(),
                    service_type: service.service_type.to_string().to_lowercase(),
                    version: service.version.clone(),
                    parameters: HashMap::new(),
                    env_var_mappings: HashMap::new(),
                    action: ServiceAction::Create,
                    action_description: format!(
                        "Create a temps-managed {} service to replace '{}' (empty — data migrated separately)",
                        service.service_type, service.name
                    ),
                    data_implications: implications,
                }
            })
            .collect();

        // -- Domain plans ----------------------------------------------------
        let domain_plans: Vec<DomainPlan> = extras
            .domains
            .iter()
            .map(|domain| DomainPlan {
                domain: domain.domain.clone(),
                environment: "production".to_string(),
                redirect_to: domain.redirect_to.clone(),
                status_code: domain.redirect_status_code,
                action: DomainAction::Import,
                action_description: format!(
                    "Register '{}' in temps; you must point its DNS at the temps server afterwards",
                    domain.domain
                ),
                replacement: None,
            })
            .collect();

        // -- Steps -----------------------------------------------------------
        let mut steps = vec![
            temps_import_types::MigrationStep {
                order: 1,
                id: "create-project".to_string(),
                title: format!("Create project '{}'", project.name),
                description: format!(
                    "Creates project '{}' in temps from Kubernetes namespace '{}'.",
                    project.name, namespace
                ),
                resource_type: temps_import_types::StepResourceType::Project,
                risk: temps_import_types::RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec!["Verify project appears in dashboard".to_string()],
                skippable: false,
                skipped: false,
                reversible: true,
                estimated_duration: Some("< 1 second".to_string()),
            },
            temps_import_types::MigrationStep {
                order: 2,
                id: "set-env-vars".to_string(),
                title: format!("Set {} environment variables", env_var_count),
                description: format!(
                    "Copies {} environment variables resolved from the pod spec, ConfigMaps, and Secret key names. {} secret value(s) are NOT read from the cluster — re-enter them in temps.",
                    env_var_count, secret_count
                ),
                resource_type: temps_import_types::StepResourceType::EnvironmentVariable,
                risk: temps_import_types::RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Verify environment variables in project settings".to_string()
                ],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("< 1 second".to_string()),
            },
            temps_import_types::MigrationStep {
                order: 3,
                id: "create-deployment".to_string(),
                title: format!("Record deployment from image '{}'", deployment.image),
                description: "Records a deployment entry pointing at the existing image. No container is started — trigger a deploy in temps afterwards to pull the image and start it behind the proxy.".to_string(),
                resource_type: temps_import_types::StepResourceType::Deployment,
                risk: temps_import_types::RiskLevel::None,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Trigger a deployment in temps to start the container".to_string()
                ],
                skippable: false,
                skipped: false,
                reversible: true,
                estimated_duration: Some("< 1 second".to_string()),
            },
        ];

        for service_plan in &service_plans {
            steps.push(temps_import_types::MigrationStep {
                order: 0, // renumbered below
                id: format!("create-service-{}", sanitize_slug(&service_plan.name)),
                title: format!(
                    "Create managed {} service '{}'",
                    service_plan.service_type, service_plan.name
                ),
                description: service_plan.action_description.clone(),
                resource_type: temps_import_types::StepResourceType::Service,
                risk: temps_import_types::RiskLevel::Medium,
                data_implications: service_plan.data_implications.clone(),
                pre_conditions: vec![],
                post_conditions: vec!["Migrate the data and update connection strings".to_string()],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("manual".to_string()),
            });
        }
        for domain_plan in &domain_plans {
            steps.push(temps_import_types::MigrationStep {
                order: 0,
                id: format!("import-domain-{}", sanitize_slug(&domain_plan.domain)),
                title: format!("Import domain '{}'", domain_plan.domain),
                description: domain_plan.action_description.clone(),
                resource_type: temps_import_types::StepResourceType::Domain,
                risk: temps_import_types::RiskLevel::Medium,
                data_implications: vec![DataImplication {
                    severity: DataImplicationSeverity::Warning,
                    message: format!(
                        "Traffic to '{}' keeps hitting the cluster until DNS is updated",
                        domain_plan.domain
                    ),
                    recommended_action: Some(
                        "Lower the DNS TTL before migration; switch the record once temps serves the app".to_string(),
                    ),
                }],
                pre_conditions: vec![],
                post_conditions: vec!["Verify TLS certificate issuance in temps".to_string()],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("manual DNS change".to_string()),
            });
        }
        for (index, step) in steps.iter_mut().enumerate() {
            step.order = index + 1;
        }

        // -- Summary ---------------------------------------------------------
        let mut critical_warnings = Vec::new();
        let mut manual_actions = vec![ManualAction {
            timing: ManualActionTiming::AfterMigration,
            description: "Trigger the first deployment in temps to actually start the container"
                .to_string(),
            reason: "The import records configuration only — the image is pulled on first deploy"
                .to_string(),
        }];
        let mut unsupported_features = Vec::new();

        if secret_count > 0 {
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::AfterMigration,
                description: format!(
                    "Re-enter {} secret environment value(s) in temps",
                    secret_count
                ),
                reason: "Secret values are never read out of the cluster by the importer"
                    .to_string(),
            });
        }
        if service_plans.iter().any(|s| {
            s.data_implications
                .iter()
                .any(|d| d.severity == DataImplicationSeverity::DataNotMigrated)
        }) {
            critical_warnings.push(
                "Database data stays in the cluster — dump and restore it into the temps-managed service before switching traffic".to_string(),
            );
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::BeforeMigration,
                description: "Plan the database dump/restore window".to_string(),
                reason: "Data is not copied automatically".to_string(),
            });
        }
        if replicas > 1 {
            critical_warnings.push(format!(
                "Workload runs {} replicas in Kubernetes; temps deploys a single container per environment",
                replicas
            ));
        }
        if !domain_plans.is_empty() {
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::WithinHours,
                description: format!(
                    "Update DNS for {} domain(s) to point at the temps server",
                    domain_plans.len()
                ),
                reason: "DNS is controlled outside both platforms".to_string(),
            });
        }
        for workload in &extras.additional_workloads {
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::AfterMigration,
                description: format!(
                    "Import sibling workload '{}' as its own temps project",
                    workload.id
                ),
                reason: "temps deploys one primary container per project".to_string(),
            });
        }
        for hpa in &extras.hpas {
            unsupported_features.push(UnsupportedFeature {
                feature: format!("HorizontalPodAutoscaler: {}", hpa),
                reason: "temps does not autoscale replicas".to_string(),
                alternative: Some(
                    "Size the environment from the measured usage in the cost analysis; temps monitoring alerts when limits approach".to_string(),
                ),
            });
        }
        if !sidecars.is_empty() {
            unsupported_features.push(UnsupportedFeature {
                feature: format!("Sidecar containers: {}", sidecars.join(", ")),
                reason: "Only the primary container is imported".to_string(),
                alternative: Some(
                    "Service-mesh sidecars are unnecessary behind the temps proxy".to_string(),
                ),
            });
        }
        for skipped in &skipped_env {
            unsupported_features.push(UnsupportedFeature {
                feature: format!("Environment: {}", skipped),
                reason: "No temps equivalent".to_string(),
                alternative: None,
            });
        }

        let overall_risk = if critical_warnings.is_empty() {
            if domain_plans.is_empty() {
                temps_import_types::RiskLevel::Low
            } else {
                temps_import_types::RiskLevel::Medium
            }
        } else {
            temps_import_types::RiskLevel::High
        };

        // -- Cost analysis ---------------------------------------------------
        let cost_analysis = extras.observation.as_ref().map(compute_cost_analysis);

        let mut headline = format!(
            "Migrate '{}' from Kubernetes namespace '{}' with {} service(s) and {} domain(s)",
            project.name,
            namespace,
            service_plans.len(),
            domain_plans.len(),
        );
        if let Some(analysis) = &cost_analysis {
            if let Some(savings) = analysis.recommendation.monthly_savings_usd {
                headline.push_str(&format!(" — estimated savings ~${:.0}/mo", savings));
            }
        }

        let summary = temps_import_types::MigrationSummary {
            headline,
            overall_risk,
            resource_counts: temps_import_types::ResourceCounts {
                projects: 1,
                environments: 1,
                deployments: 1 + additional_deployments.len(),
                environment_variables: env_var_count,
                services: service_plans.len(),
                domains: domain_plans.len(),
            },
            critical_warnings,
            manual_actions_required: manual_actions,
            unsupported_features,
        };

        let complexity_score = service_plans.len()
            + domain_plans.len()
            + additional_deployments.len()
            + deployment.volumes.len();
        let metadata = PlanMetadata {
            generated_at: chrono::Utc::now(),
            generator_version: self.version.clone(),
            complexity: match complexity_score {
                0..=1 => PlanComplexity::Low,
                2..=4 => PlanComplexity::Medium,
                _ => PlanComplexity::High,
            },
            warnings: {
                let mut warnings = Vec::new();
                if extras.observation.is_none() {
                    warnings.push(
                        "Cluster observation unavailable — plan generated without cost analysis"
                            .to_string(),
                    );
                }
                if insecure_tls_skip_verify {
                    warnings.push(
                        "Kubeconfig has insecure-skip-tls-verify set — TLS certificate verification was disabled while connecting to this cluster".to_string(),
                    );
                }
                warnings
            },
        };

        Ok(ImportPlan {
            version: "1.0".to_string(),
            source: "kubernetes".to_string(),
            source_id: source_id.to_string(),
            project,
            environment,
            deployment,
            services: service_plans,
            domains: domain_plans,
            additional_deployments,
            steps,
            summary,
            metadata,
            cost_analysis,
        })
    }
}

// ---------------------------------------------------------------------------
// Execution (mirrors the Docker importer's record-only execution)
// ---------------------------------------------------------------------------

async fn execute_plan(
    _importer: &KubernetesImporter,
    context: ImportContext,
    plan: ImportPlan,
    services: &dyn ImportServiceProvider,
) -> ImportResult<ImportOutcome> {
    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    };
    use std::time::Instant;
    use temps_entities::{
        deployment_containers, deployments, environments, prelude::DeploymentMetadata, projects,
    };

    let start_time = Instant::now();
    info!(
        "Executing Kubernetes import for session: {} (dry_run: {})",
        context.session_id, context.dry_run
    );

    // Manual steps reported honestly in the outcome
    let mut step_results: Vec<StepResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for service_plan in plan
        .services
        .iter()
        .filter(|s| s.action == ServiceAction::Create)
    {
        let message = format!(
            "Managed service creation is not yet automated for Kubernetes imports — create a {} service named '{}' in temps and migrate its data (see the plan's data implications)",
            service_plan.service_type, service_plan.name
        );
        warnings.push(message.clone());
        step_results.push(StepResult {
            step_id: format!("create-service-{}", sanitize_slug(&service_plan.name)),
            step_title: format!("Create managed service '{}'", service_plan.name),
            success: true,
            skipped: true,
            message,
            created_resources: vec![],
            duration_seconds: 0.0,
        });
    }
    for domain_plan in plan
        .domains
        .iter()
        .filter(|d| d.action == DomainAction::Import)
    {
        let message = format!(
            "Domain registration is not yet automated for Kubernetes imports — add '{}' to the environment in temps and update its DNS",
            domain_plan.domain
        );
        warnings.push(message.clone());
        step_results.push(StepResult {
            step_id: format!("import-domain-{}", sanitize_slug(&domain_plan.domain)),
            step_title: format!("Import domain '{}'", domain_plan.domain),
            success: true,
            skipped: true,
            message,
            created_resources: vec![],
            duration_seconds: 0.0,
        });
    }

    if context.dry_run {
        info!(
            "Dry run — would create project '{}' with environment '{}' and image '{}'",
            context.project_name, plan.environment.name, plan.deployment.image
        );
        return Ok(ImportOutcome {
            session_id: context.session_id.clone(),
            success: true,
            project_id: None,
            environment_id: None,
            deployment_id: None,
            warnings,
            errors: vec![],
            created_resources: vec![],
            step_results,
            duration_seconds: start_time.elapsed().as_secs_f64(),
        });
    }

    let mut created_resources = Vec::new();

    let db = services.db();
    let project_service = services
        .project_service()
        .downcast_ref::<temps_projects::ProjectService>()
        .ok_or_else(|| ImportError::Internal("Failed to get ProjectService".to_string()))?;

    use temps_presets::get_preset_by_slug;
    if get_preset_by_slug(&context.preset).is_none() {
        warnings.push(format!(
            "Preset '{}' not found, using default configuration",
            context.preset
        ));
    }

    let create_project_request = temps_projects::services::types::CreateProjectRequest {
        name: context.project_name.clone(),
        repo_name: context.repo_name.clone(),
        repo_owner: context.repo_owner.clone(),
        directory: context.directory.clone(),
        main_branch: context.main_branch.clone(),
        preset: context.preset.clone(),
        preset_config: None,
        environment_variables: Some(
            plan.deployment
                .env_vars
                .iter()
                .filter(|env| !env.is_secret)
                .map(|env| (env.key.clone(), env.value.clone()))
                .collect(),
        ),
        automatic_deploy: true,
        storage_service_ids: vec![],
        is_public_repo: None,
        git_url: None,
        git_provider_connection_id: context.git_provider_connection_id,
        exposed_port: None,
        // Kubernetes workloads are pure containers with no git repo —
        // DockerImage is the correct source_type (was incorrectly Git, which
        // both misrepresents the project and made determine_deployment_source_type
        // try to plan a git deploy on any future redeploy with no metadata override).
        source_type: temps_entities::source_type::SourceType::DockerImage,
        template_slug: None,
    };

    let project = project_service
        .create_project(create_project_request)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to create project: {}", e)))?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "project".to_string(),
        resource_id: project.id,
        resource_name: project.name.clone(),
    });
    info!(
        "✓ Created project {} (id: {}) from Kubernetes import",
        project.name, project.id
    );

    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .filter(environments::Column::Name.eq(&plan.environment.name))
        .one(db)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to find environment: {}", e)))?
        .ok_or_else(|| {
            ImportError::ExecutionFailed(format!(
                "Environment '{}' not found for project {}",
                plan.environment.name, project.id
            ))
        })?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "environment".to_string(),
        resource_id: environment.id,
        resource_name: environment.name.clone(),
    });

    let deployment_count = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .paginate(db, 100)
        .num_items()
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to count deployments: {}", e)))?;
    let deployment_slug = format!("{}-{}", project.slug, deployment_count + 1);

    let deployment_metadata = DeploymentMetadata {
        builder: Some("kubernetes-import".to_string()),
        labels: vec!["imported".to_string(), "kubernetes".to_string()],
        ..Default::default()
    };

    let now = chrono::Utc::now();
    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set(deployment_slug.clone()),
        state: Set("pending".to_string()),
        metadata: Set(Some(deployment_metadata)),
        image_name: Set(Some(plan.deployment.image.clone())),
        commit_message: Set(Some(format!(
            "Imported from Kubernetes ({})",
            plan.source_id
        ))),
        // Not deployed yet — the orchestrator triggers the real
        // pipeline and records the outcome after this returns.
        deploying_at: Set(None),
        ready_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        ..Default::default()
    };
    let deployment = deployment
        .insert(db)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to create deployment: {}", e)))?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "deployment".to_string(),
        resource_id: deployment.id,
        resource_name: deployment.slug.clone(),
    });

    for (index, port_mapping) in plan.deployment.ports.iter().enumerate() {
        let container_name = if index == 0 {
            format!("{}-{}", project.name, deployment.slug)
        } else {
            format!("{}-{}-{}", project.name, deployment.slug, index)
        };
        let deployment_container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(plan.source_id.clone()),
            container_name: Set(container_name),
            container_port: Set(port_mapping.container_port as i32),
            host_port: Set(port_mapping.host_port.map(|p| p as i32)),
            image_name: Set(Some(plan.deployment.image.clone())),
            status: Set(Some("pending".to_string())),
            deployed_at: Set(now),
            ready_at: Set(None),
            ..Default::default()
        };
        deployment_container.insert(db).await.map_err(|e| {
            ImportError::ExecutionFailed(format!(
                "Failed to create deployment container for port {}: {}",
                port_mapping.container_port, e
            ))
        })?;
    }

    let mut environment_active: environments::ActiveModel = environment.clone().into();
    environment_active.current_deployment_id = Set(Some(deployment.id));
    environment_active.last_deployment = Set(Some(now));
    environment_active.update(db).await.map_err(|e| {
        ImportError::ExecutionFailed(format!(
            "Failed to update environment with current deployment: {}",
            e
        ))
    })?;

    let project_entity = projects::Entity::find_by_id(project.id)
        .one(db)
        .await
        .map_err(|e| {
            ImportError::ExecutionFailed(format!("Failed to fetch project entity: {}", e))
        })?
        .ok_or_else(|| {
            ImportError::ExecutionFailed(format!("Project {} not found after creation", project.id))
        })?;
    let mut project_active: projects::ActiveModel = project_entity.into();
    project_active.last_deployment = Set(Some(now));
    project_active.update(db).await.map_err(|e| {
        ImportError::ExecutionFailed(format!("Failed to update project last deployment: {}", e))
    })?;

    let duration = start_time.elapsed().as_secs_f64();
    info!(
        "✅ Kubernetes import completed in {:.2}s — project {} (id: {}), environment {} (id: {}), deployment {} (id: {})",
        duration, project.name, project.id, environment.name, environment.id, deployment.slug, deployment.id
    );

    Ok(ImportOutcome {
        session_id: context.session_id.clone(),
        success: true,
        project_id: Some(project.id),
        environment_id: Some(environment.id),
        deployment_id: Some(deployment.id),
        warnings,
        errors: vec![],
        created_resources,
        step_results,
        duration_seconds: duration,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ObservedNode;
    use temps_import_types::ValidationStatus;

    fn snapshot(name: &str) -> WorkloadSnapshot {
        let mut env = HashMap::new();
        env.insert("PLAN".to_string(), "pro".to_string());
        env.insert("LAB_SECRET".to_string(), "***REDACTED***".to_string());
        env.insert(
            "DATABASE_URL".to_string(),
            "postgres://lab@db.shop.svc:5432/labdb".to_string(),
        );
        let mut ports = HashMap::new();
        ports.insert(80u16, None);
        WorkloadSnapshot {
            id: WorkloadId::new(format!("shop/deployment/{}", name)),
            name: Some(name.to_string()),
            workload_type: WorkloadType::Container,
            status: WorkloadStatus::Running,
            image: Some("traefik/whoami:latest".to_string()),
            command: None,
            entrypoint: None,
            working_dir: None,
            env,
            ports,
            volumes: vec![],
            network: NetworkInfo {
                mode: NetworkMode::Bridge,
                networks: vec![],
                hostname: None,
                domain_name: None,
            },
            resources: ResourceInfo {
                cpu_limit: Some(1.0),
                memory_limit: Some(1024 * 1024 * 1024),
                memory_reservation: Some(768 * 1024 * 1024),
                cpu_shares: Some(800 * 1024 / 1000),
            },
            labels: HashMap::new(),
            health_check: Some(serde_json::json!({
                "http_path": "/health", "port": 80, "interval": 10, "timeout": 5, "retries": 3
            })),
            restart_policy: Some(RestartPolicy::Always),
            created_at: chrono::Utc::now(),
            source_metadata: serde_json::json!({
                "kind": "deployment",
                "namespace": "shop",
                "replicas": 1,
                "secret_env_keys": ["LAB_SECRET"],
                "env_sources": {"LAB_SECRET": "Secret 'shop-secrets'", "PLAN": "ConfigMap 'shop-config'"},
                "sidecar_containers": [],
                "skipped_env": [],
            }),
        }
    }

    fn observation() -> ClusterObservation {
        ClusterObservation {
            nodes: vec![ObservedNode {
                name: "lab-k8s".to_string(),
                instance_type: Some("cpx22".to_string()),
                region: Some("fsn1".to_string()),
                provider_id: Some("hcloud://123".to_string()),
                is_eks: false,
                is_gke: false,
                is_ask: false,
                allocatable_cpu_millis: 2000,
                allocatable_memory_mb: 3840,
                capacity_cpu_millis: 2000,
                capacity_memory_mb: 3900,
            }],
            requested_cpu_millis: 1550,
            requested_memory_mb: 1568,
            actual_cpu_millis: Some(60),
            actual_memory_mb: Some(700),
            running_pods: 9,
        }
    }

    fn project_snapshot() -> ProjectSnapshot {
        let mut source_metadata = serde_json::json!({
            "namespace": "shop",
            "hpas": ["storefront → Deployment/storefront (1..3 replicas)"],
            "truncated_siblings": 0,
        });
        source_metadata[CLUSTER_OBSERVATION_KEY] = serde_json::to_value(observation()).unwrap();
        ProjectSnapshot {
            id: WorkloadId::new("shop/deployment/storefront"),
            name: "storefront".to_string(),
            primary_workload: snapshot("storefront"),
            additional_workloads: vec![snapshot("api")],
            services: vec![ServiceSnapshot {
                id: "shop/statefulset/db".to_string(),
                name: "db".to_string(),
                service_type: SnapshotServiceType::Postgres,
                version: Some("16".to_string()),
                connection_url: None,
                env_vars: HashMap::new(),
                has_data: true,
                data_size_bytes: None,
                metadata: serde_json::json!({"image": "postgres:16"}),
            }],
            domains: vec![DomainSnapshot {
                domain: "storefront.203.0.113.10.sslip.io".to_string(),
                is_apex: false,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            }],
            git_info: None,
            detected_framework: None,
            source_metadata,
        }
    }

    #[test]
    fn test_workload_ref_parse() {
        let workload_ref =
            WorkloadRef::parse(&WorkloadId::new("shop/deployment/storefront")).unwrap();
        assert_eq!(workload_ref.namespace, "shop");
        assert_eq!(workload_ref.kind, WorkloadKind::Deployment);
        assert_eq!(workload_ref.name, "storefront");

        assert!(WorkloadRef::parse(&WorkloadId::new("no-slashes")).is_err());
        assert!(WorkloadRef::parse(&WorkloadId::new("a/daemonset/b")).is_err());
        assert!(WorkloadRef::parse(&WorkloadId::new("a//b")).is_err());
    }

    #[test]
    fn test_detect_database() {
        assert_eq!(
            detect_database("postgres:16-alpine"),
            Some((SnapshotServiceType::Postgres, Some("16".to_string())))
        );
        assert_eq!(
            detect_database("bitnami/postgresql:15.4.0"),
            Some((SnapshotServiceType::Postgres, Some("15".to_string())))
        );
        assert_eq!(
            detect_database("redis:latest"),
            Some((SnapshotServiceType::Redis, None))
        );
        assert_eq!(
            detect_database("mariadb:11"),
            Some((SnapshotServiceType::Mysql, Some("11".to_string())))
        );
        assert_eq!(detect_database("traefik/whoami:latest"), None);
        // Registry with port must not be mistaken for a tag
        assert_eq!(detect_database("registry.example.com:5000/team/app"), None);
    }

    #[test]
    fn test_generate_project_plan_full() {
        let importer = KubernetesImporter::new();
        let plan = importer.generate_project_plan(project_snapshot()).unwrap();

        assert_eq!(plan.source, "kubernetes");
        assert_eq!(plan.project.name, "storefront");
        assert_eq!(plan.services.len(), 1);
        assert_eq!(plan.domains.len(), 1);
        assert_eq!(plan.additional_deployments.len(), 1);

        // Steps: project, env vars, deployment, 1 service, 1 domain
        assert_eq!(plan.steps.len(), 5);
        assert!(plan.steps.iter().enumerate().all(|(i, s)| s.order == i + 1));

        // Secrets stay redacted and marked
        let secret = plan
            .deployment
            .env_vars
            .iter()
            .find(|e| e.key == "LAB_SECRET")
            .unwrap();
        assert!(secret.is_secret);
        assert_eq!(secret.value, "***REDACTED***");
        assert_eq!(
            secret.source_description.as_deref(),
            Some("Secret 'shop-secrets'")
        );

        // Cost analysis is attached and shows severe overprovisioning
        let analysis = plan.cost_analysis.as_ref().expect("cost analysis");
        assert_eq!(
            analysis.overprovisioning.verdict,
            temps_import_types::OverprovisioningVerdict::Severe
        );
        assert!(analysis.recommendation.fits_single_node);

        // HPA reported as unsupported feature
        assert!(plan
            .summary
            .unsupported_features
            .iter()
            .any(|f| f.feature.contains("HorizontalPodAutoscaler")));

        // Data warning present because the postgres service has data
        assert!(plan
            .summary
            .critical_warnings
            .iter()
            .any(|w| w.contains("Database data stays in the cluster")));
        assert_eq!(
            plan.summary.overall_risk,
            temps_import_types::RiskLevel::High
        );
    }

    #[test]
    fn test_generate_workload_plan_without_cluster_context() {
        let importer = KubernetesImporter::new();
        let plan = importer.generate_plan(snapshot("api")).unwrap();

        assert!(plan.cost_analysis.is_none());
        assert!(plan.services.is_empty());
        assert!(plan.domains.is_empty());
        assert_eq!(plan.steps.len(), 3);
        assert!(plan
            .metadata
            .warnings
            .iter()
            .any(|w| w.contains("without cost analysis")));
        // Resource limits converted to millicores / MB
        assert_eq!(plan.deployment.resources.cpu_limit, Some(1000));
        assert_eq!(plan.deployment.resources.memory_limit, Some(1024));
        assert_eq!(plan.deployment.resources.memory_request, Some(768));
    }

    /// Regression test: a kubeconfig with `insecure-skip-tls-verify` used to
    /// only produce a server-side `warn!()` log — the user driving the
    /// import over the API never saw it. `KubeClient::skip_tls_verify()`
    /// now flows into the snapshot's `source_metadata` and from there into
    /// `PlanMetadata.warnings`, so it's visible in the plan response.
    #[test]
    fn test_insecure_tls_skip_verify_surfaces_as_plan_warning() {
        let importer = KubernetesImporter::new();
        let mut workload = snapshot("api");
        workload.source_metadata["insecure_tls_skip_verify"] = serde_json::json!(true);
        let plan = importer.generate_plan(workload).unwrap();

        assert!(
            plan.metadata
                .warnings
                .iter()
                .any(|w| w.contains("insecure-skip-tls-verify")),
            "expected an insecure-skip-tls-verify warning, got: {:?}",
            plan.metadata.warnings
        );
    }

    #[test]
    fn test_tls_verified_snapshot_has_no_insecure_tls_warning() {
        let importer = KubernetesImporter::new();
        // The fixture doesn't set insecure_tls_skip_verify, matching a
        // normal kubeconfig that verifies the cluster's CA.
        let plan = importer.generate_plan(snapshot("api")).unwrap();

        assert!(
            !plan
                .metadata
                .warnings
                .iter()
                .any(|w| w.contains("insecure-skip-tls-verify")),
            "a verified TLS connection must not carry the insecure-skip-tls-verify warning"
        );
    }

    #[test]
    fn test_validation_passes_for_clean_snapshot() {
        let importer = KubernetesImporter::new();
        let workload = snapshot("storefront");
        let plan = importer.generate_plan(workload.clone()).unwrap();
        let report = importer.validate(&workload, &plan);
        assert_eq!(report.overall_status, ValidationStatus::Passed);
    }

    #[test]
    fn test_validation_flags_private_registry_and_replicas() {
        let importer = KubernetesImporter::new();
        let mut workload = snapshot("storefront");
        workload.image = Some("registry.internal.corp:5000/team/app:v3".to_string());
        workload.source_metadata["replicas"] = serde_json::json!(3);
        let plan = importer.generate_plan(workload.clone()).unwrap();
        let report = importer.validate(&workload, &plan);
        assert_eq!(report.overall_status, ValidationStatus::PassedWithWarnings);
        assert!(report
            .results
            .iter()
            .any(|r| r.rule_id == "k8s-private-registry" && !r.passed));
        assert!(report
            .results
            .iter()
            .any(|r| r.rule_id == "k8s-multi-replica" && !r.passed));
    }

    #[tokio::test]
    async fn test_execute_dry_run_reports_manual_steps() {
        struct StubProvider {
            db: sea_orm::DatabaseConnection,
        }
        #[async_trait]
        impl ImportServiceProvider for StubProvider {
            fn db(&self) -> &sea_orm::DatabaseConnection {
                &self.db
            }
            fn project_service(&self) -> &dyn std::any::Any {
                &()
            }
            fn deployment_service(&self) -> &dyn std::any::Any {
                &()
            }
            fn git_provider_manager(&self) -> &dyn std::any::Any {
                &()
            }
        }

        let importer = KubernetesImporter::new();
        let plan = importer.generate_project_plan(project_snapshot()).unwrap();
        let provider = StubProvider {
            db: sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        };
        let context = ImportContext {
            session_id: "test-session".to_string(),
            user_id: 1,
            dry_run: true,
            project_name: "storefront".to_string(),
            preset: "docker".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            credentials: ImportCredentials::none(),
            metadata: HashMap::new(),
        };

        let outcome = importer.execute(context, plan, &provider).await.unwrap();
        assert!(outcome.success);
        assert!(outcome.project_id.is_none());
        // One manual service step + one manual domain step
        assert_eq!(outcome.step_results.len(), 2);
        assert!(outcome.step_results.iter().all(|s| s.skipped));
        assert_eq!(outcome.warnings.len(), 2);
    }

    #[test]
    fn test_resolve_credentials_missing() {
        let err = resolve_credentials(&ImportCredentials::none()).unwrap_err();
        assert!(matches!(err, KubernetesImportError::MissingKubeconfig));
    }
}
