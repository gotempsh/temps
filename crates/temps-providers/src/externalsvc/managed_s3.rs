// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed S3-compatible backend contract.
//!
//! A Temps-managed `s3` / `blob` service is one *contract* — "an S3 endpoint,
//! per-environment credentials scoped to per-environment buckets, and a stable
//! set of app-facing environment variables" — implemented by interchangeable
//! storage engines:
//!
//! | backend  | status                       | implementation                  |
//! |----------|------------------------------|---------------------------------|
//! | `rustfs` | default                      | [`super::rustfs::RustfsService`] |
//! | `garage` | optional                     | [`super::garage::GarageService`] |
//! | `minio`  | legacy, existing services only | [`super::s3::S3Service`]       |
//!
//! Everything engine-independent lives here so that adding a further backend
//! is one new [`ManagedS3Backend`] implementation plus one arm in each
//! dispatch `match` (the compiler finds them all, there is no catch-all):
//!
//! * parsing and validating the `backend` / `buckets` service parameters,
//! * the naming scheme for per-environment buckets,
//! * deterministic per-environment credentials ([`derive_env_credentials`]),
//! * rendering the app-facing environment ([`render_runtime_env`]),
//! * the S3 mirror used for backup, restore and backend-to-backend migration
//!   ([`super::s3_mirror`]).
//!
//! Garage runs as an ordinary container next to the RustFS ones and is
//! administered in-process over its admin API, so no out-of-process provider
//! is involved.

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use utoipa::ToSchema;

use super::{HealthProbeResult, ServiceConfig};

pub const DEFAULT_MANAGED_S3_BACKEND: ManagedS3BackendKind = ManagedS3BackendKind::Rustfs;

/// Most buckets one service may declare. Every linked environment gets that
/// many physical buckets, so the bound is on the multiplier.
pub const MAX_MANAGED_S3_BUCKETS: usize = 16;
/// Shortest and longest *logical* bucket name. The physical name is prefixed
/// with the project/environment scope and must fit S3's 63-byte limit, which
/// [`physical_bucket_name`] guarantees by hashing when it does not.
pub const MIN_LOGICAL_BUCKET_LEN: usize = 3;
pub const MAX_LOGICAL_BUCKET_LEN: usize = 32;
/// S3's maximum bucket-name length.
pub const MAX_PHYSICAL_BUCKET_LEN: usize = 63;

/// Everything that can go wrong in the managed-S3 layer. Every variant names
/// the backend / service / bucket involved so a log line is enough to act on.
#[derive(Debug, thiserror::Error)]
pub enum ManagedS3Error {
    #[error(
        "unsupported managed S3 backend '{backend}'; supported backends are rustfs (default) and garage"
    )]
    UnsupportedBackend { backend: String },

    #[error("managed S3 backend must be a string (rustfs or garage), got {found}")]
    BackendNotAString { found: String },

    #[error(
        "managed S3 backend 'minio' is deprecated and cannot be used for new services; use the default 'rustfs' backend or 'garage'"
    )]
    DeprecatedBackend,

    #[error("managed S3 'buckets' must be a list of bucket names (or a comma-separated string), got {found}")]
    BucketsNotAList { found: String },

    #[error("invalid bucket name '{bucket}': {reason}")]
    InvalidBucketName { bucket: String, reason: String },

    #[error("a service may declare at most {max} buckets, got {count}")]
    TooManyBuckets { count: usize, max: usize },

    #[error("{backend} service '{service}' has an invalid configuration: {reason}")]
    InvalidConfig {
        backend: &'static str,
        service: String,
        reason: String,
    },

    #[error("{backend} service '{service}': {operation} failed: {reason}")]
    Admin {
        backend: &'static str,
        service: String,
        operation: String,
        reason: String,
    },

    #[error(
        "{backend} service '{service}' did not become ready within {waited_secs}s: {last_error}"
    )]
    NotReady {
        backend: &'static str,
        service: String,
        waited_secs: u64,
        last_error: String,
    },

    #[error("Docker {operation} for {backend} service '{service}' failed: {reason}")]
    Docker {
        backend: &'static str,
        service: String,
        operation: String,
        reason: String,
    },

    #[error("S3 mirror for service '{service}' failed during {operation}: {reason}")]
    Mirror {
        service: String,
        operation: String,
        reason: String,
    },

    #[error("{operation} is not supported for {backend} service '{service}': {reason}")]
    Unsupported {
        backend: &'static str,
        service: String,
        operation: &'static str,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ManagedS3BackendKind {
    Rustfs,
    Garage,
    Minio,
}

impl ManagedS3BackendKind {
    pub fn parse(value: &str) -> Result<Self, ManagedS3Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "rustfs" => Ok(Self::Rustfs),
            "garage" => Ok(Self::Garage),
            "minio" => Ok(Self::Minio),
            other => Err(ManagedS3Error::UnsupportedBackend {
                backend: other.to_string(),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rustfs => "rustfs",
            Self::Garage => "garage",
            Self::Minio => "minio",
        }
    }

