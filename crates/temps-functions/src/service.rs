// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The functions service (ADR-055 phase 10).
//!
//! A function is created from an uploaded artifact: a WASM module (stored
//! once per digest under `<data dir>/functions/`) or an OCI image
//! reference. It becomes one `Function` workload on the workload plane with
//! a `ScaleToZero` lifetime, an ingress endpoint row whose target is the
//! activator, and a hostname the proxy hands to the activator. Nothing runs
//! until the first request: the activator wakes the workload through
//! [`crate::FunctionActivationBackend`] and puts it back to sleep after its
//! idle time.
//!
//! Memory: the service keeps nothing per function. The activator keeps one
//! slot and one host entry per function; the plane keeps the rows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use temps_entities::{functions, ingress_endpoints, projects};
use temps_workload::{
    Artifact, EgressProfile, Isolation, Lifetime, NetworkSpec, PortSpec, ResourceAmounts,
    Resources, WorkloadKind, WorkloadRef, WorkloadSpec,
};
use temps_workload_activator::{
    ActivationKey, ActivationKind, ActivationState, Activator, Registration,
};
use temps_workload_ingress::EndpointTarget;
use temps_workload_plane::{ApplyRequest, Owner, SharedPlane, WorkloadPlane};
use tracing::{info, warn};

use crate::error::FunctionError;

/// Owner kind of a function's workload and ingress endpoint.
pub const FUNCTION_OWNER_KIND: &str = "function";
/// Largest module accepted (the WASM driver's default limit).
pub const MAX_WASM_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_IDLE_TIMEOUT_SECS: i32 = 300;
pub const DEFAULT_WAKE_TIMEOUT_SECS: i32 = 30;
const MAX_NAME_LEN: usize = 40;
/// Console page that explains how to make functions available.
pub const SETUP_PATH: &str = "/settings/functions";

/// What a function runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionArtifact {
    /// A WASI preview 1 core module.
    Wasm {
        module: Vec<u8>,
    },
    Oci {
        image: String,
    },
}