    /// Whether a *new* service may be created on this backend. MinIO is kept
    /// only so existing services keep working.
    pub fn is_creatable(self) -> bool {
        match self {
            Self::Rustfs | Self::Garage => true,
            Self::Minio => false,
        }
    }
}

impl std::fmt::Display for ManagedS3BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3BackendSelection {
    #[serde(default = "default_backend_kind")]
    pub backend: ManagedS3BackendKind,
    /// Logical bucket names, normalized (lower-case, deduplicated, validated).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<String>,
}

impl Default for ManagedS3BackendSelection {
    fn default() -> Self {
        Self {
            backend: DEFAULT_MANAGED_S3_BACKEND,
            buckets: Vec::new(),
        }
    }
}

fn default_backend_kind() -> ManagedS3BackendKind {
    DEFAULT_MANAGED_S3_BACKEND
}

impl ManagedS3BackendSelection {
    pub fn from_parameters(parameters: &serde_json::Value) -> Result<Self, ManagedS3Error> {
        // Treat absent/null as the default, parse strings, and reject any
        // present-but-non-string value rather than silently defaulting.
        let backend = match parameters.get("backend") {
            None | Some(serde_json::Value::Null) => DEFAULT_MANAGED_S3_BACKEND,
            Some(serde_json::Value::String(value)) => ManagedS3BackendKind::parse(value)?,
            Some(other) => {
                return Err(ManagedS3Error::BackendNotAString {
                    found: other.to_string(),
                })
            }
        };

        let buckets = parse_bucket_parameter(parameters.get("buckets"))?;

        Ok(Self { backend, buckets })
    }

    /// Record the validated selection in a service's parameters, replacing
    /// whatever spelling the request used (`Garage`, `a, B`, a flat string…).
    pub fn write_into_parameters(&self, parameters: &mut HashMap<String, serde_json::Value>) {
        parameters.insert(
            "backend".to_string(),
            serde_json::Value::String(self.backend.as_str().to_string()),
        );
        if self.buckets.is_empty() {
            parameters.remove("buckets");
        } else {
            parameters.insert(
                "buckets".to_string(),
                serde_json::Value::Array(
                    self.buckets
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
    }

    pub fn validate_for_service_create(&self) -> Result<(), ManagedS3Error> {
        if !self.backend.is_creatable() {
            return Err(ManagedS3Error::DeprecatedBackend);
        }
        Ok(())
    }
}

/// Parse the `buckets` service parameter. Accepts a JSON array of names or a
/// comma-separated string (the shape a flat `key=value` client sends); blank
/// entries are ignored so a half-filled form row is not an error.
pub fn parse_bucket_parameter(
    value: Option<&serde_json::Value>,
) -> Result<Vec<String>, ManagedS3Error> {
    let raw: Vec<String> = match value {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        // A list that was flattened to a string on its way to a worker node
        // arrives as `["a","b"]`; a flat client sends `a,b`.
        Some(serde_json::Value::String(list)) if list.trim_start().starts_with('[') => {
            serde_json::from_str::<Vec<String>>(list).map_err(|_| {
                ManagedS3Error::BucketsNotAList {
                    found: format!("the text {list}"),
                }
            })?
        }
        Some(serde_json::Value::String(list)) => list.split(',').map(str::to_string).collect(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                serde_json::Value::String(name) => Ok(name.clone()),
                other => Err(ManagedS3Error::BucketsNotAList {
                    found: format!("a list containing {other}"),
                }),
            })
            .collect::<Result<_, _>>()?,
        Some(other) => {
            return Err(ManagedS3Error::BucketsNotAList {
                found: other.to_string(),
            })
        }
    };
    normalize_bucket_names(&raw)
}

/// Validate and normalize logical bucket names: trimmed, lower-cased, blank
/// entries dropped, duplicates removed (first occurrence wins, so the primary
/// bucket stays first), at most [`MAX_MANAGED_S3_BUCKETS`].
///
/// A logical name becomes part of the physical bucket name and of an
/// environment variable name (`S3_BUCKET_<NAME>`), so it is restricted to
/// lower-case letters, digits and hyphens, starting and ending with a letter
/// or digit — valid on every S3-compatible backend.
pub fn normalize_bucket_names(raw: &[String]) -> Result<Vec<String>, ManagedS3Error> {
    let mut names: Vec<String> = Vec::new();
    for entry in raw {
        let name = entry.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        validate_logical_bucket_name(&name)?;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.len() > MAX_MANAGED_S3_BUCKETS {
        return Err(ManagedS3Error::TooManyBuckets {
            count: names.len(),
            max: MAX_MANAGED_S3_BUCKETS,
        });
    }
    Ok(names)
}

fn validate_logical_bucket_name(name: &str) -> Result<(), ManagedS3Error> {
    let invalid = |reason: &str| ManagedS3Error::InvalidBucketName {
        bucket: name.to_string(),
        reason: reason.to_string(),
    };
    if name.len() < MIN_LOGICAL_BUCKET_LEN || name.len() > MAX_LOGICAL_BUCKET_LEN {
        return Err(invalid(&format!(
            "must be {MIN_LOGICAL_BUCKET_LEN}-{MAX_LOGICAL_BUCKET_LEN} characters long"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(invalid(
            "may only contain lower-case letters, digits and hyphens",
        ));
    }
    let edge_ok = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    if !edge_ok(name.chars().next()) || !edge_ok(name.chars().last()) {
        return Err(invalid("must start and end with a letter or digit"));
    }
    Ok(())
}

/// One bucket an environment owns: the name the operator declared and the
/// name that exists on the storage backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3BucketPlan {
    /// Declared name; `None` for the legacy single bucket of a service that
    /// declares no buckets.
    pub logical: Option<String>,
    /// Backend-wide unique bucket name (`<project>-<environment>[-<logical>]`).
    pub physical: String,
}

/// `<project>-<environment>` as used in bucket names: lower-case, `_`→`-`,
/// anything else outside `[a-z0-9-]` dropped, no leading/trailing hyphen.
/// The environment part is omitted when empty (a project-wide link).
pub fn scope_slug(project_scope: &str, environment_scope: &str) -> String {
    let joined = if environment_scope.is_empty() {
        project_scope.to_string()
    } else {
        format!("{project_scope}-{environment_scope}")
    };
    let cleaned: String = joined
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == '_' { '-' } else { c })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    cleaned.trim_matches('-').to_string()
}

/// Backend-wide unique bucket name for `logical` inside `scope`. Names that
/// would exceed S3's 63 bytes are shortened and given a short hash of the
/// full name, so two long names never collide.
pub fn physical_bucket_name(scope: &str, logical: Option<&str>) -> String {
    let full = match logical {
        Some(logical) => format!("{scope}-{logical}"),
        None => scope.to_string(),
    };
    if full.len() <= MAX_PHYSICAL_BUCKET_LEN {
        return full;
    }
    let digest = short_hash_hex(&full);
    let suffix = &digest[..8];
    let keep = MAX_PHYSICAL_BUCKET_LEN - suffix.len() - 1;
    let head = full[..keep].trim_end_matches('-');
    format!("{head}-{suffix}")
}

/// The buckets an environment owns. A service that declares none keeps its
/// historical single `<project>-<environment>` bucket.
pub fn plan_buckets(scope: &str, logical_names: &[String]) -> Vec<S3BucketPlan> {
    if logical_names.is_empty() {
        return vec![S3BucketPlan {
            logical: None,
            physical: physical_bucket_name(scope, None),
        }];
    }
    logical_names
        .iter()
        .map(|logical| S3BucketPlan {
            logical: Some(logical.clone()),
            physical: physical_bucket_name(scope, Some(logical)),
        })
        .collect()
}

fn short_hash_hex(input: &str) -> String {
    use sha2::Digest;
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Credentials one environment uses, plus the buckets they reach.
#[derive(Clone, PartialEq, Eq)]
pub struct ManagedS3Grant {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub buckets: Vec<S3BucketPlan>,
}

/// Hand-written so a `{:?}` in a log line or a failed assertion cannot print
/// the secret key.
impl std::fmt::Debug for ManagedS3Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedS3Grant")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field("buckets", &self.buckets)
            .finish()
    }
}

/// Shape of a backend's credentials. Garage insists on `GK` + 24 hex for the
/// key id and 64 hex for the secret; RustFS accepts 3..=20 / 8..=40.
#[derive(Debug, Clone, Copy)]
pub struct CredentialFormat {
    pub access_prefix: &'static str,
    pub access_hex_len: usize,
    pub secret_hex_len: usize,
    pub upper_case_access: bool,
}

/// Per-environment credentials, derived rather than stored.
///
/// `HMAC-SHA256(root_secret, "temps-managed-s3/v1/<backend>/<label>/<scope>")`.
/// Deriving makes provisioning idempotent without keeping per-environment
/// state in Temps: a redeploy computes the same pair and the backend call is
/// an upsert. It also means RustFS — which can set but never read back a
/// secret — needs no side channel to learn what it issued. Only the service's
/// own root secret can reproduce them, and rotating that secret rotates
/// every environment key with it.
pub fn derive_env_credentials(
    kind: ManagedS3BackendKind,
    root_secret: &str,
    scope: &str,
    format: CredentialFormat,
) -> (String, String) {
    let derive = |label: &str, hex_len: usize| -> String {
        // HMAC accepts any key length, so `new_from_slice` cannot fail; fall
        // back to an empty digest rather than panic if that ever changes.
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(root_secret.as_bytes()) else {
            return "0".repeat(hex_len);
        };
        mac.update(format!("temps-managed-s3/v1/{}/{label}/{scope}", kind.as_str()).as_bytes());
        let mut digest = hex::encode(mac.finalize().into_bytes());
        while digest.len() < hex_len {
            digest.push_str(&digest.clone());
        }
        digest.truncate(hex_len);
        digest
    };
    let access_body = derive("access", format.access_hex_len);
    let access_body = if format.upper_case_access {
        access_body.to_ascii_uppercase()
    } else {
        access_body
    };
    (
        format!("{}{access_body}", format.access_prefix),
        derive("secret", format.secret_hex_len),
    )
}

/// The grant an environment receives, computed without touching the backend.
/// Used by the side-effect-free preview and as the single place the
/// credentials are derived for both backends.
pub fn derive_grant(
    kind: ManagedS3BackendKind,
    root_secret: &str,
    scope: &str,
    logical_buckets: &[String],
    format: CredentialFormat,
) -> ManagedS3Grant {
    let (access_key_id, secret_access_key) =
        derive_env_credentials(kind, root_secret, scope, format);
    ManagedS3Grant {
        access_key_id,
        secret_access_key,
        buckets: plan_buckets(scope, logical_buckets),
    }
}

/// Where a managed service is reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedS3Endpoint {
    /// Docker network address other containers use (`<container>:<port>`).
    pub internal_host: String,
    pub internal_port: u16,
    /// Loopback address on the owning host, for the control plane.
    pub host: String,
    pub host_port: u16,
    pub region: String,
}

impl ManagedS3Endpoint {
    /// URL injected into app containers.
    pub fn internal_url(&self) -> String {
        format!("http://{}:{}", self.internal_host, self.internal_port)
    }

    /// URL the control plane (running on the owning host) talks to.
    pub fn host_url(&self) -> String {
        format!("http://{}:{}", self.host, self.host_port)
    }
}

/// The app-facing environment. These names are the contract: they are
/// identical for every backend, and the existing ones never change.
///
/// `S3_BUCKET` is the primary (first) bucket; `S3_BUCKET_<NAME>` is added for
/// every declared bucket, where `<NAME>` is the logical name upper-cased with
/// `-` as `_`.
pub fn render_runtime_env(
    endpoint: &ManagedS3Endpoint,
    grant: &ManagedS3Grant,
) -> Result<HashMap<String, String>, ManagedS3Error> {
    let primary = grant
        .buckets
        .first()
        .ok_or_else(|| ManagedS3Error::InvalidConfig {
            backend: "managed-s3",
            service: String::new(),
            reason: "an environment grant must reach at least one bucket".to_string(),
        })?;
    let url = endpoint.internal_url();
    let mut env = HashMap::new();

    env.insert("S3_BUCKET".to_string(), primary.physical.clone());
    for plan in &grant.buckets {
        if let Some(logical) = &plan.logical {
            env.insert(bucket_env_name(logical), plan.physical.clone());
        }
    }
    env.insert("S3_ENDPOINT".to_string(), url.clone());
    env.insert("S3_HOST".to_string(), endpoint.internal_host.clone());
    env.insert("S3_PORT".to_string(), endpoint.internal_port.to_string());
    env.insert("S3_ACCESS_KEY".to_string(), grant.access_key_id.clone());
    env.insert("S3_SECRET_KEY".to_string(), grant.secret_access_key.clone());
    env.insert("S3_REGION".to_string(), endpoint.region.clone());
    env.insert("S3_FORCE_PATH_STYLE".to_string(), "true".to_string());

    env.insert("AWS_ACCESS_KEY_ID".to_string(), grant.access_key_id.clone());
    env.insert(
        "AWS_SECRET_ACCESS_KEY".to_string(),
        grant.secret_access_key.clone(),
    );
    env.insert("AWS_DEFAULT_REGION".to_string(), endpoint.region.clone());
    env.insert("AWS_REGION".to_string(), endpoint.region.clone());
    env.insert("AWS_ENDPOINT_URL".to_string(), url);
    Ok(env)
}

/// `S3_BUCKET_<UPPER_SNAKE_NAME>` for a logical bucket name.
pub fn bucket_env_name(logical: &str) -> String {
    format!(
        "S3_BUCKET_{}",
        logical.to_ascii_uppercase().replace('-', "_")
    )
}

/// An S3 location outside the managed service: a backup destination/source.
#[derive(Clone)]
pub struct S3Location {
    /// `http(s)://host:port`, or `None` for AWS S3 proper.
    pub endpoint: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    /// Present for temporary (STS) credentials only.
    pub session_token: Option<String>,
    pub bucket: String,
    /// Key prefix under `bucket`, without leading/trailing slash.
    pub prefix: String,
}

impl std::fmt::Debug for S3Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Location")
            .field("endpoint", &self.endpoint)
            .field("access_key", &self.access_key)
            .field("secret_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish()
    }
}

/// Another managed service the data is read from during a migration.
#[derive(Clone)]
pub struct MigrationSource {
    pub service: String,
    pub backend: ManagedS3BackendKind,
    pub endpoint: ManagedS3Endpoint,
    pub admin_access_key: String,
    pub admin_secret_key: String,
}

impl std::fmt::Debug for MigrationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationSource")
            .field("service", &self.service)
            .field("backend", &self.backend)
            .field("endpoint", &self.endpoint)
            .field("admin_access_key", &self.admin_access_key)
            .field("admin_secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Outcome of a mirror-style operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3MirrorReport {
    /// Buckets that were copied, by backend name.
    pub buckets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ManagedS3AccessKeyRequest {
    pub project_scope: String,
    pub environment_scope: String,
    /// Declared logical bucket names (empty ⇒ the legacy single bucket).
    pub buckets: Vec<String>,
}

impl ManagedS3AccessKeyRequest {
    pub fn scope(&self) -> String {
        scope_slug(&self.project_scope, &self.environment_scope)
    }

    pub fn bucket_plan(&self) -> Vec<S3BucketPlan> {
        plan_buckets(&self.scope(), &self.buckets)
    }
}

/// One S3-compatible storage engine managed by Temps.
///
/// Implementations are cheap handles (a service name plus a Docker client);
/// every operation takes the stored [`ServiceConfig`] so the same handle works
/// on the control plane and on a worker that only received a config blob.
#[async_trait]
pub trait ManagedS3Backend: Send + Sync {
    fn kind(&self) -> ManagedS3BackendKind;

    /// Name of the managed service (used in errors and helper containers).
    fn service_name(&self) -> String;

    /// Docker container the service runs in. Restore helpers are labelled
    /// with it so the restore fence and crash recovery can find them.
    fn container_name(&self) -> String;

    /// Where the service listens, from its stored configuration.
    fn endpoint(&self, config: &ServiceConfig) -> Result<ManagedS3Endpoint, ManagedS3Error>;

    /// Service-wide administrative S3 credentials (full access to every
    /// bucket). Used for mirroring and bucket administration, never injected
    /// into apps.
    fn admin_credentials(&self, config: &ServiceConfig)
        -> Result<(String, String), ManagedS3Error>;

    /// Start the engine and make it ready to take writes, returning the
    /// parameters the engine inferred (ports, generated secrets) to persist.
    async fn provision_service(
        &self,
        config: ServiceConfig,
    ) -> Result<HashMap<String, String>, ManagedS3Error>;

    /// Create `bucket` (a physical name) if missing and make sure the
    /// service's administrative key can reach it. Idempotent.
    async fn create_bucket(
        &self,
        config: &ServiceConfig,
        bucket: &str,
    ) -> Result<(), ManagedS3Error>;

    /// Create (or refresh) the environment's credentials and the buckets they
    /// are scoped to, so that the key reaches exactly those buckets and no
    /// others. Idempotent: a redeploy returns the same grant.
    async fn create_access_key(
        &self,
        config: &ServiceConfig,
        request: &ManagedS3AccessKeyRequest,
    ) -> Result<ManagedS3Grant, ManagedS3Error>;

    /// Remove the environment's credentials. Buckets and their objects are
    /// kept: deleting data is a separate, explicit decision.
    async fn revoke_access_key(
        &self,
        config: &ServiceConfig,
        project_scope: &str,
        environment_scope: &str,
    ) -> Result<(), ManagedS3Error>;

    /// Authenticated end-to-end probe.
    async fn health_check(
        &self,
        config: &ServiceConfig,
    ) -> Result<HealthProbeResult, ManagedS3Error>;

    /// The app-facing environment for `grant`. Identical across backends.
    fn render_runtime_env(
        &self,
        config: &ServiceConfig,
        grant: &ManagedS3Grant,
    ) -> Result<HashMap<String, String>, ManagedS3Error> {
        render_runtime_env(&self.endpoint(config)?, grant)
    }

    /// Mirror every bucket of this service to `destination`.
    ///
    /// Required rather than defaulted: the mirror runs helper containers on
    /// the backend's own Docker daemon, and the daemon handle stays private to
    /// each backend instead of being exposed through this trait.
    async fn backup(
        &self,
        config: &ServiceConfig,
        destination: &S3Location,
    ) -> Result<ManagedS3MirrorReport, ManagedS3Error>;

    /// Mirror the buckets stored under `source` into this service. With
    /// `exact`, objects that are not in the backup are removed from the
    /// restored buckets (in-place restore); otherwise the target is only added
    /// to (restore into a new service).
    async fn restore(
        &self,
        config: &ServiceConfig,
        source: &S3Location,
        exact: bool,
    ) -> Result<ManagedS3MirrorReport, ManagedS3Error>;

    /// Copy every bucket of `source` into this service with plain S3
    /// semantics, so the data can move between any two backends.
    async fn migrate_from(
        &self,
        config: &ServiceConfig,
        source: &MigrationSource,
    ) -> Result<ManagedS3MirrorReport, ManagedS3Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: serde_json::Value) -> serde_json::Value {
        value
    }

    #[test]
    fn default_backend_is_rustfs() {
        let selection = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({})))
            .expect("empty parameters select the default backend");
        assert_eq!(selection.backend, ManagedS3BackendKind::Rustfs);
        assert!(selection.buckets.is_empty());
    }

    #[test]
    fn parses_garage_backend_and_buckets() {
        let selection = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": "garage",
            "buckets": ["uploads", "private-files", ""]
        })))
        .expect("garage selection parses");
        assert_eq!(selection.backend, ManagedS3BackendKind::Garage);
        assert_eq!(selection.buckets, vec!["uploads", "private-files"]);
    }

    #[test]
    fn garage_is_accepted_for_new_services() {
        let selection = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": "garage"
        })))
        .expect("garage selection parses");
        selection
            .validate_for_service_create()
            .expect("garage is a creatable backend");
    }

    #[test]
    fn rustfs_is_accepted_for_new_services() {
        let selection = ManagedS3BackendSelection::default();
        selection
            .validate_for_service_create()
            .expect("rustfs is a creatable backend");
    }

    #[test]
    fn parses_legacy_minio_backend_but_rejects_new_creation() {
        let selection = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": "minio"
        })))
        .expect("minio still parses so existing services load");
        assert_eq!(selection.backend, ManagedS3BackendKind::Minio);
        let error = selection
            .validate_for_service_create()
            .expect_err("minio must be rejected for new services");
        assert!(matches!(error, ManagedS3Error::DeprecatedBackend));
        assert!(error.to_string().contains("deprecated"));
    }

    #[test]
    fn unknown_backend_is_rejected_and_named() {
        let error = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": "ceph"
        })))
        .expect_err("unknown backend is rejected");
        assert!(matches!(
            &error,
            ManagedS3Error::UnsupportedBackend { backend } if backend == "ceph"
        ));
        assert!(error.to_string().contains("ceph"));
    }

    #[test]
    fn backend_is_case_insensitive_and_blank_means_default() {
        assert_eq!(
            ManagedS3BackendKind::parse(" Garage ").expect("case-insensitive"),
            ManagedS3BackendKind::Garage
        );
        assert_eq!(
            ManagedS3BackendKind::parse("").expect("blank is default"),
            ManagedS3BackendKind::Rustfs
        );
    }

    #[test]
    fn non_string_backend_is_rejected() {
        let error = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": 123
        })))
        .expect_err("non-string backend is rejected")
        .to_string();
        assert!(error.contains("must be a string"));
    }

    #[test]
    fn null_backend_falls_back_to_default() {
        let selection = ManagedS3BackendSelection::from_parameters(&params(serde_json::json!({
            "backend": serde_json::Value::Null
        })))
        .expect("null backend is the default");
        assert_eq!(selection.backend, DEFAULT_MANAGED_S3_BACKEND);
    }

    #[test]
    fn buckets_are_normalized_deduplicated_and_ordered() {
        let names = parse_bucket_parameter(Some(&serde_json::json!([
            "Uploads",
            " hls-media ",
            "uploads",
            "",
            "backups"
        ])))
        .expect("valid names");
        assert_eq!(names, vec!["uploads", "hls-media", "backups"]);
    }

    #[test]
    fn buckets_accept_a_comma_separated_string() {
        let names =
            parse_bucket_parameter(Some(&serde_json::json!("uploads, private-files,,uploads")))
                .expect("valid names");
        assert_eq!(names, vec!["uploads", "private-files"]);
    }

    #[test]
    fn bucket_names_follow_s3_naming_rules() {
        for (name, fragment) in [
            ("ab", "3-32"),
            ("-bad-start", "start and end"),
            ("bad-end-", "start and end"),
            ("has_underscore", "lower-case letters"),
            ("has.dot", "lower-case letters"),
            ("has space", "lower-case letters"),
            ("ünicode", "lower-case letters"),
        ] {
            let error = normalize_bucket_names(&[name.to_string()])
                .expect_err("invalid name must be rejected");
            assert!(
                matches!(&error, ManagedS3Error::InvalidBucketName { bucket, .. } if bucket == &name.to_ascii_lowercase()),
                "{name}: {error}"
            );
            assert!(error.to_string().contains(fragment), "{name}: {error}");
        }
        let too_long = "a".repeat(MAX_LOGICAL_BUCKET_LEN + 1);
        assert!(normalize_bucket_names(&[too_long]).is_err());
        normalize_bucket_names(&["a".repeat(MAX_LOGICAL_BUCKET_LEN)])
            .expect("the maximum length is valid");
    }

    #[test]
    fn bucket_count_is_bounded() {
        let many: Vec<String> = (0..=MAX_MANAGED_S3_BUCKETS)
            .map(|i| format!("bucket-{i:02}"))
            .collect();
        let error = normalize_bucket_names(&many).expect_err("too many buckets");
        assert!(matches!(
            error,
            ManagedS3Error::TooManyBuckets { count, max }
                if count == MAX_MANAGED_S3_BUCKETS + 1 && max == MAX_MANAGED_S3_BUCKETS
        ));
        normalize_bucket_names(&many[..MAX_MANAGED_S3_BUCKETS]).expect("the limit is inclusive");
    }

    #[test]
    fn duplicates_do_not_count_toward_the_limit() {
        let mut names: Vec<String> = (0..MAX_MANAGED_S3_BUCKETS)
            .map(|i| format!("bucket-{i:02}"))
            .collect();
        names.extend(names.clone());
        assert_eq!(
            normalize_bucket_names(&names)
                .expect("duplicates collapse")
                .len(),
            MAX_MANAGED_S3_BUCKETS
        );
    }

    #[test]
    fn non_list_buckets_are_rejected() {
        for bad in [
            serde_json::json!(7),
            serde_json::json!({"a": 1}),
            serde_json::json!(["ok-name", 3]),
        ] {
            assert!(
                matches!(
                    parse_bucket_parameter(Some(&bad)),
                    Err(ManagedS3Error::BucketsNotAList { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn scope_slug_matches_the_legacy_rustfs_bucket_name() {
        assert_eq!(
            scope_slug("My_Project", "production"),
            "my-project-production"
        );
        assert_eq!(scope_slug("shop", ""), "shop");
        assert_eq!(scope_slug("shop", "preview/feat x"), "shop-previewfeatx");
    }

    #[test]
    fn physical_bucket_names_are_unique_and_within_the_s3_limit() {
        assert_eq!(physical_bucket_name("shop-prod", None), "shop-prod");
        assert_eq!(
            physical_bucket_name("shop-prod", Some("uploads")),
            "shop-prod-uploads"
        );
        let long_scope = "p".repeat(60);
        let a = physical_bucket_name(&long_scope, Some("uploads"));
        let b = physical_bucket_name(&long_scope, Some("uploadz"));
        assert!(a.len() <= MAX_PHYSICAL_BUCKET_LEN, "{a}");
        assert!(b.len() <= MAX_PHYSICAL_BUCKET_LEN, "{b}");
        assert_ne!(a, b, "shortened names must not collide");
        assert!(!a.ends_with('-') && !a.starts_with('-'));
    }

    #[test]
    fn no_declared_buckets_keeps_the_single_legacy_bucket() {
        let plan = plan_buckets("shop-prod", &[]);
        assert_eq!(
            plan,
            vec![S3BucketPlan {
                logical: None,
                physical: "shop-prod".to_string()
            }]
        );
    }

    #[test]
    fn declared_buckets_get_one_physical_bucket_each_in_order() {
        let plan = plan_buckets(
            "shop-prod",
            &["uploads".to_string(), "hls-media".to_string()],
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].physical, "shop-prod-uploads");
        assert_eq!(plan[1].physical, "shop-prod-hls-media");
        assert_eq!(plan[1].logical.as_deref(), Some("hls-media"));
    }

    const GARAGE_FORMAT: CredentialFormat = CredentialFormat {
        access_prefix: "GK",
        access_hex_len: 24,
        secret_hex_len: 64,
        upper_case_access: false,
    };

    #[test]
    fn derived_credentials_are_deterministic_scoped_and_well_formed() {
        let (a1, s1) = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            "root-secret",
            "shop-prod",
            GARAGE_FORMAT,
        );
        let (a2, s2) = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            "root-secret",
            "shop-prod",
            GARAGE_FORMAT,
        );
        assert_eq!((a1.clone(), s1.clone()), (a2, s2), "stable across calls");
        assert_eq!(a1.len(), 26);
        assert!(a1.starts_with("GK"));
        assert!(a1[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s1.len(), 64);

        let (other_scope, _) = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            "root-secret",
            "shop-staging",
            GARAGE_FORMAT,
        );
        let (other_root, other_root_secret) = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            "another-root",
            "shop-prod",
            GARAGE_FORMAT,
        );
        assert_ne!(a1, other_scope, "environments get different keys");
        assert_ne!(a1, other_root, "a different root secret changes the keys");
        assert_ne!(s1, other_root_secret);
        assert_ne!(
            a1[2..],
            s1[..24],
            "the access key must not be a prefix of the secret"
        );
    }

    #[test]
    fn derived_credentials_differ_per_backend_and_honour_the_format() {
        let rustfs_format = CredentialFormat {
            access_prefix: "TMPS",
            access_hex_len: 16,
            secret_hex_len: 40,
            upper_case_access: true,
        };
        let (garage, _) =
            derive_env_credentials(ManagedS3BackendKind::Garage, "root", "scope", GARAGE_FORMAT);
        let (rustfs, rustfs_secret) =
            derive_env_credentials(ManagedS3BackendKind::Rustfs, "root", "scope", rustfs_format);
        assert_ne!(garage[2..18], rustfs[4..]);
        assert_eq!(rustfs.len(), 20);
        assert!(rustfs.starts_with("TMPS"));
        assert_eq!(rustfs, rustfs.to_ascii_uppercase());
        assert_eq!(rustfs_secret.len(), 40);
    }

    fn sample_endpoint() -> ManagedS3Endpoint {
        ManagedS3Endpoint {
            internal_host: "garage-media".to_string(),
            internal_port: 3900,
            host: "localhost".to_string(),
            host_port: 39000,
            region: "us-east-1".to_string(),
        }
    }

    #[test]
    fn runtime_env_renders_stable_names_and_one_variable_per_bucket() {
        let grant = ManagedS3Grant {
            access_key_id: "AK".to_string(),
            secret_access_key: "SK".to_string(),
            buckets: plan_buckets(
                "shop-prod",
                &["uploads".to_string(), "private-files".to_string()],
            ),
        };
        let env = render_runtime_env(&sample_endpoint(), &grant).expect("env renders");

        // Stable, backend-independent names.
        for (name, value) in [
            ("S3_BUCKET", "shop-prod-uploads"),
            ("S3_ENDPOINT", "http://garage-media:3900"),
            ("S3_HOST", "garage-media"),
            ("S3_PORT", "3900"),
            ("S3_ACCESS_KEY", "AK"),
            ("S3_SECRET_KEY", "SK"),
            ("S3_REGION", "us-east-1"),
            ("AWS_ACCESS_KEY_ID", "AK"),
            ("AWS_SECRET_ACCESS_KEY", "SK"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("AWS_REGION", "us-east-1"),
            ("AWS_ENDPOINT_URL", "http://garage-media:3900"),
            ("S3_FORCE_PATH_STYLE", "true"),
        ] {
            assert_eq!(env.get(name).map(String::as_str), Some(value), "{name}");
        }
        // One variable per declared bucket; the first is also S3_BUCKET.
        assert_eq!(
            env.get("S3_BUCKET_UPLOADS").map(String::as_str),
            Some("shop-prod-uploads")
        );
        assert_eq!(
            env.get("S3_BUCKET_PRIVATE_FILES").map(String::as_str),
            Some("shop-prod-private-files")
        );
        assert_eq!(env.len(), 15);
    }

    #[test]
    fn runtime_env_for_an_undeclared_bucket_set_has_no_per_bucket_variables() {
        let grant = ManagedS3Grant {
            access_key_id: "AK".to_string(),
            secret_access_key: "SK".to_string(),
            buckets: plan_buckets("shop-prod", &[]),
        };
        let env = render_runtime_env(&sample_endpoint(), &grant).expect("env renders");
        assert_eq!(env.get("S3_BUCKET").map(String::as_str), Some("shop-prod"));
        assert!(
            !env.keys().any(|k| k.starts_with("S3_BUCKET_")),
            "legacy services keep exactly the historical variables"
        );
    }

    #[test]
    fn runtime_env_requires_at_least_one_bucket() {
        let grant = ManagedS3Grant {
            access_key_id: "AK".to_string(),
            secret_access_key: "SK".to_string(),
            buckets: Vec::new(),
        };
        assert!(render_runtime_env(&sample_endpoint(), &grant).is_err());
    }

    #[test]
    fn bucket_env_names_are_upper_snake_case() {
        assert_eq!(bucket_env_name("hls-media"), "S3_BUCKET_HLS_MEDIA");
        assert_eq!(bucket_env_name("uploads"), "S3_BUCKET_UPLOADS");
    }

    #[test]
    fn grant_and_location_debug_output_hides_secrets() {
        let grant = ManagedS3Grant {
            access_key_id: "AK-visible".to_string(),
            secret_access_key: "top-secret-value".to_string(),
            buckets: Vec::new(),
        };
        assert!(!format!("{grant:?}").contains("top-secret-value"));
        let location = S3Location {
            endpoint: None,
            access_key: "AK".to_string(),
            secret_key: "top-secret-value".to_string(),
            session_token: Some("token-value".to_string()),
            bucket: "b".to_string(),
            prefix: String::new(),
        };
        let debug = format!("{location:?}");
        assert!(!debug.contains("top-secret-value") && !debug.contains("token-value"));
    }

    #[test]
    fn only_rustfs_and_garage_are_creatable() {
        assert!(ManagedS3BackendKind::Rustfs.is_creatable());
        assert!(ManagedS3BackendKind::Garage.is_creatable());
        assert!(!ManagedS3BackendKind::Minio.is_creatable());
    }
}