impl FunctionArtifact {
    pub fn kind(&self) -> &'static str {
        match self {
            FunctionArtifact::Wasm { .. } => "wasm",
            FunctionArtifact::Oci { .. } => "oci",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateFunction {
    pub name: String,
    pub artifact: FunctionArtifact,
    pub port: Option<u16>,
    pub idle_timeout_seconds: Option<i32>,
    pub wake_timeout_seconds: Option<i32>,
    pub memory_limit_mb: Option<i32>,
}

/// Where function hostnames live (the preview domain).
#[async_trait]
pub trait FunctionDomain: Send + Sync {
    async fn base_domain(&self) -> String;
}

/// The preview domain from platform settings, `localho.st` when unset.
pub struct SettingsDomain {
    config: Arc<temps_config::ConfigService>,
}

impl SettingsDomain {
    pub fn new(config: Arc<temps_config::ConfigService>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl FunctionDomain for SettingsDomain {
    async fn base_domain(&self) -> String {
        match self.config.get_settings().await {
            Ok(settings) if !settings.preview_domain.is_empty() => {
                temps_core::public_base_domain(&settings.preview_domain)
            }
            Ok(_) => "localho.st".to_string(),
            Err(error) => {
                warn!("reading platform settings for a function hostname failed, using localho.st: {error}");
                "localho.st".to_string()
            }
        }
    }
}

/// Whether functions can run here, and why not (Feature Discoverability).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionsCapability {
    pub configured: bool,
    pub wasm: RuntimeAvailability,
    pub oci: RuntimeAvailability,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAvailability {
    pub available: bool,
    pub reason: Option<String>,
}

/// A function and what it is doing now.
#[derive(Debug, Clone)]
pub struct FunctionView {
    pub function: functions::Model,
    /// `asleep`, `waking`, `awake`, `sleeping`, or `unregistered` (not
    /// served by this process).
    pub state: &'static str,
    /// Why it is not serving, from the plane's `Ready` condition.
    pub ready_reason: Option<String>,
    pub ready_message: Option<String>,
}

/// The validated, normalised settings of a new function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Normalized {
    pub name: String,
    pub port: u16,
    pub idle_timeout_seconds: i32,
    pub wake_timeout_seconds: i32,
    pub memory_limit_mb: Option<i32>,
}

pub(crate) fn validate(request: &CreateFunction) -> Result<Normalized, FunctionError> {
    let invalid = |message: String| FunctionError::Validation { message };
    let name = request.name.trim().to_string();
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid(format!(
            "function name must be 1 to {MAX_NAME_LEN} characters, got {}",
            name.len()
        )));
    }
    let starts_with_letter = name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    let allowed = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !starts_with_letter || !allowed || name.ends_with('-') || name.contains("--") {
        return Err(invalid(format!(
            "function name '{name}' must start with a lowercase letter and contain only \
             lowercase letters, digits and single inner hyphens"
        )));
    }
    match &request.artifact {
        FunctionArtifact::Wasm { module } => validate_module(module)?,
        FunctionArtifact::Oci { image } => {
            let image = image.trim();
            if image.is_empty() || image.chars().any(char::is_whitespace) {
                return Err(invalid(format!(
                    "OCI image reference '{image}' must be non-empty and contain no whitespace"
                )));
            }
        }
    }
    let port = request.port.unwrap_or(DEFAULT_PORT);
    if port == 0 {
        return Err(invalid("port must be between 1 and 65535".to_string()));
    }
    let idle = request
        .idle_timeout_seconds
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
    if !(10..=86_400).contains(&idle) {
        return Err(invalid(format!(
            "idle_timeout_seconds must be between 10 and 86400, got {idle}"
        )));
    }
    let wake = request
        .wake_timeout_seconds
        .unwrap_or(DEFAULT_WAKE_TIMEOUT_SECS);
    if !(1..=300).contains(&wake) {
        return Err(invalid(format!(
            "wake_timeout_seconds must be between 1 and 300, got {wake}"
        )));
    }
    if let Some(memory) = request.memory_limit_mb {
        if !(1..=4096).contains(&memory) {
            return Err(invalid(format!(
                "memory_limit_mb must be between 1 and 4096, got {memory}"
            )));
        }
    }
    Ok(Normalized {
        name,
        port,
        idle_timeout_seconds: idle,
        wake_timeout_seconds: wake,
        memory_limit_mb: request.memory_limit_mb,
    })
}

/// A WASI preview 1 core module starts with `\0asm` and version 1. A
/// component (version 0x0d, layer 1) is refused here with the reason, not
/// at the first request.
fn validate_module(module: &[u8]) -> Result<(), FunctionError> {
    let invalid = |message: String| FunctionError::Validation { message };
    if module.is_empty() {
        return Err(invalid("the WASM module is empty".to_string()));
    }
    if module.len() > MAX_WASM_BYTES {
        return Err(invalid(format!(
            "the WASM module is {} bytes; the limit is {MAX_WASM_BYTES}",
            module.len()
        )));
    }
    if module.len() < 8 || &module[0..4] != b"\0asm" {
        return Err(invalid(
            "the upload is not a WASM binary (it does not start with \\0asm)".to_string(),
        ));
    }
    if module[4..8] != [1, 0, 0, 0] {
        return Err(invalid(
            "the upload is a WASM component or an unknown WASM version; functions run WASI \
             preview 1 core modules (compile for wasm32-wasip1)"
                .to_string(),
        ));
    }
    Ok(())
}

/// `sha256:<hex>` of `bytes`, the digest form the WASM driver checks.
pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// The function's hostname: `fn-<name>-<project slug>.<base domain>`.
pub(crate) fn hostname_for(
    name: &str,
    project_slug: &str,
    base_domain: &str,
) -> Result<String, FunctionError> {
    let label = format!("fn-{name}-{project_slug}").to_ascii_lowercase();
    if label.len() > 63 {
        return Err(FunctionError::Validation {
            message: format!(
                "the hostname label '{label}' is {} characters; DNS allows 63. Use a shorter \
                 function name",
                label.len()
            ),
        });
    }
    Ok(format!("{label}.{}", base_domain.trim_matches('.')).to_ascii_lowercase())
}

fn blob_path(dir: &Path, digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    dir.join(format!("{hex}.wasm"))
}

/// The workload a function runs as. Egress is restricted (admission would
/// restrict it anyway: functions are untrusted).
pub(crate) fn function_workload_spec(
    function: &functions::Model,
    artifacts_dir: &Path,
) -> Result<WorkloadSpec, FunctionError> {
    let (artifact, isolation) = match (
        function.artifact_kind.as_str(),
        function.wasm_digest.as_ref(),
        function.image_reference.as_ref(),
    ) {
        ("wasm", Some(digest), _) => (
            Artifact::WasmComponent {
                blob: blob_path(artifacts_dir, digest).display().to_string(),
                digest: digest.clone(),
            },
            Isolation::InHost,
        ),
        ("oci", _, Some(image)) => (Artifact::oci(image.clone()), Isolation::Process),
        (kind, _, _) => {
            return Err(FunctionError::Validation {
                message: format!(
                    "function {} has artifact kind '{kind}' without its artifact",
                    function.id
                ),
            })
        }
    };
    let port = u16::try_from(function.port).map_err(|_| FunctionError::Validation {
        message: format!(
            "function {} has port {} out of range",
            function.id, function.port
        ),
    })?;
    let idle_secs =
        u32::try_from(function.idle_timeout_seconds).map_err(|_| FunctionError::Validation {
            message: format!(
                "function {} has a negative idle timeout {}",
                function.id, function.idle_timeout_seconds
            ),
        })?;
    let limits = function.memory_limit_mb.map(|mb| ResourceAmounts {
        memory_mb: u32::try_from(mb).ok(),
        ..Default::default()
    });
    Ok(WorkloadSpec {
        owner: WorkloadRef::new(WorkloadKind::Function, function.id),
        artifact,
        isolation,
        process: Default::default(),
        env: Default::default(),
        resources: Resources {
            requests: None,
            limits,
        },
        volumes: Vec::new(),
        ports: vec![PortSpec {
            name: "http".to_string(),
            container_port: port,
            protocol: Default::default(),
            app_protocol: Default::default(),
        }],
        network: NetworkSpec {
            egress: Some(EgressProfile::Restricted),
            ..Default::default()
        },
        probes: Default::default(),
        placement: Default::default(),
        lifetime: Lifetime::ScaleToZero { idle_secs },
        security: None,
        labels: [(
            "sh.temps.project_id".to_string(),
            function.project_id.to_string(),
        )]
        .into_iter()
        .collect(),
    })
}

pub(crate) fn activation_key(function_id: i64) -> ActivationKey {
    ActivationKey::new(ActivationKind::Function, function_id)
}

fn registration(function: &functions::Model) -> Registration {
    Registration {
        idle_secs: u32::try_from(function.idle_timeout_seconds).unwrap_or(300),
        wake_timeout_secs: u32::try_from(function.wake_timeout_seconds).unwrap_or(30),
        awake: false,
    }
}

pub struct FunctionService {
    db: Arc<DatabaseConnection>,
    plane: Arc<SharedPlane>,
    activator: Arc<Activator>,
    artifacts_dir: PathBuf,
    domain: Arc<dyn FunctionDomain>,
}

impl FunctionService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        plane: Arc<SharedPlane>,
        activator: Arc<Activator>,
        artifacts_dir: PathBuf,
        domain: Arc<dyn FunctionDomain>,
    ) -> Self {
        Self {
            db,
            plane,
            activator,
            artifacts_dir,
            domain,
        }
    }

    fn plane(&self) -> Result<Arc<WorkloadPlane>, FunctionError> {
        self.plane
            .get()
            .ok_or_else(|| FunctionError::PlaneUnavailable {
                reason: "the workload plane is not running in this process (it starts with the \
                     deployments plugin and needs the audit log); functions are stored and \
                     served only where it runs"
                    .to_string(),
            })
    }

    /// A sample function of `kind`, to ask the plane whether it could run it.
    fn sample(&self, kind: &str) -> functions::Model {
        let now = Utc::now();
        functions::Model {
            id: 0,
            project_id: 0,
            name: "sample".to_string(),
            artifact_kind: kind.to_string(),
            image_reference: (kind == "oci").then(|| "sample:latest".to_string()),
            wasm_digest: (kind == "wasm").then(|| digest_of(b"")),
            wasm_size_bytes: None,
            port: i32::from(DEFAULT_PORT),
            idle_timeout_seconds: DEFAULT_IDLE_TIMEOUT_SECS,
            wake_timeout_seconds: DEFAULT_WAKE_TIMEOUT_SECS,
            memory_limit_mb: None,
            hostname: String::new(),
            workload_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn runtime_for(&self, plane: &WorkloadPlane, kind: &str) -> RuntimeAvailability {
        let spec = match function_workload_spec(&self.sample(kind), &self.artifacts_dir) {
            Ok(spec) => spec,
            Err(error) => {
                return RuntimeAvailability {
                    available: false,
                    reason: Some(error.to_string()),
                }
            }
        };
        match plane.local_driver_for(&spec) {
            Ok(_) => RuntimeAvailability {
                available: true,
                reason: None,
            },
            Err(reason) => RuntimeAvailability {
                available: false,
                reason: Some(reason),
            },
        }
    }

    /// Whether functions can be created and served by this installation.
    pub fn capability(&self) -> FunctionsCapability {
        let plane = match self.plane() {
            Ok(plane) => plane,
            Err(error) => {
                let unavailable = RuntimeAvailability {
                    available: false,
                    reason: Some(error.to_string()),
                };
                return FunctionsCapability {
                    configured: false,
                    wasm: unavailable.clone(),
                    oci: unavailable,
                    reason: Some(error.to_string()),
                    setup_path: Some(SETUP_PATH.to_string()),
                };
            }
        };
        let wasm = self.runtime_for(&plane, "wasm");
        let oci = self.runtime_for(&plane, "oci");
        let configured = wasm.available || oci.available;
        let reason = (!configured).then(|| {
            "no runtime on the control plane can run a function: the WASM runtime is built in \
             only with `--features wasm`, and an OCI function needs a runtime that confines \
             its egress"
                .to_string()
        });
        FunctionsCapability {
            configured,
            wasm,
            oci,
            reason,
            setup_path: (!configured).then(|| SETUP_PATH.to_string()),
        }
    }

    /// Store the module under its digest (once), atomically.
    async fn store_module(&self, module: &[u8]) -> Result<(String, PathBuf), FunctionError> {
        let digest = digest_of(module);
        let path = blob_path(&self.artifacts_dir, &digest);
        let storage = |path: &Path, reason: String| FunctionError::Storage {
            path: path.display().to_string(),
            reason,
        };
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok((digest, path));
        }
        tokio::fs::create_dir_all(&self.artifacts_dir)
            .await
            .map_err(|e| storage(&self.artifacts_dir, format!("creating the directory: {e}")))?;
        let partial = path.with_extension(format!("wasm.partial-{}", std::process::id()));
        tokio::fs::write(&partial, module)
            .await
            .map_err(|e| storage(&partial, format!("writing the module: {e}")))?;
        tokio::fs::rename(&partial, &path)
            .await
            .map_err(|e| storage(&path, format!("moving the module into place: {e}")))?;
        Ok((digest, path))
    }

    /// Remove the module of `digest` when no function uses it any more.
    async fn collect_module(&self, digest: &str) {
        let in_use = functions::Entity::find()
            .filter(functions::Column::WasmDigest.eq(digest))
            .count(self.db.as_ref())
            .await;
        match in_use {
            Ok(0) => {
                let path = blob_path(&self.artifacts_dir, digest);
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        warn!(path = %path.display(), "removing an unused function module failed: {error}");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => warn!(
                digest,
                "checking whether a function module is still used failed: {error}"
            ),
        }
    }

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateFunction,
    ) -> Result<functions::Model, FunctionError> {
        let normalized = validate(&request)?;
        let plane = self.plane()?;
        let kind = request.artifact.kind();
        let runtime = self.runtime_for(&plane, kind);
        if !runtime.available {
            return Err(FunctionError::NoRuntime {
                artifact_kind: kind,
                available: runtime.reason.unwrap_or_else(|| "none".to_string()),
                hint: match request.artifact {
                    FunctionArtifact::Wasm { .. } => {
                        "WASM functions need a temps binary built with `--features wasm`"
                    }
                    FunctionArtifact::Oci { .. } => {
                        "upload a WASM module instead, or wait for a runtime that confines \
                         container egress"
                    }
                },
            });
        }
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(FunctionError::db("reading the function's project"))?
            .ok_or(FunctionError::ProjectNotFound { project_id })?;
        let duplicate = functions::Entity::find()
            .filter(functions::Column::ProjectId.eq(project_id))
            .filter(functions::Column::Name.eq(normalized.name.clone()))
            .count(self.db.as_ref())
            .await
            .map_err(FunctionError::db(
                "checking for a function of the same name",
            ))?;
        if duplicate > 0 {
            return Err(FunctionError::DuplicateName {
                project_id,
                name: normalized.name,
            });
        }
        let base_domain = self.domain.base_domain().await;
        let hostname = hostname_for(&normalized.name, &project.slug, &base_domain)?;

        let (image_reference, wasm_digest, wasm_size_bytes) = match &request.artifact {
            FunctionArtifact::Wasm { module } => {
                let (digest, _) = self.store_module(module).await?;
                (None, Some(digest), i64::try_from(module.len()).ok())
            }
            FunctionArtifact::Oci { image } => (Some(image.trim().to_string()), None, None),
        };
        let now = Utc::now();
        let inserted = functions::ActiveModel {
            project_id: Set(project_id),
            name: Set(normalized.name.clone()),
            artifact_kind: Set(kind.to_string()),
            image_reference: Set(image_reference),
            wasm_digest: Set(wasm_digest.clone()),
            wasm_size_bytes: Set(wasm_size_bytes),
            port: Set(i32::from(normalized.port)),
            idle_timeout_seconds: Set(normalized.idle_timeout_seconds),
            wake_timeout_seconds: Set(normalized.wake_timeout_seconds),
            memory_limit_mb: Set(normalized.memory_limit_mb),
            hostname: Set(hostname.clone()),
            workload_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await;
        let function = match inserted {
            Ok(function) => function,
            Err(error) => {
                if let Some(digest) = &wasm_digest {
                    self.collect_module(digest).await;
                }
                // A concurrent create of the same name (or hostname) loses
                // on the unique index.
                let message = error.to_string();
                if message.contains("functions_project_name_unique")
                    || message.contains("functions_hostname_unique")
                {
                    return Err(FunctionError::DuplicateName {
                        project_id,
                        name: normalized.name,
                    });
                }
                return Err(FunctionError::Database {
                    operation: "inserting the function",
                    source: error,
                });
            }
        };

        match self.attach_workload(&plane, &function, &project.slug).await {
            Ok(function) => Ok(function),
            Err(error) => {
                // Nothing half-made is left: the row goes, and the module if
                // no other function uses it.
                if let Err(cleanup) = functions::Entity::delete_by_id(function.id)
                    .exec(self.db.as_ref())
                    .await
                {
                    warn!(
                        function_id = function.id,
                        "removing a function whose workload failed: {cleanup}"
                    );
                }
                if let Some(digest) = &wasm_digest {
                    self.collect_module(digest).await;
                }
                Err(error)
            }
        }
    }

    /// Apply the function's workload, record it with its ingress endpoint,
    /// and register it with the activator.
    async fn attach_workload(
        &self,
        plane: &WorkloadPlane,
        function: &functions::Model,
        project_slug: &str,
    ) -> Result<functions::Model, FunctionError> {
        let spec = function_workload_spec(function, &self.artifacts_dir)?;
        let workload_id = plane
            .apply(ApplyRequest {
                owner: Owner::new(FUNCTION_OWNER_KIND, function.id),
                name: format!("fn-{}", function.id),
                spec,
                project_slug: Some(project_slug.to_string()),
                finalizers: Vec::new(),
            })
            .await
            .map_err(|source| FunctionError::Plane {
                function_id: function.id,
                name: function.name.clone(),
                operation: "accept its workload",
                source: Box::new(source),
            })?;
        let recorded = async {
            let txn = self
                .db
                .begin()
                .await
                .map_err(FunctionError::db("starting the function transaction"))?;
            let mut active: functions::ActiveModel = function.clone().into();
            active.workload_id = Set(Some(workload_id));
            active.updated_at = Set(Utc::now());
            let updated = active
                .update(&txn)
                .await
                .map_err(FunctionError::db("recording the function's workload"))?;
            ingress_endpoints::ActiveModel {
                owner_kind: Set(FUNCTION_OWNER_KIND.to_string()),
                owner_id: Set(function.id),
                environment_id: Set(None),
                sandbox_id: Set(None),
                workload_id: Set(Some(workload_id)),
                target: Set(ingress_endpoints::StoredTarget(EndpointTarget::Activator {
                    workload_id,
                })),
                path_prefix: Set(None),
                protocol: Set("http".to_string()),
                ..Default::default()
            }
            .insert(&txn)
            .await
            .map_err(FunctionError::db(
                "creating the function's ingress endpoint",
            ))?;
            txn.commit()
                .await
                .map_err(FunctionError::db("committing the function"))?;
            Ok::<_, FunctionError>(updated)
        }
        .await;
        let updated = match recorded {
            Ok(updated) => updated,
            Err(error) => {
                if let Err(cleanup) = plane
                    .delete(workload_id, "Deleted", "the function could not be recorded")
                    .await
                {
                    warn!(
                        workload_id,
                        "removing the workload of an unrecorded function failed: {cleanup}"
                    );
                }
                return Err(error);
            }
        };
        let key = activation_key(updated.id);
        self.activator.register(key, registration(&updated));
        self.activator.add_host(&updated.hostname, key);
        info!(
            function_id = updated.id,
            project_id = updated.project_id,
            workload_id,
            hostname = %updated.hostname,
            kind = %updated.artifact_kind,
            "function created; it sleeps until its first request"
        );
        Ok(updated)
    }

    async fn find(
        &self,
        project_id: i32,
        function_id: i64,
    ) -> Result<functions::Model, FunctionError> {
        functions::Entity::find_by_id(function_id)
            .filter(functions::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(FunctionError::db("reading the function"))?
            .ok_or(FunctionError::NotFound {
                project_id,
                function_id,
            })
    }

    /// What the function is doing: the activator's state, and the plane's
    /// reason when it is not serving.
    async fn view(&self, function: functions::Model) -> FunctionView {
        let state = self
            .activator
            .state(activation_key(function.id))
            .map(ActivationState::as_str)
            .unwrap_or("unregistered");
        let mut view = FunctionView {
            function,
            state,
            ready_reason: None,
            ready_message: None,
        };
        let (Some(workload_id), Ok(plane)) = (view.function.workload_id, self.plane()) else {
            return view;
        };
        match plane.get(workload_id).await {
            Ok(workload) => {
                if let Some(ready) = workload
                    .status
                    .condition(temps_workload::ConditionKind::Ready)
                {
                    view.ready_reason = Some(ready.reason.clone());
                    view.ready_message = Some(ready.message.clone());
                }
            }
            Err(error) => {
                view.ready_reason = Some("WorkloadUnavailable".to_string());
                view.ready_message = Some(error.to_string());
            }
        }
        view
    }

    pub async fn get(
        &self,
        project_id: i32,
        function_id: i64,
    ) -> Result<FunctionView, FunctionError> {
        let function = self.find(project_id, function_id).await?;
        Ok(self.view(function).await)
    }

    /// Newest first. `page` is 1-based; `page_size` defaults to 20, max 100.
    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<FunctionView>, u64), FunctionError> {
        let (page, page_size) = normalize_pagination(page, page_size);
        let paginator = functions::Entity::find()
            .filter(functions::Column::ProjectId.eq(project_id))
            .order_by_desc(functions::Column::CreatedAt)
            .order_by_desc(functions::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(FunctionError::db("counting functions"))?;
        let rows = paginator
            .fetch_page(page - 1)
            .await
            .map_err(FunctionError::db("listing functions"))?;
        // State comes from memory; the plane status is not read per row (a
        // list stays one query). The detail view reads it.
        let views = rows
            .into_iter()
            .map(|function| FunctionView {
                state: self
                    .activator
                    .state(activation_key(function.id))
                    .map(ActivationState::as_str)
                    .unwrap_or("unregistered"),
                function,
                ready_reason: None,
                ready_message: None,
            })
            .collect();
        Ok((views, total))
    }

    pub async fn delete(
        &self,
        project_id: i32,
        function_id: i64,
    ) -> Result<functions::Model, FunctionError> {
        let function = self.find(project_id, function_id).await?;
        let key = activation_key(function.id);
        if let Some(workload_id) = function.workload_id {
            let plane = self.plane()?;
            match plane
                .delete(workload_id, "Deleted", "the function was deleted")
                .await
            {
                Ok(()) | Err(temps_workload_plane::PlaneError::NotFound { .. }) => {}
                Err(source) => {
                    return Err(FunctionError::Plane {
                        function_id: function.id,
                        name: function.name.clone(),
                        operation: "delete its workload",
                        source: Box::new(source),
                    })
                }
            }
        }
        let txn = self
            .db
            .begin()
            .await
            .map_err(FunctionError::db("starting the delete transaction"))?;
        ingress_endpoints::Entity::delete_many()
            .filter(ingress_endpoints::Column::OwnerKind.eq(FUNCTION_OWNER_KIND))
            .filter(ingress_endpoints::Column::OwnerId.eq(function.id))
            .exec(&txn)
            .await
            .map_err(FunctionError::db(
                "deleting the function's ingress endpoint",
            ))?;
        functions::Entity::delete_by_id(function.id)
            .exec(&txn)
            .await
            .map_err(FunctionError::db("deleting the function"))?;
        txn.commit()
            .await
            .map_err(FunctionError::db("committing the function delete"))?;
        self.activator.unregister(key);
        if let Some(digest) = &function.wasm_digest {
            self.collect_module(digest).await;
        }
        info!(function_id = function.id, project_id, "function deleted");
        Ok(function)
    }

    /// Start the function now (the console's "wake" button), through the
    /// activator so it is never woken twice at once.
    pub async fn wake(
        &self,
        project_id: i32,
        function_id: i64,
    ) -> Result<FunctionView, FunctionError> {
        let function = self.find(project_id, function_id).await?;
        self.ensure_registered(&function)?;
        self.activator
            .activate(activation_key(function.id))
            .await
            .map_err(|source| FunctionError::Activation {
                function_id: function.id,
                name: function.name.clone(),
                source,
            })?;
        Ok(self.view(function).await)
    }

    /// Stop the function now, whatever its idle time.
    pub async fn sleep(
        &self,
        project_id: i32,
        function_id: i64,
    ) -> Result<(FunctionView, bool), FunctionError> {
        let function = self.find(project_id, function_id).await?;
        self.ensure_registered(&function)?;
        let slept = self
            .activator
            .sleep_now(activation_key(function.id))
            .await
            .map_err(|source| FunctionError::Activation {
                function_id: function.id,
                name: function.name.clone(),
                source,
            })?;
        Ok((self.view(function).await, slept))
    }

    fn ensure_registered(&self, function: &functions::Model) -> Result<(), FunctionError> {
        if function.workload_id.is_none() {
            return Err(FunctionError::PlaneUnavailable {
                reason: format!(
                    "function '{}' has no workload yet (it was stored while the workload plane \
                     was not running)",
                    function.name
                ),
            });
        }
        let key = activation_key(function.id);
        if !self.activator.is_registered(key) {
            self.activator.register(key, registration(function));
            self.activator.add_host(&function.hostname, key);
        }
        Ok(())
    }

    /// Register every function that has a workload with the activator
    /// (asleep), replacing what was registered. Run once at startup: it
    /// reads every function row, which is O(functions) once, never per
    /// request.
    pub async fn load_into_activator(&self) -> Result<usize, FunctionError> {
        let rows = functions::Entity::find()
            .filter(functions::Column::WorkloadId.is_not_null())
            .all(self.db.as_ref())
            .await
            .map_err(FunctionError::db("loading functions for the activator"))?;
        self.activator.replace_kind(
            ActivationKind::Function,
            rows.iter()
                .map(|function| (function.id, registration(function))),
        );
        self.activator.replace_hosts(
            ActivationKind::Function,
            rows.iter()
                .map(|function| (function.hostname.clone(), function.id)),
        );
        Ok(rows.len())
    }
}

pub fn normalize_pagination(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(20).clamp(1, 100);
    (page, page_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// A minimal WASI preview 1 core module header plus an empty body.
    fn core_module() -> Vec<u8> {
        b"\0asm\x01\x00\x00\x00".to_vec()
    }

    fn request(name: &str) -> CreateFunction {
        CreateFunction {
            name: name.to_string(),
            artifact: FunctionArtifact::Wasm {
                module: core_module(),
            },
            port: None,
            idle_timeout_seconds: None,
            wake_timeout_seconds: None,
            memory_limit_mb: None,
        }
    }

    struct FixedDomain;

    #[async_trait]
    impl FunctionDomain for FixedDomain {
        async fn base_domain(&self) -> String {
            "fn.example.test".to_string()
        }
    }

    fn service(db: DatabaseConnection, dir: &Path) -> FunctionService {
        FunctionService::new(
            Arc::new(db),
            Arc::new(SharedPlane::new()),
            Arc::new(Activator::default()),
            dir.to_path_buf(),
            Arc::new(FixedDomain),
        )
    }

    #[test]
    fn validation_normalises_defaults_and_refuses_bad_input() {
        let ok = validate(&request("hello-world")).expect("valid");
        assert_eq!(ok.port, DEFAULT_PORT);
        assert_eq!(ok.idle_timeout_seconds, DEFAULT_IDLE_TIMEOUT_SECS);

        let cases: Vec<(CreateFunction, &str)> = vec![
            (request(""), "1 to 40"),
            (request("Hello"), "lowercase letter"),
            (request("9lives"), "lowercase letter"),
            (request("a--b"), "single inner hyphens"),
            (request("trailing-"), "single inner hyphens"),
            (
                CreateFunction {
                    artifact: FunctionArtifact::Wasm {
                        module: b"not wasm".to_vec(),
                    },
                    ..request("a")
                },
                "\\0asm",
            ),
            (
                CreateFunction {
                    artifact: FunctionArtifact::Wasm {
                        module: b"\0asm\x0d\x00\x01\x00".to_vec(),
                    },
                    ..request("a")
                },
                "component",
            ),
            (
                CreateFunction {
                    artifact: FunctionArtifact::Wasm { module: Vec::new() },
                    ..request("a")
                },
                "empty",
            ),
            (
                CreateFunction {
                    artifact: FunctionArtifact::Oci {
                        image: "has space:1".to_string(),
                    },
                    ..request("a")
                },
                "whitespace",
            ),
            (
                CreateFunction {
                    port: Some(0),
                    ..request("a")
                },
                "port",
            ),
            (
                CreateFunction {
                    idle_timeout_seconds: Some(5),
                    ..request("a")
                },
                "idle_timeout_seconds",
            ),
            (
                CreateFunction {
                    wake_timeout_seconds: Some(301),
                    ..request("a")
                },
                "wake_timeout_seconds",
            ),
            (
                CreateFunction {
                    memory_limit_mb: Some(0),
                    ..request("a")
                },
                "memory_limit_mb",
            ),
        ];
        for (input, expected) in cases {
            let name = input.name.clone();
            let error = validate(&input).expect_err("refused");
            assert!(
                matches!(&error, FunctionError::Validation { message } if message.contains(expected)),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn hostnames_are_one_dns_label_under_the_base_domain() {
        assert_eq!(
            hostname_for("api", "shop", "fn.example.test.").expect("ok"),
            "fn-api-shop.fn.example.test"
        );
        let long = "a".repeat(40);
        let error = hostname_for(&long, &"b".repeat(30), "example.test").expect_err("too long");
        assert!(error.to_string().contains("63"), "{error}");
    }

    #[test]
    fn a_wasm_function_runs_in_host_with_restricted_egress_and_its_limits() {
        let now = Utc::now();
        let function = functions::Model {
            id: 7,
            project_id: 3,
            name: "api".to_string(),
            artifact_kind: "wasm".to_string(),
            image_reference: None,
            wasm_digest: Some("sha256:abcd".to_string()),
            wasm_size_bytes: Some(8),
            port: 8080,
            idle_timeout_seconds: 120,
            wake_timeout_seconds: 10,
            memory_limit_mb: Some(64),
            hostname: "fn-api-shop.example.test".to_string(),
            workload_id: None,
            created_at: now,
            updated_at: now,
        };
        let spec = function_workload_spec(&function, Path::new("/data/functions")).expect("spec");
        assert_eq!(spec.owner.kind, WorkloadKind::Function);
        assert_eq!(spec.isolation, Isolation::InHost);
        assert_eq!(
            spec.artifact,
            Artifact::WasmComponent {
                blob: "/data/functions/abcd.wasm".to_string(),
                digest: "sha256:abcd".to_string(),
            }
        );
        assert_eq!(spec.lifetime, Lifetime::ScaleToZero { idle_secs: 120 });
        assert_eq!(spec.network.egress, Some(EgressProfile::Restricted));
        assert_eq!(spec.ports.len(), 1);
        assert_eq!(
            spec.resources.limits.and_then(|limits| limits.memory_mb),
            Some(64)
        );

        let broken = functions::Model {
            wasm_digest: None,
            ..function
        };
        assert!(matches!(
            function_workload_spec(&broken, Path::new("/d")),
            Err(FunctionError::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn without_a_plane_create_is_refused_and_the_capability_says_why() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service(db, dir.path());
        let capability = service.capability();
        assert!(!capability.configured);
        assert_eq!(capability.setup_path.as_deref(), Some(SETUP_PATH));
        assert!(capability
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("workload plane is not running")));

        let error = service
            .create(1, request("api"))
            .await
            .expect_err("no plane");
        assert!(
            matches!(error, FunctionError::PlaneUnavailable { .. }),
            "{error}"
        );
        // Validation still comes first.
        let error = service
            .create(1, request("Bad"))
            .await
            .expect_err("invalid");
        assert!(matches!(error, FunctionError::Validation { .. }), "{error}");
    }

    #[tokio::test]
    async fn get_of_a_missing_function_is_not_found_with_both_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<functions::Model>::new()])
            .into_connection();
        let service = service(db, dir.path());
        let error = service.get(4, 99).await.expect_err("missing");
        assert!(
            matches!(
                error,
                FunctionError::NotFound {
                    project_id: 4,
                    function_id: 99
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("99") && error.to_string().contains('4'));
    }

    #[tokio::test]
    async fn modules_are_stored_once_by_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service(db, &dir.path().join("functions"));
        let module = core_module();
        let (digest, path) = service.store_module(&module).await.expect("stored");
        assert_eq!(digest, digest_of(&module));
        assert_eq!(tokio::fs::read(&path).await.expect("read"), module);
        let (again, same) = service.store_module(&module).await.expect("stored again");
        assert_eq!((again, same), (digest, path));
    }

    #[test]
    fn pagination_defaults_and_caps() {
        assert_eq!(normalize_pagination(None, None), (1, 20));
        assert_eq!(normalize_pagination(Some(0), Some(500)), (1, 100));
        assert_eq!(normalize_pagination(Some(3), Some(0)), (3, 1));
    }
}

/// End to end on a real database and the real WASM driver: a function
/// created from a module assembled from WAT is asleep, the activator wakes
/// it on the first request (once, however many arrive together), it
/// serves HTTP, it is put back to sleep, and deleting it removes its
/// workload, endpoint, hostname and blob. Skips without Docker.
#[cfg(test)]
mod wasm_end_to_end {
    use super::*;
    use std::net::SocketAddr;
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_workload_activator::ActivatorConfig;
    use temps_workload_plane::{NoEnvSource, NodeDriver, PlaneConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const HEADER: &str = "Status: 200\nContent-Type: text/plain\n\nhello from a function";

    fn hello_module() -> Vec<u8> {
        let wat = format!(
            r#"(module
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{header}")
  (func (export "_start")
    (i32.store (i32.const 0) (i32.const 1024))
    (i32.store (i32.const 4) (i32.const {len}))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))"#,
            header = HEADER.replace('\n', "\\n"),
            len = HEADER.len()
        );
        wat::parse_str(wat).expect("the module assembles")
    }

    struct FixedDomain;

    #[async_trait]
    impl FunctionDomain for FixedDomain {
        async fn base_domain(&self) -> String {
            "fn.example.test".to_string()
        }
    }

    struct NoAudit;

    #[async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    async fn get(address: &str) -> (u16, String) {
        let address: SocketAddr = address.parse().expect("an ip:port");
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("the function accepts a connection");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: fn.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("sent");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    #[tokio::test]
    async fn a_wasm_function_is_created_woken_once_served_slept_and_deleted() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                let text = format!("{error:#}");
                if is_container_runtime_unavailable(&text) {
                    println!("Docker not available, skipping: {text}");
                    return;
                }
                panic!("test database could not be created: {text}");
            }
        };
        let db = test_db.db.clone();
        let project = projects::ActiveModel {
            name: Set("Functions".to_string()),
            slug: Set("shop".to_string()),
            repo_owner: Set("example".to_string()),
            repo_name: Set("shop".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            main_branch: Set("main".to_string()),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("project inserted");

        let wasm = temps_workload_wasm::WasmDriver::new(Default::default()).expect("wasm driver");
        let plane = Arc::new(
            WorkloadPlane::new(
                db.clone(),
                NodeDriver(None),
                Arc::new(NoEnvSource),
                Arc::new(NoAudit),
                PlaneConfig::new("fnt"),
            )
            .with_local_driver(Arc::new(wasm)),
        );
        let shared = Arc::new(SharedPlane::new());
        assert!(shared.set(plane));
        let activator = Arc::new(Activator::new(ActivatorConfig::default()));
        activator.set_backend(
            ActivationKind::Function,
            Arc::new(crate::backend::FunctionActivationBackend::new(
                db.clone(),
                shared.clone(),
            )),
        );
        let artifacts = tempfile::tempdir().expect("tempdir");
        let service = FunctionService::new(
            db.clone(),
            shared,
            activator.clone(),
            artifacts.path().to_path_buf(),
            Arc::new(FixedDomain),
        );
        assert!(service.capability().wasm.available);

        let created = service
            .create(
                project.id,
                CreateFunction {
                    name: "hello".to_string(),
                    artifact: FunctionArtifact::Wasm {
                        module: hello_module(),
                    },
                    port: None,
                    idle_timeout_seconds: Some(60),
                    wake_timeout_seconds: None,
                    memory_limit_mb: None,
                },
            )
            .await
            .expect("created");
        let function = created;
        assert_eq!(function.hostname, "fn-hello-shop.fn.example.test");
        assert!(function.workload_id.is_some());
        let key = activation_key(function.id);
        assert_eq!(activator.key_for_host(&function.hostname), Some(key));
        assert_eq!(activator.state(key), Some(ActivationState::Asleep));
        let blob = blob_path(
            artifacts.path(),
            function.wasm_digest.as_deref().expect("a digest"),
        );
        assert!(blob.exists());

        // Concurrent first requests share one wake.
        let (a, b, c) = tokio::join!(
            activator.activate(key),
            activator.activate(key),
            activator.activate(key)
        );
        let a = a.expect("woken");
        let (b, c) = (b.expect("woken"), c.expect("woken"));
        assert_eq!(activator.metrics().wakes, 1, "one wake for three requests");
        let address = a.address.expect("an address");
        assert_eq!(b.address.as_deref(), Some(&*address));
        assert_eq!(c.address.as_deref(), Some(&*address));
        let (status, body) = get(&address).await;
        assert_eq!((status, body.as_str()), (200, "hello from a function"));

        // Idle past its timeout: the sweep puts it to sleep.
        let swept = activator
            .sweep_at(activator.last_activity(key).expect("touched") + 61)
            .await;
        assert_eq!(swept.slept, vec![key]);
        assert!(
            tokio::net::TcpStream::connect(address.as_str())
                .await
                .is_err(),
            "a sleeping function no longer listens"
        );

        // The next request wakes it again.
        let again = activator.activate(key).await.expect("woken again");
        let (status, _) = get(&again.address.expect("an address")).await;
        assert_eq!(status, 200);

        let deleted = service
            .delete(project.id, function.id)
            .await
            .expect("deleted");
        assert_eq!(deleted.id, function.id);
        assert!(activator.key_for_host(&function.hostname).is_none());
        assert!(!activator.is_registered(key));
        assert!(!blob.exists(), "an unused module is removed");
        assert!(matches!(
            service.get(project.id, function.id).await,
            Err(FunctionError::NotFound { .. })
        ));
    }
}
