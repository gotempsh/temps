// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::Client;
use bollard::query_parameters::{
    InspectContainerOptions, LogsOptionsBuilder, StopContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures::TryStreamExt;
use rand::RngExt;
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{self};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use temps_core::EncryptionService;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, SensitiveValues, ServiceConfig, ServiceResourceLimits,
    ServiceType,
};

/// Input configuration for creating an S3/MinIO service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "S3/MinIO Configuration",
    description = "Configuration for S3-compatible storage service (MinIO)"
)]
pub struct S3InputConfig {
    /// S3/MinIO port (auto-assigned if not provided)
    #[schemars(example = example_port())]
    pub port: Option<String>,

    /// S3 access key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = example_access_key())]
    pub access_key: Option<String>,

    /// S3 secret key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = example_secret_key())]
    pub secret_key: Option<String>,

    /// S3 host address
    #[serde(default = "default_host")]
    #[schemars(example = example_host(), default = "default_host")]
    pub host: String,

    /// S3 region
    #[serde(default = "default_region")]
    #[schemars(example = example_region(), default = "default_region")]
    pub region: String,

    /// Docker image for the S3 server. New services default to RustFS
    /// (`rustfs/rustfs:1.0.0`); an existing service keeps the MinIO image it
    /// was created with. Which server the image runs is `server`, not the
    /// image name.
    ///
    /// The serde fallback for a persisted config with no `docker_image` stays
    /// the legacy MinIO reference on purpose: such a row belongs to a service
    /// whose volume holds MinIO's on-disk format, and silently starting
    /// RustFS on it would not serve that data.
    #[serde(default = "legacy_minio_image")]
    #[schemars(example = example_image(), default = "default_image")]
    pub docker_image: String,

    /// Which S3 server `docker_image` runs: `rustfs` or `minio`. It decides
    /// the container's credential variables, command and healthcheck.
    ///
    /// New services default to `rustfs` and store it. A persisted config
    /// without it predates the field; see [`S3Service::resolve_server_kind`]
    /// for how its kind is worked out (image metadata, then MinIO).
    #[serde(default, deserialize_with = "deserialize_optional_server_kind")]
    #[schemars(example = example_server_kind(), default = "default_server_kind")]
    pub server: Option<S3ServerKind>,

    /// Real Docker container name when this service was imported from an
    /// existing MinIO/S3-compatible container (set by `import_from_container`,
    /// never user-editable — omitted from the create form). Overrides the
    /// derived `s3-{name}` container name so internal addressing targets the
    /// actual pre-existing container instead of a synthesized name that
    /// doesn't exist. Mirrors the MariaDB/Postgres/Redis/MongoDB fix for the
    /// same class of bug.
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

/// Internal runtime configuration for S3/MinIO service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    pub port: String,
    pub access_key: String,
    pub secret_key: String,
    pub host: String,
    pub region: String,
    pub docker_image: String,
    /// See `S3InputConfig::server`. `None` until resolved for a config that
    /// predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<S3ServerKind>,
    /// Real container name for imported services — see
    /// `S3InputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

/// Rewrites a bare (unqualified, i.e. Docker-Hub-resolved) `minio/minio` or
/// `minio/mc` reference to its `quay.io` equivalent, preserving the tag.
///
/// History: Docker Hub stopped serving these repositories first, so this
/// pointed old persisted defaults at quay.io. quay.io has since stopped
/// serving them too, so the rewrite no longer makes anything pullable.
///
/// It is kept because removing it would break running services. Since it
/// shipped, every MinIO container for a bare-reference service has been
/// (re)created as `quay.io/minio/minio:<tag>`. `create_container_once`
/// recreates the container whenever its image differs from
/// `config.docker_image`, so dropping the rewrite would make the next
/// `init()` tear those containers down to recreate them as
/// `minio/minio:<tag>` — an image name the host may not hold and nothing can
/// pull any more. Keeping the rewrite keeps those containers untouched. A
/// host that only holds the bare spelling (the service never re-initialised
/// after this rewrite shipped) is handled by
/// [`S3Service::ensure_server_image`], which tags the local bare image with
/// the quay.io name instead of pulling.
///
/// Only the exact former built-in defaults are rewritten; an
/// already-qualified reference (`quay.io/...`, a private mirror,
/// `ghcr.io/...`) is left untouched since it reflects an explicit operator
/// choice, not our old default.
fn normalize_minio_registry(image: String) -> String {
    for repo in ["minio/minio", "minio/mc"] {
        if let Some(rest) = image.strip_prefix(repo) {
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('@') {
                return format!("quay.io/{repo}{rest}");
            }
        }
    }
    image
}

impl From<S3InputConfig> for S3Config {
    fn from(input: S3InputConfig) -> Self {
        Self {
            port: input.port.unwrap_or_else(|| {
                find_available_port(9000)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "9000".to_string())
            }),
            access_key: input.access_key.unwrap_or_else(default_access_key),
            secret_key: input.secret_key.unwrap_or_else(default_secret_key),
            host: input.host,
            region: input.region,
            docker_image: normalize_minio_registry(input.docker_image),
            server: input.server,
            container_name: input.container_name,
        }
    }
}

fn deserialize_optional_key<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// Parses `S3InputConfig::server`. A blank string (an untouched form field)
/// counts as absent; anything other than `rustfs`/`minio` is rejected.
fn deserialize_optional_server_kind<'de, D>(
    deserializer: D,
) -> Result<Option<S3ServerKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => S3ServerKind::parse(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Treats a blank string the same as an absent value — see
/// `S3InputConfig::container_name`.
fn deserialize_optional_non_empty<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}
fn default_region() -> String {
    "us-east-1".to_string()
}
fn default_host() -> String {
    "localhost".to_string()
}

fn default_access_key() -> String {
    // AWS Access Key format: AKIA + 16 uppercase alphanumeric characters = 20 chars total
    let mut rng = rand::rng();
    let random_part: String = (0..16)
        .map(|_| {
            let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            charset[rng.random_range(0..charset.len())] as char
        })
        .collect();
    format!("AKIA{}", random_part)
}

fn default_secret_key() -> String {
    // AWS Secret Key format: 40 characters of base64-like characters (alphanumeric + / +)
    let mut rng = rand::rng();
    (0..40)
        .map(|_| {
            let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/+";
            charset[rng.random_range(0..charset.len())] as char
        })
        .collect()
}

// Schema example functions
fn example_port() -> &'static str {
    "9000"
}

fn example_access_key() -> &'static str {
    "minioadmin"
}

fn example_secret_key() -> &'static str {
    "minioadmin"
}

fn example_host() -> &'static str {
    "localhost"
}

fn example_region() -> &'static str {
    "us-east-1"
}

/// Image for new S3 services of this engine. MinIO no longer publishes
/// server images (quay.io and Docker Hub both stopped serving them), so new
/// services run RustFS, which speaks the same S3 API.
fn default_image() -> String {
    super::rustfs::DEFAULT_RUSTFS_IMAGE.to_string()
}

fn example_image() -> &'static str {
    super::rustfs::DEFAULT_RUSTFS_IMAGE
}

/// The last MinIO release this engine defaulted to. Only used to fill in a
/// persisted config that predates the `docker_image` parameter (see
/// `S3InputConfig::docker_image`), never for new services.
const LEGACY_MINIO_IMAGE: &str = "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z";

fn legacy_minio_image() -> String {
    LEGACY_MINIO_IMAGE.to_string()
}

/// Repository part of an image reference: no `@digest`, no `:tag` (a `:`
/// before the last `/` is a registry port, not a tag).
fn image_repository(image: &str) -> &str {
    let without_digest = image.split_once('@').map_or(image, |(repo, _)| repo);
    match without_digest.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => without_digest,
    }
}

/// The S3 server program a service's container runs. RustFS and MinIO speak
/// the same S3 API but take different credential variables, commands and
/// healthchecks, so this, not the image name, decides those settings: a
/// private mirror can publish either server under any repository name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum S3ServerKind {
    Rustfs,
    Minio,
}

impl S3ServerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            S3ServerKind::Rustfs => "rustfs",
            S3ServerKind::Minio => "minio",
        }
    }

    /// Parses a stored or user-supplied `server` value (case-insensitive).
    pub fn parse(value: &str) -> Result<Self, S3ServerKindError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rustfs" => Ok(S3ServerKind::Rustfs),
            "minio" => Ok(S3ServerKind::Minio),
            _ => Err(S3ServerKindError::Unknown {
                value: value.to_string(),
            }),
        }
    }

    /// Image a service of this kind runs when none is configured.
    pub fn default_image(self) -> &'static str {
        match self {
            S3ServerKind::Rustfs => super::rustfs::DEFAULT_RUSTFS_IMAGE,
            S3ServerKind::Minio => LEGACY_MINIO_IMAGE,
        }
    }
}

impl std::fmt::Display for S3ServerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Failures to work out which S3 server an image runs.
#[derive(Debug, thiserror::Error)]
pub enum S3ServerKindError {
    #[error("Unknown S3 server kind '{value}': expected 'rustfs' or 'minio'")]
    Unknown { value: String },
    #[error(
        "Could not inspect S3 server image '{image}' to tell whether it runs RustFS or MinIO \
         (set the service's `server` parameter to 'rustfs' or 'minio' to skip this check): {error}"
    )]
    ImageInspect {
        image: String,
        #[source]
        error: bollard::errors::Error,
    },
}

fn default_server_kind() -> S3ServerKind {
    S3ServerKind::Rustfs
}

fn example_server_kind() -> S3ServerKind {
    S3ServerKind::Rustfs
}

/// Works out which S3 server an image runs from its own metadata: the
/// entrypoint and command (`rustfs` vs `minio` binary), the environment it
/// ships (`RUSTFS_*` vs `MINIO_*`) and its `name` label. Returns `None` when
/// the metadata points at neither server or at both, so the caller decides
/// what an unknown image means; the repository name is never consulted.
pub(crate) fn detect_server_kind(
    entrypoint: &[String],
    cmd: &[String],
    env: &[String],
    labels: &HashMap<String, String>,
) -> Option<S3ServerKind> {
    let program_is = |name: &str| {
        entrypoint.iter().chain(cmd.iter()).any(|arg| {
            arg.split_whitespace()
                .next()
                .and_then(|program| program.rsplit('/').next())
                .is_some_and(|program| program.eq_ignore_ascii_case(name))
        })
    };
    let env_has = |prefix: &str| {
        env.iter().any(|entry| {
            entry
                .split_once('=')
                .map_or(entry.as_str(), |(key, _)| key)
                .starts_with(prefix)
        })
    };
    let named = |name: &str| {
        labels
            .get("name")
            .is_some_and(|label| label.trim().eq_ignore_ascii_case(name))
    };

    let rustfs = program_is("rustfs") || env_has("RUSTFS_") || named("rustfs");
    let minio = program_is("minio") || env_has("MINIO_") || named("minio");
    match (rustfs, minio) {
        (true, false) => Some(S3ServerKind::Rustfs),
        (false, true) => Some(S3ServerKind::Minio),
        _ => None,
    }
}

/// [`detect_server_kind`] over a pulled image's config.
fn detect_image_server_kind(config: Option<&bollard::models::ImageConfig>) -> Option<S3ServerKind> {
    let config = config?;
    detect_server_kind(
        config.entrypoint.as_deref().unwrap_or_default(),
        config.cmd.as_deref().unwrap_or_default(),
        config.env.as_deref().unwrap_or_default(),
        &config.labels.clone().unwrap_or_default(),
    )
}

/// True when the image *name* looks like MinIO. Only used to word error
/// messages about images that could not be pulled; it never decides how a
/// container is configured.
fn image_name_mentions_minio(image: &str) -> bool {
    image_repository(image)
        .to_ascii_lowercase()
        .contains("minio")
}

/// Environment that sets the root credentials of an S3 server container:
/// `RUSTFS_ACCESS_KEY`/`RUSTFS_SECRET_KEY` for RustFS,
/// `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` for MinIO.
pub(crate) fn s3_server_credentials_env(
    kind: S3ServerKind,
    access_key: &str,
    secret_key: &str,
) -> [(&'static str, String); 2] {
    match kind {
        S3ServerKind::Rustfs => [
            ("RUSTFS_ACCESS_KEY", access_key.to_string()),
            ("RUSTFS_SECRET_KEY", secret_key.to_string()),
        ],
        S3ServerKind::Minio => [
            ("MINIO_ROOT_USER", access_key.to_string()),
            ("MINIO_ROOT_PASSWORD", secret_key.to_string()),
        ],
    }
}

/// Env, command and healthcheck for the S3 server container.
struct S3ServerContainerSpec {
    env: Vec<String>,
    /// `None` runs the image's default command.
    cmd: Option<Vec<String>>,
    healthcheck: String,
}

fn s3_server_container_spec(config: &S3Config, kind: S3ServerKind) -> S3ServerContainerSpec {
    let mut env: Vec<String> =
        s3_server_credentials_env(kind, &config.access_key, &config.secret_key)
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
    match kind {
        // RustFS serves `/data` by default (`RUSTFS_VOLUMES=/data` in the
        // image), and answers `/health` on the S3 port.
        S3ServerKind::Rustfs => S3ServerContainerSpec {
            env,
            cmd: None,
            healthcheck: "curl -sf http://localhost:9000/health > /dev/null || exit 1".to_string(),
        },
        S3ServerKind::Minio => {
            // Allow unauthenticated Prometheus scraping from the local metrics
            // collector.  These containers are private (no public exposure), so
            // removing the JWT requirement is safe and avoids needing to
            // generate & rotate bearer tokens on every scrape.
            env.push("MINIO_PROMETHEUS_AUTH_TYPE=public".to_string());
            S3ServerContainerSpec {
                env,
                cmd: Some(vec!["server".to_string(), "/data".to_string()]),
                healthcheck: "mc ready local".to_string(),
            }
        }
    }
}

/// The Docker Hub spelling of a `quay.io/minio/...` reference, i.e. the name
/// the image was pulled under before [`normalize_minio_registry`] existed.
fn legacy_docker_hub_minio_spelling(image: &str) -> Option<String> {
    image
        .strip_prefix("quay.io/minio/")
        .map(|rest| format!("minio/{rest}"))
}

/// Error for a MinIO server image that is neither pullable nor on the host.
fn minio_image_unavailable_error(image: &str, pull_error: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Cannot start S3 service: its MinIO image '{image}' could not be pulled ({pull_error}) \
         and is not present on this host. MinIO no longer publishes server images \
         (quay.io/minio/minio and Docker Hub minio/minio stopped serving them). To bring the \
         service back, either set its docker_image to a registry you control that hosts this \
         MinIO release, or move the data to RustFS: restore one of this service's backups to a \
         new service with docker_image set to '{}'.",
        super::rustfs::DEFAULT_RUSTFS_IMAGE
    )
}

use super::port_util::{find_available_port, find_available_port_async, is_port_conflict_error};

pub struct S3Service {
    name: String,
    config: Arc<RwLock<Option<S3Config>>>,
    client: Arc<RwLock<Option<Client>>>,
    /// Resource limits captured at init time, applied to recreate paths.
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
    encryption_service: Arc<EncryptionService>,
}

impl S3Service {
    /// Shell script executed inside a disposable `rc` container
    /// ([`super::rc_client::RC_IMAGE`]) by `restore_in_place`.
    ///
    /// SECURITY: This is a compile-time constant. It MUST NOT be changed to a
    /// runtime `format!()` string that interpolates user-supplied values. All
    /// dynamic values arrive via Docker environment variables:
    ///   - `RESTORE_PREFIX` — user-influenced backup path (via `backup_location`)
    ///   - `RC_HOST_bkp`, `RC_HOST_live` — S3 credentials
    ///
    /// The shell expands `${RESTORE_PREFIX}` safely — environment variable values
    /// are never re-parsed as shell commands, so a value containing `'` or other
    /// shell metacharacters cannot escape the quoting context and inject commands.
    ///
    /// Word-splitting fix: bucket names are iterated with `while IFS= read -r`
    /// from a temp file, avoiding glob expansion on whitespace in names.
    ///
    /// The listing is `rc ls --json`: its pretty-printed document carries a
    /// `"truncated"` flag, which the script requires to be `false` (a `true`
    /// or a missing flag aborts — a partial bucket set must never restore as
    /// if it were complete; there is no jq in the image, so this is a grep on
    /// the flag's own line). Each folder entry's `"key"` is the FULL key
    /// relative to the backup bucket (`backups/x/bucket-a/`), not just the
    /// folder name mc printed, so the `sed` keeps only the last path segment.
    /// A failed listing aborts the restore instead of being read as "nothing
    /// to restore", and so does a failed `rc mb` (with `--ignore-existing` it
    /// exits 0 for a bucket that is already there).
    ///
    /// Temporary backup credentials: when `TEMPS_REMOTE_SESSION_TOKEN` is set,
    /// every call against the backup (`bkp`) side goes through `bkp_rc`, which
    /// adds `-H x-amz-security-token`, and each bucket is staged through a
    /// scratch directory so the live-side `rc` call never carries the token
    /// (the live server would reject it). Without a token the mirror stays a
    /// direct `bkp` → `live` copy.
    const RESTORE_IN_PLACE_SCRIPT: &'static str = r#"set -e
DEST='live'
if [ -z "${RESTORE_PREFIX}" ]; then
  echo '[restore] RESTORE_PREFIX is empty — aborting'
  exit 1
fi
if [ -n "${TEMPS_REMOTE_SESSION_TOKEN:-}" ]; then
  bkp_rc() { rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" "$@"; }
else
  bkp_rc() { rc "$@"; }
fi
STAGE_ROOT=$(mktemp -d)
trap 'rm -rf "${STAGE_ROOT}"' EXIT
echo "[restore] listing ${RESTORE_PREFIX}"
LISTING=$(mktemp)
BUCKET_LIST=$(mktemp)
if ! bkp_rc ls --json "${RESTORE_PREFIX}" > "${LISTING}"; then
  echo "[restore] could not list ${RESTORE_PREFIX} — aborting"
  exit 1
fi
if grep -q '"truncated": *true' "${LISTING}"; then
  echo "[restore] the listing of ${RESTORE_PREFIX} is truncated — refusing a partial restore"
  exit 1
fi
if ! grep -q '"truncated": *false' "${LISTING}"; then
  echo "[restore] could not verify that the listing of ${RESTORE_PREFIX} is complete — aborting"
  exit 1
fi
sed -n 's/^[[:space:]]*"key": *"\(.*\)",*[[:space:]]*$/\1/p' "${LISTING}" | grep '/$' | sed 's|/$||; s|.*/||' > "${BUCKET_LIST}" || true
rm -f "${LISTING}"
if [ ! -s "${BUCKET_LIST}" ]; then
  rm -f "${BUCKET_LIST}"
  echo '[restore] no bucket directories found — nothing to restore'
  exit 0
fi
while IFS= read -r bucket; do
  [ -z "${bucket}" ] && continue
  echo "[restore] ensuring ${DEST}/${bucket} exists"
  rc mb --ignore-existing "${DEST}/${bucket}"
  echo "[restore] mirroring ${RESTORE_PREFIX}${bucket}/ -> ${DEST}/${bucket}/ (--overwrite --remove)"
  if [ -n "${TEMPS_REMOTE_SESSION_TOKEN:-}" ]; then
    STAGE="${STAGE_ROOT}/bucket"
    rm -rf "${STAGE}"
    mkdir -p "${STAGE}"
    bkp_rc mirror --overwrite "${RESTORE_PREFIX}${bucket}/" "${STAGE}/"
    rc mirror --overwrite --remove "${STAGE}/" "${DEST}/${bucket}/"
    rm -rf "${STAGE}"
  else
    rc mirror --overwrite --remove "${RESTORE_PREFIX}${bucket}/" "${DEST}/${bucket}/"
  fi
  echo "[restore] done: ${bucket}"
done < "${BUCKET_LIST}"
rm -f "${BUCKET_LIST}"
echo '[restore] complete'"#;

    pub fn new(
        name: String,
        docker: Arc<Docker>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            client: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
            encryption_service,
        }
    }

    fn get_container_name(&self) -> String {
        format!("minio-{}", self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `minio-{name}`. Every operation that talks to the live container must
    /// resolve through this, not `get_container_name()` directly, or it
    /// targets a synthesized name that doesn't exist for imported services.
    fn get_live_container_name(&self, config: &S3Config) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    fn get_effective_address_for_environment(
        &self,
        service_config: ServiceConfig,
        execution_environment: temps_core::ExecutionEnvironment,
    ) -> Result<(String, String)> {
        let config = self.get_s3_config(service_config)?;
        Ok(match execution_environment {
            temps_core::ExecutionEnvironment::Host => ("localhost".to_string(), config.port),
            temps_core::ExecutionEnvironment::Docker => (
                self.get_live_container_name(&config),
                S3_INTERNAL_PORT.to_string(),
            ),
        })
    }

    /// Creates and starts the MinIO container, retrying with a fresh host
    /// port if the chosen one lost the race described in `port_util` docs
    /// (bindable when we checked, but taken by the time Docker actually binds
    /// it). The container name is deterministic, so a failed attempt must be
    /// removed before retrying or the next attempt's "already exists" check
    /// short-circuits without picking a new port.
    ///
    /// `config` is taken by mutable reference so a retry's port change is
    /// written back to the caller — otherwise the caller (and anything it
    /// persists to the database) keeps referencing the original port even
    /// though the container actually ended up bound to a different one. The
    /// resolved `server` kind is written back the same way, so `init()`
    /// reports it and a new service stores it.
    async fn create_container(
        &self,
        docker: &Docker,
        config: &mut S3Config,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        const MAX_ATTEMPTS: u32 = 3;
        info!("Pulling S3 server image {}", config.docker_image);
        Self::ensure_server_image(docker, &config.docker_image, config.server).await?;
        let kind = Self::resolve_server_kind(docker, config).await?;
        let mut attempt_config = config.clone();
        attempt_config.server = Some(kind);
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .create_container_once(docker, &attempt_config, resource_limits)
                .await
            {
                Ok(()) => {
                    *config = attempt_config;
                    return Ok(());
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_port_conflict_error(&e.to_string()) => {
                    warn!(
                        "Port {} for MinIO container was already allocated (attempt {}/{}), retrying with a fresh port: {}",
                        attempt_config.port, attempt, MAX_ATTEMPTS, e
                    );
                    let _ = docker
                        .remove_container(
                            &self.get_container_name(),
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                    let base_port: u16 = attempt_config.port.parse().unwrap_or(9000);
                    if let Some(new_port) =
                        find_available_port_async(docker, base_port.wrapping_add(1)).await
                    {
                        attempt_config.port = new_port.to_string();
                    }
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop always returns Ok or Err before exhausting MAX_ATTEMPTS")
    }

    /// Creates and starts the server container once. `create_container` has
    /// already made the image available and resolved `config.server`.
    async fn create_container_once(
        &self,
        docker: &Docker,
        config: &S3Config,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        let kind = config.server.ok_or_else(|| {
            anyhow::anyhow!(
                "S3 service '{}': server kind for image '{}' was not resolved before creating its container",
                self.name,
                config.docker_image
            )
        })?;

        let container_name = self.get_container_name();
        // Add volume name construction
        let volume_name = format!("minio_{}_data", self.name);

        // Create volume if it doesn't exist
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await?;

        // Check if container already exists
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            // Check if we need to recreate with a new image
            let existing_image = containers
                .first()
                .and_then(|c| c.image.as_deref())
                .unwrap_or("");

            if existing_image == config.docker_image {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }

            info!(
                "Container {} already exists with different image (current: {}, requested: {}), removing it to recreate",
                container_name, existing_image, config.docker_image
            );

            // Stop the container first
            let _ = docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await?;
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key.as_str(), "minio"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        // RustFS and MinIO take different credential variables and commands;
        // the resolved server kind decides which.
        let server_spec = s3_server_container_spec(config, kind);
        ensure_network_exists(docker)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;
        let networking_config = Some(bollard::models::NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                temps_core::NETWORK_NAME.to_string(),
                bollard::models::EndpointSettings {
                    ..Default::default()
                },
            )])),
        });
        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("9000/tcp", &config.port)),
            // Add volume mount
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            log_config: Some(crate::utils::default_service_log_config()),
            // Security hardening for service containers
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(512),
            ..Default::default()
        };
        resource_limits.apply_to_host_config(&mut host_config);

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(config.docker_image.to_string()),
            networking_config,
            exposed_ports: Some(Vec::from(["9000/tcp".to_string()])),
            env: Some(server_spec.env),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            cmd: server_spec.cmd,
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), server_spec.healthcheck]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(5000000000),   // 5 seconds
                start_interval: Some(1000000000), // 1 second
            }),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MinIO container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start MinIO container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        info!("MinIO container {} created and started", container.id);
        Ok(())
    }

    /// Which S3 server `config.docker_image` runs.
    ///
    /// An explicit `server` wins. A config without one was stored before the
    /// field existed, so the pulled image's own metadata decides (see
    /// [`detect_server_kind`]). When the metadata names neither server, or
    /// both, the service runs as MinIO: every config that can lack the field
    /// was created while this engine only ran MinIO, so that is exactly how
    /// it ran before. An error here instead would stop an existing service
    /// from starting over an image label. The warning names the parameter
    /// that settles it.
    async fn resolve_server_kind(
        docker: &Docker,
        config: &S3Config,
    ) -> Result<S3ServerKind, S3ServerKindError> {
        if let Some(kind) = config.server {
            return Ok(kind);
        }
        let image = docker
            .inspect_image(&config.docker_image)
            .await
            .map_err(|error| S3ServerKindError::ImageInspect {
                image: config.docker_image.clone(),
                error,
            })?;
        match detect_image_server_kind(image.config.as_ref()) {
            Some(kind) => {
                info!(
                    "S3 service image '{}' has no stored server kind; its metadata says {}",
                    config.docker_image, kind
                );
                Ok(kind)
            }
            None => {
                warn!(
                    "S3 service image '{}' has no stored server kind and its metadata \
                     (entrypoint, command, env, name label) does not identify RustFS or MinIO; \
                     running it as MinIO, as this service ran before. Set the service's `server` \
                     parameter to 'rustfs' or 'minio' if that is wrong.",
                    config.docker_image
                );
                Ok(S3ServerKind::Minio)
            }
        }
    }

    /// Make the server image available before creating its container.
    ///
    /// `pull_image_with_retry` already falls back to a copy of the exact
    /// reference on this host. Unless the service is known to run RustFS, two
    /// more cases are handled for MinIO images, since MinIO no longer
    /// publishes them anywhere:
    /// - the host holds the image under its pre-quay.io Docker Hub spelling
    ///   (`minio/minio:<tag>`, see [`normalize_minio_registry`]): tag it with
    ///   the configured name and use it, no registry needed;
    /// - it is not on the host at all: fail with an error that says why and
    ///   what to do, instead of a bare registry error.
    ///
    /// The kind is not known yet for a config stored before `server` existed
    /// (it comes from the pulled image), so `None` gets the MinIO handling:
    /// such a config was created as MinIO.
    async fn ensure_server_image(
        docker: &Docker,
        image: &str,
        kind: Option<S3ServerKind>,
    ) -> Result<()> {
        let pull_error = match crate::utils::pull_image_with_retry(docker, image, None).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if kind == Some(S3ServerKind::Rustfs) {
            return Err(anyhow::anyhow!(
                "Cannot start S3 service: its RustFS image '{}' could not be pulled and is not \
                 present on this host: {}",
                image,
                pull_error
            ));
        }

        // A digest reference can't be satisfied by retagging (`docker tag`
        // refuses digests), and falling back to `:latest` would silently
        // relabel an arbitrary release, so digest pins go straight to the
        // actionable error.
        let legacy = legacy_docker_hub_minio_spelling(image).filter(|_| !image.contains('@'));
        if let Some(legacy) = legacy {
            if docker.inspect_image(&legacy).await.is_ok() {
                let repository = image_repository(image);
                let tag = image
                    .strip_prefix(repository)
                    .and_then(|rest| rest.strip_prefix(':'))
                    .unwrap_or("latest");
                docker
                    .tag_image(
                        &legacy,
                        Some(
                            bollard::query_parameters::TagImageOptionsBuilder::new()
                                .repo(repository)
                                .tag(tag)
                                .build(),
                        ),
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "MinIO image '{}' is only on this host as '{}', and tagging it failed: {}",
                            image,
                            legacy,
                            e
                        )
                    })?;
                warn!(
                    "Could not pull MinIO image '{}' ({}); using the local copy '{}' instead",
                    image, pull_error, legacy
                );
                return Ok(());
            }
        }

        // The name only picks the wording of the error.
        if kind == Some(S3ServerKind::Minio) || image_name_mentions_minio(image) {
            Err(minio_image_unavailable_error(image, &pull_error))
        } else {
            Err(anyhow::anyhow!(
                "Cannot start S3 service: its image '{}' could not be pulled and is not present \
                 on this host: {}",
                image,
                pull_error
            ))
        }
    }

    async fn pull_rc_image(&self, docker: &Docker) -> Result<()> {
        info!("Pulling RustFS client image {}", super::rc_client::RC_IMAGE);

        crate::utils::pull_image_with_retry(docker, super::rc_client::RC_IMAGE, None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    async fn wait_for_container_health(&self, docker: &Docker, container_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(500);
        let mut total_wait = Duration::from_secs(0);
        let max_wait = Duration::from_secs(90);
        let max_delay = Duration::from_secs(2);

        while total_wait < max_wait {
            let info = docker
                .inspect_container(container_id, None::<InspectContainerOptions>)
                .await?;
            if let Some(state) = info.state {
                // Considered ready if it's running and either has a HEALTHY
                // Docker healthcheck status or no healthcheck is defined at
                // all (e.g. an imported container from a vanilla image with
                // no HEALTHCHECK directive — requiring an explicit HEALTHY
                // here would spin until `max_wait` every time).
                let is_running =
                    state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING);
                let health_status = state.health.as_ref().and_then(|h| h.status.as_ref());

                if is_running
                    && (health_status.is_none()
                        || health_status == Some(&bollard::models::HealthStatusEnum::HEALTHY))
                {
                    return Ok(());
                }
                if state.status == Some(bollard::models::ContainerStateStatusEnum::EXITED)
                    || state.status == Some(bollard::models::ContainerStateStatusEnum::DEAD)
                {
                    let exit_code = state.exit_code.unwrap_or(-1);
                    return Err(anyhow::anyhow!(
                        "S3 server container exited unexpectedly with code {}",
                        exit_code
                    ));
                }
            }
            sleep(delay).await;
            total_wait += delay;
            delay = std::cmp::min(delay.mul_f32(1.5), max_delay);
        }

        Err(anyhow::anyhow!(
            "S3 server container health check timed out"
        ))
    }

    async fn initialize_client(&self, config: ServiceConfig) -> Result<Client> {
        let s3_config = self.get_s3_config(config)?;
        info!(
            "Initializing S3 client (host={}, port={}, region={})",
            s3_config.host, s3_config.port, s3_config.region
        );
        let config = aws_sdk_s3::Config::builder()
            .endpoint_url(format!("http://{}:{}", s3_config.host, s3_config.port))
            .region(Region::new(s3_config.region))
            .behavior_version_latest()
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                s3_config.access_key,
                s3_config.secret_key,
                None,
                None,
                "minio",
            ))
            .force_path_style(true)
            .build();

        let client = Client::from_conf(config);
        Ok(client)
    }

    async fn create_bucket(&self, config: ServiceConfig, name: &str) -> Result<()> {
        // Initialize client if not already initialized
        let client = self.initialize_client(config).await?;

        let sanitized_name = name.replace("_", "-").to_lowercase();

        // Check if bucket already exists
        match client.head_bucket().bucket(&sanitized_name).send().await {
            Ok(_) => {
                info!("Bucket {} already exists", sanitized_name);
                return Ok(());
            }
            Err(err) => {
                debug!("Bucket {} does not exist: {}", sanitized_name, err);
            }
        }

        // Create bucket if it doesn't exist
        client
            .create_bucket()
            .bucket(sanitized_name.clone())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create bucket {}: {:?}", sanitized_name, e))?;

        info!("Created bucket {}", sanitized_name);
        Ok(())
    }

    #[allow(dead_code)]
    async fn delete_bucket(&self, config: ServiceConfig, name: &str) -> Result<()> {
        // Initialize client if not already initialized
        let client = self.initialize_client(config).await?;

        let sanitized_name = name.replace("_", "-").to_lowercase();

        // Check if bucket exists before attempting to delete
        match client.head_bucket().bucket(&sanitized_name).send().await {
            Ok(_) => {
                // Bucket exists, proceed with deletion
                client
                    .delete_bucket()
                    .bucket(&sanitized_name)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to delete bucket: {}", e))?;

                info!("Deleted bucket {}", sanitized_name);
                Ok(())
            }
            Err(err) => {
                debug!("Bucket {} does not exist: {}", sanitized_name, err);
                Ok(()) // Return Ok since the end state (bucket doesn't exist) is what we want
            }
        }
    }
    fn get_s3_config(&self, service_config: ServiceConfig) -> Result<S3Config> {
        // Parse input config and transform to runtime config
        let input_config: S3InputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse S3 configuration: {}", e))?;

        Ok(S3Config::from(input_config))
    }

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    #[allow(dead_code)]
    async fn verify_image_pullable(&self, image: &str) -> Result<()> {
        info!("Attempting to pull Docker image: {}", image);

        // Try to pull the image - this will fail if it doesn't exist. Retries
        // transient stream errors so a dropped connection isn't mistaken for
        // the image genuinely being unavailable.
        match crate::utils::pull_image_with_retry(&self.docker, image, None).await {
            Ok(()) => {
                info!("Docker image {} is available and pullable", image);
                Ok(())
            }
            Err(e) => {
                error!("Failed to pull Docker image {}: {}", image, e);
                Err(anyhow::anyhow!(
                    "Cannot upgrade: Docker image '{}' is not available or cannot be pulled. Error: {}",
                    image, e
                ))
            }
        }
    }
}

/// Internal port used by MinIO inside the container
const S3_INTERNAL_PORT: &str = "9000";

impl S3Service {
    /// Build the per-tenant bucket name shared between provisioning and
    /// preview paths.
    fn bucket_name_for(project_id: &str, environment: &str) -> String {
        format!("{}-{}", project_id, environment)
            .replace('_', "-")
            .to_lowercase()
    }

    /// Build the `S3_*` / `AWS_*` env vars for a given bucket name. Shared
    /// between `get_runtime_env_vars` and `preview_runtime_env_vars`.
    fn build_runtime_env_vars(
        &self,
        config: ServiceConfig,
        bucket_name: &str,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        let s3_config = self.get_s3_config(config.clone())?;
        let effective_host = self.get_live_container_name(&s3_config);
        let effective_port = S3_INTERNAL_PORT.to_string();

        env_vars.insert("S3_BUCKET".to_string(), bucket_name.to_string());

        let endpoint = format!("http://{}:{}", effective_host, effective_port);
        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());

        let access_key = config
            .parameters
            .get("access_key")
            .and_then(|v| v.as_str())
            .context("Missing access key parameter")?;
        let secret_key = config
            .parameters
            .get("secret_key")
            .and_then(|v| v.as_str())
            .context("Missing secret key parameter")?;

        env_vars.insert("S3_HOST".to_string(), effective_host.clone());
        env_vars.insert("S3_PORT".to_string(), effective_port);
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.to_string());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.to_string());
        env_vars.insert("S3_REGION".to_string(), "us-east-1".to_string());

        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.to_string());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.to_string());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), "us-east-1".to_string());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint);

        Ok(env_vars)
    }
}

/// Docker-free, static metadata about this engine.
///
/// The parameter schema is generated from the input-config type and
/// depends on nothing at runtime, so it must be reachable without
/// constructing a service instance — a control plane with no local
/// Docker daemon still has to serve it to the console.
impl S3Service {
    /// JSON Schema describing this engine's creation parameters.
    pub fn parameter_schema() -> Option<serde_json::Value> {
        // Generate JSON Schema from S3InputConfig
        let schema = schemars::schema_for!(S3InputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable (based on S3ParameterStrategy::updateable_keys)
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable - must match S3ParameterStrategy::updateable_keys()
                let editable = match key.as_str() {
                    "host" => false,        // Read-only
                    "port" => true,         // Updateable
                    "access_key" => false,  // Read-only
                    "secret_key" => false,  // Read-only
                    "region" => false,      // Read-only
                    "docker_image" => true, // Updateable
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
    }
}

#[async_trait]
impl ExternalService for S3Service {
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_s3_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        self.get_effective_address_for_environment(
            service_config,
            temps_core::runtime::execution_environment_compatibility(),
        )
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        S3_INTERNAL_PORT.to_string()
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!(
            "Initializing S3 service (name={}, type={:?}, version={:?})",
            config.name, config.service_type, config.version
        );

        // Pull resource limits before consuming the parameters JSON.
        let resource_limits = ServiceResourceLimits::from_parameters(&config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }
        *self.resource_limits.write().await = resource_limits.clone();

        // Parse input config and transform to runtime config
        let mut s3_config = self.get_s3_config(config)?;
        info!(
            "S3 runtime config (host={}, port={}, region={}, image={})",
            s3_config.host, s3_config.port, s3_config.region, s3_config.docker_image
        );

        // Store runtime config. Gets overwritten below once the real
        // container port is known.
        *self.config.write().await = Some(s3_config.clone());

        if s3_config.container_name.is_none() {
            // Create Docker container. `create_container` may retry on a
            // different host port than requested (see its docs); it writes
            // that back into `s3_config`, so everything below reflects the
            // port the container is actually bound to.
            self.create_container(&self.docker, &mut s3_config, &resource_limits)
                .await?;
            *self.config.write().await = Some(s3_config.clone());
        } else {
            info!(
                "S3/MinIO service '{}' is imported from container '{}'; skipping container creation",
                self.name,
                self.get_live_container_name(&s3_config)
            );
        }

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (keys, port) are persisted
        let runtime_config_json = serde_json::to_value(&s3_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize S3 runtime config: {}", e))?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        info!(
            "Inferred S3 params (keys: {:?})",
            inferred_params.keys().collect::<Vec<_>>()
        );
        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        // let client = self.get_client().await?;
        // let config = self.config.read().await;
        Ok(true)
        // if let Some(cfg) = config.as_ref() {
        //     let result = client.head_bucket().bucket(&cfg.bucket).send().await;
        //     Ok(result.is_ok())
        // } else {
        //     Ok(false)
        // }
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::{Duration, Instant};

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_s3_config(service_config) {
            Ok(c) => c,
            Err(e) => return Ok(HealthProbeResult::down(format!("invalid s3 config: {}", e))),
        };

        let endpoint = format!("http://{}:{}", cfg.host, cfg.port);
        let start = Instant::now();

        let probe = async {
            let creds = aws_sdk_s3::config::Credentials::new(
                cfg.access_key.clone(),
                cfg.secret_key.clone(),
                None,
                None,
                "s3-health-probe",
            );
            let s3_config = aws_sdk_s3::Config::builder()
                .region(Region::new(cfg.region.clone()))
                .endpoint_url(endpoint.clone())
                .credentials_provider(creds)
                .force_path_style(true)
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .build();
            let client = Client::from_conf(s3_config);
            client
                .list_buckets()
                .send()
                .await
                .map_err(|e| format!("ListBuckets failed: {}", e))?;
            Ok::<(), String>(())
        };

        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Err(_) => Ok(HealthProbeResult::down(format!(
                "s3 probe to {} timed out after {}s",
                endpoint,
                PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(msg)) => Ok(HealthProbeResult::down(format!(
                "s3 probe to {} {}",
                endpoint, msg
            ))),
            Ok(Ok(())) => {
                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();
                if elapsed_ms > DEGRADED_MS {
                    Ok(HealthProbeResult::degraded(
                        format!("s3 responded in {}ms (>{}ms)", elapsed_ms, DEGRADED_MS),
                        response_time,
                    ))
                } else {
                    Ok(HealthProbeResult::operational(response_time))
                }
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::S3
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Failed to read config"))?;
        match &*config {
            Some(cfg) => {
                let endpoint = format!("http://localhost:{}", cfg.port);
                Ok(format!("s3://{}", endpoint))
            }
            None => Err(anyhow::anyhow!("S3 not configured")),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        *self.client.write().await = None;
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        Self::parameter_schema()
    }

    async fn start(&self) -> Result<()> {
        let docker = &self.docker;
        let existing_config = self.config.read().await.as_ref().cloned();
        let container_name = existing_config
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Starting MinIO container {}", container_name);

        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        // Imported services never create a container: the real one already
        // exists (we only attach + address it). Fail loudly if it's gone
        // rather than silently synthesizing a fresh empty one.
        if let Some(config) = existing_config
            .as_ref()
            .filter(|c| c.container_name.is_some())
        {
            if containers.is_empty() {
                return Err(anyhow::anyhow!(
                    "Imported S3/MinIO container '{}' not found",
                    container_name
                ));
            }
            let is_running = matches!(
                containers[0].state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );
            if !is_running {
                docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to start imported S3/MinIO container: {}", e)
                    })?;
            }
            self.wait_for_container_health(docker, &container_name)
                .await?;
            let _ = config;
            return Ok(());
        }

        if containers.is_empty() {
            let mut config =
                existing_config.ok_or_else(|| anyhow::anyhow!("S3 configuration not found"))?;
            let limits = self.resource_limits.read().await.clone();
            self.create_container(docker, &mut config, &limits).await?;
            *self.config.write().await = Some(config);
        } else {
            docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start existing MinIO container: {}", e))?;
        }

        self.wait_for_container_health(docker, &container_name)
            .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Clear the client
        *self.client.write().await = None;

        // Stop the container
        let docker = &self.docker;
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Stopping MinIO container {}", container_name);

        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop MinIO container: {}", e))?;
        }

        Ok(())
    }
    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![super::RuntimeEnvVar {
            name: "S3_BUCKET".to_string(),
            description: "S3 bucket name for this project/environment".to_string(),
            example: "project_123_production".to_string(),
            sensitive: false,
        }]
    }

    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let bucket_name = Self::bucket_name_for(project_id, environment);
        // Create the bucket
        self.create_bucket(config.clone(), &bucket_name).await?;
        self.build_runtime_env_vars(config, &bucket_name)
    }

    async fn preview_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let bucket_name = Self::bucket_name_for(project_id, environment);
        // Preview: skip create_bucket so the UI doesn't provision buckets.
        self.build_runtime_env_vars(config, &bucket_name)
    }
    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume
        let docker = &self.docker;
        let container_name = self.get_container_name();
        let volume_name = format!("minio_{}_data", self.name);

        info!("Removing MinIO container and volume for {}", self.name);

        // Remove container if it exists
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            // Stop container first if running
            docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop MinIO container: {}", e))?;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove MinIO container: {}", e))?;
        }

        // Remove volume
        match docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(_) => info!("Removed volume {}", volume_name),
            Err(e) => info!("Error removing volume {}: {}", volume_name, e),
        }

        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name (stored raw
        // in parameters, since the typed config isn't available here) wins over
        // the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = S3_INTERNAL_PORT.to_string();

        let endpoint = format!("http://{}:{}", effective_host, effective_port);

        let access_key = parameters
            .get("access_key")
            .context("Missing access key parameter")?;
        let secret_key = parameters
            .get("secret_key")
            .context("Missing secret key parameter")?;
        let region_val = "us-east-1".to_string();
        let region = parameters.get("region").unwrap_or(&region_val);

        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_HOST".to_string(), effective_host);
        env_vars.insert("S3_PORT".to_string(), effective_port);
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.clone());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.clone());
        env_vars.insert("S3_REGION".to_string(), region.clone());

        // Also provide AWS-style environment variables
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.clone());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.clone());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), region.clone());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint.clone());

        Ok(env_vars)
    }
    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name (stored raw
        // in parameters, since the typed config isn't available here) wins over
        // the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = S3_INTERNAL_PORT.to_string();

        let access_key = parameters
            .get("access_key")
            .context("Missing access key parameter")?;
        let secret_key = parameters
            .get("secret_key")
            .context("Missing secret key parameter")?;
        let endpoint = format!("http://{}:{}", effective_host, effective_port);

        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_HOST".to_string(), effective_host);
        env_vars.insert("S3_PORT".to_string(), effective_port);
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.clone());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.clone());
        env_vars.insert("S3_REGION".to_string(), "us-east-1".to_string());

        // AWS-style environment variables
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.clone());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.clone());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), "us-east-1".to_string());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint);

        Ok(env_vars)
    }

    /// Backup S3 data to another S3 location
    async fn backup_to_s3(
        &self,
        // we are not using the s3 client for this backup, we are using the rc container to backup the data
        _s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        _subpath: &str,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        info!(
            "Starting S3 backup using the RustFS client (rc) for backup {}",
            backup.id
        );

        // Use a standard backup path without versioning
        let backup_prefix = subpath_root;
        let container_name = format!("rc-backup-{}", backup.id);

        // Create a backup record directly using ActiveModel setters (no need to build and then copy)
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set(backup_prefix.to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": "s3",
                    "service_name": self.name,
                    "timestamp": Utc::now().to_rfc3339(),
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0), // System user ID
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        // Pull the RustFS client image
        self.pull_rc_image(&self.docker).await?;

        let service_config = service_config.clone();
        let s3_source_config = self.get_s3_config(service_config)?;

        // Decrypt the backup destination's credentials
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt access key: {}", e))?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt secret key: {}", e))?;
        // `None` for every operator-configured long-lived credential, which
        // leaves the values below byte-for-byte what they were.
        let decrypted_session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            s3_source,
        )
        .map_err(|e| anyhow::anyhow!("Failed to decrypt session token: {}", e))?;

        let default_dest_endpoint = format!("http://{}:9000", s3_source.bucket_name);
        let dest_endpoint = s3_source
            .endpoint
            .as_deref()
            .unwrap_or(&default_dest_endpoint);
        let has_token = decrypted_session_token
            .as_deref()
            .is_some_and(|token| !token.is_empty());

        // Both aliases are defined through `RC_HOST_*` (no `rc alias set`, so
        // no keys in argv; credentials percent-encoded because generated
        // secrets can contain `/`). A temporary destination credential's
        // session token travels separately as `TEMPS_REMOTE_SESSION_TOKEN`
        // and is only sent, via `-H`, by the destination-side step of each
        // mirror.
        let mut env_vars = vec![
            super::rc_client::rc_host_env(
                "original",
                &format!("http://{}:{}", s3_source_config.host, s3_source_config.port),
                &s3_source_config.access_key,
                &s3_source_config.secret_key,
            ),
            super::rc_client::rc_host_env(
                "backup-dest",
                dest_endpoint,
                &decrypted_access_key,
                &decrypted_secret_key,
            ),
        ];
        env_vars.extend(super::rc_client::remote_session_token_env(
            decrypted_session_token.as_deref(),
        ));

        // Long-running rc helper container (a `sh` entrypoint + tty keeps it
        // alive); the commands below run via exec.
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(super::rc_client::RC_IMAGE.to_string()),
            env: Some(env_vars),
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Create the container
        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        // Start the container
        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let dest_name = format!(
            "backup-dest/{}/{}",
            s3_source.bucket_name,
            subpath_root.trim_matches('/')
        );

        let mut success = true;
        let mut error_logs = Vec::new();
        // Client stderr is both logged and persisted into
        // `external_service_backups.error_message`, and `mc` used to echo the
        // credential-bearing host URL into it. The destination credential may
        // be a temporary one, so its session token belongs on the list
        // alongside the keys.
        let sensitive_values = SensitiveValues::new()
            .credential(
                &s3_source_config.access_key,
                &s3_source_config.secret_key,
                None,
            )
            .credential(
                &decrypted_access_key,
                &decrypted_secret_key,
                decrypted_session_token.as_deref(),
            );

        // `rc mirror` cannot take the alias root as its source the way
        // `mc mirror original/ ...` did (without --remove, to preserve files),
        // so mirror bucket by bucket into `<dest_name>/<bucket>/` — the same
        // layout every restore path lists.
        if success {
            if let Err(e) = super::rc_client::mirror_all_buckets(
                &self.docker,
                &container.id,
                "original",
                &dest_name,
                has_token,
                &sensitive_values,
            )
            .await
            {
                error!("S3 backup mirror failed: {}", e);
                error_logs.push(e.to_string());
                success = false;
            }
        }

        // Clean up the container
        self.docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        if success {
            // Compute size by listing the destination prefix.
            let dest_s3_client = s3_credentials.build_s3_client().await;
            let size_bytes = match super::s3_util::list_total_size(
                &dest_s3_client,
                &s3_credentials.bucket_name,
                &format!("{}/", subpath_root.trim_matches('/')),
            )
            .await
            {
                Ok(n) => Some(n),
                Err(e) => {
                    warn!(
                        "S3 mirror backup succeeded but failed to compute size: {}",
                        e
                    );
                    None
                }
            };

            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("completed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            backup_update.size_bytes = Set(size_bytes);
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            info!("S3 backup completed successfully ({:?} bytes)", size_bytes);
            Ok(super::BackupOutcome::new(
                backup_prefix.to_string(),
                size_bytes,
            ))
        } else {
            let error_message = error_logs.join("\n");

            // Update backup record with error
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.error_message = Set(Some(error_message.clone()));
            backup_update.finished_at = Set(Some(Utc::now()));
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            Err(anyhow::anyhow!("Backup failed: {}", error_message))
        }
    }

    async fn restore_from_s3(
        &self,
        // we are not using the s3 client for this restore, we are using the rc container to restore the backup
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!(
            "Starting S3 restore from backup location: {}",
            backup_location
        );

        // Ensure S3 container is running before attempting restore
        self.start().await?;

        let docker = &self.docker;
        let container_name = format!("rc-restore-{}", uuid::Uuid::new_v4());
        let s3_config = self.get_s3_config(service_config)?;

        // Pull the RustFS client image
        self.pull_rc_image(docker).await?;

        // Note: s3_source credentials are expected to be plain-text (already decrypted by caller)
        // When called from CLI, they come from env vars (not encrypted)
        // When called from main app, caller should decrypt before passing
        let source_access_key = &s3_source.access_key_id;
        let source_secret_key = &s3_source.secret_key;
        // Plaintext on the same contract; `None` for a long-lived credential.
        let source_session_token = s3_source.session_token.as_deref();
        let has_token = source_session_token.is_some_and(|token| !token.is_empty());
        let source_endpoint = s3_source
            .endpoint
            .as_deref()
            .unwrap_or("https://s3.amazonaws.com");

        // Aliases via `RC_HOST_*` only (no `rc alias set`: keys would sit in
        // argv, and its probe cannot send a session token). A temporary source
        // credential's token goes in `TEMPS_REMOTE_SESSION_TOKEN` and is only
        // sent, via `-H`, by the source-side `rc` calls.
        let mut env_vars = vec![
            super::rc_client::rc_host_env(
                "backup-source",
                source_endpoint,
                source_access_key,
                source_secret_key,
            ),
            super::rc_client::rc_host_env(
                "dest",
                &format!("http://localhost:{}", s3_config.port),
                &s3_config.access_key,
                &s3_config.secret_key,
            ),
        ];
        env_vars.extend(super::rc_client::remote_session_token_env(
            source_session_token,
        ));

        // Long-running rc helper container (a `sh` entrypoint + tty keeps it
        // alive); the commands below run via exec.
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(super::rc_client::RC_IMAGE.to_string()),
            env: Some(env_vars),
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Create the container
        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        // Start the container
        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        // Client stderr is logged and folded into the returned error, and `mc`
        // used to print the host URL it could not use into it. Both
        // credentials — including a temporary source credential's session
        // token — have to be scrubbed out of it first.
        let sensitive_values = SensitiveValues::new()
            .credential(source_access_key, source_secret_key, source_session_token)
            .credential(&s3_config.access_key, &s3_config.secret_key, None);

        // The trailing `/` lists the folder's contents rather than every key
        // that merely shares the prefix.
        let source_backup_location = format!(
            "backup-source/{}/{}/",
            s3_source.bucket_name,
            backup_location.trim_matches('/')
        );
        // First, list the buckets in the backup location
        let buckets = super::rc_client::list_dir_names(
            docker,
            &container.id,
            &source_backup_location,
            has_token,
            &sensitive_values,
        )
        .await;
        let buckets = match buckets {
            Ok(buckets) => buckets,
            Err(e) => {
                let _ = docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                // The typed error already names the listed location.
                return Err(e.into());
            }
        };

        info!("Found buckets to restore: {:?}", buckets);

        // For each bucket, create it and mirror its contents
        for bucket_name in &buckets {
            let dest_location = format!("dest/{}", bucket_name);
            // Create bucket command; `--ignore-existing` exits 0 when the
            // bucket is already there.
            let create_bucket_cmd = vec!["rc", "mb", "--ignore-existing", &dest_location];

            // Execute create bucket command
            let exec = docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(create_bucket_cmd.clone()),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            let mut stdout = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
                    match output {
                        bollard::container::LogOutput::StdOut { message } => {
                            let msg = String::from_utf8_lossy(&message);
                            stdout.push_str(&msg);
                            info!("stdout: {}", msg);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            error!(
                                "stderr: {}",
                                sensitive_values.redact(String::from_utf8_lossy(&message).as_ref())
                            );
                        }
                        _ => {}
                    }
                }
            }

            // Any non-zero exit is a real failure (auth, network).
            if let Some(inspect_result) = docker.inspect_exec(&exec.id).await?.exit_code {
                if inspect_result != 0 {
                    return Err(anyhow::anyhow!(
                        "Failed to create bucket {}: Exit code {} - {}",
                        bucket_name,
                        inspect_result,
                        sensitive_values.redact(&stdout)
                    ));
                }
            }

            let source_bucket_loc = format!("{}{}/", source_backup_location, bucket_name);
            let dest_bucket_loc = format!("dest/{}/", bucket_name);
            // Mirror command for this bucket.
            // --overwrite: replace existing objects.
            // No --remove here: the CLI restore path does not guarantee the
            // live bucket only contains pre-backup objects (the caller may
            // have additional context). The API path (restore_in_place) uses
            // --remove for a stricter point-in-time guarantee.
            // A temporary backup credential stages the mirror so only the
            // source-side `rc` call sends its session token.
            let mirror_cmd = super::rc_client::mirror_command(
                &source_bucket_loc,
                &dest_bucket_loc,
                false,
                super::rc_client::TokenSide::when(has_token, super::rc_client::TokenSide::Source),
            );
            let mirror_cmd: Vec<&str> = mirror_cmd.iter().map(String::as_str).collect();

            info!(
                "Executing mirror command for bucket {}: {:?}",
                bucket_name, mirror_cmd
            );

            let mirror_exec = docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(mirror_cmd),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            let mut mirror_stdout = String::new();
            let mut mirror_stderr = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&mirror_exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
                    match output {
                        bollard::container::LogOutput::StdOut { message } => {
                            let msg = String::from_utf8_lossy(&message).into_owned();
                            info!("mirror stdout: {}", msg);
                            mirror_stdout.push_str(&msg);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            let msg = String::from_utf8_lossy(&message).into_owned();
                            error!("mirror stderr: {}", sensitive_values.redact(&msg));
                            mirror_stderr.push_str(&msg);
                        }
                        _ => {}
                    }
                }
            }

            let mirror_exit = docker
                .inspect_exec(&mirror_exec.id)
                .await?
                .exit_code
                .unwrap_or(-1);
            if mirror_exit != 0 {
                let _ = docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                // Redact the accumulated output, not the individual chunks: a
                // credential can straddle two Docker log frames and only the
                // joined string is guaranteed to contain it intact.
                return Err(anyhow::anyhow!(
                    "rc mirror failed for bucket '{}' with exit code {}. stdout: {}. stderr: {}",
                    bucket_name,
                    mirror_exit,
                    sensitive_values.redact(mirror_stdout.trim()),
                    sensitive_values.redact(mirror_stderr.trim()),
                ));
            }
        }

        // Clean up the container
        docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        info!("S3 restore completed successfully");
        Ok(())
    }

    async fn restore_capabilities(
        &self,
        _service_config: ServiceConfig,
    ) -> Result<super::RestoreCapabilities> {
        Ok(super::RestoreCapabilities {
            restore_in_place: true,
            restore_to_new_service: true,
            // PITR for object stores would require versioned buckets + per-version
            // pointers; not yet wired up.
            pitr: false,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        })
    }

    /// Restore the live S3/MinIO service from a backup using a one-shot `rc` container.
    ///
    /// ## Semantics: `--overwrite --remove` (true point-in-time restore)
    ///
    /// The mirror runs with both `--overwrite` (replace existing objects with
    /// their backed-up versions) **and** `--remove` (delete live objects that are
    /// absent from the backup). Together these guarantee the live bucket matches
    /// the backup exactly at the object level.
    ///
    /// ⚠ **Destructive**: objects written to the live service after the backup
    /// was taken **will be permanently deleted**. This is intentional — an
    /// in-place restore that leaves post-backup writes in place is not a
    /// restore, it is a partial merge. Callers that want additive-only behaviour
    /// should use `restore_to_new_service` instead.
    ///
    /// ## What `--remove` does and does NOT remove
    ///
    /// `rc mirror --remove` removes objects within each mirrored bucket. It does
    /// NOT remove extra buckets in the live service that were not present at
    /// backup time — those are left in place. A future enhancement could enumerate
    /// and drop them, but that increases blast radius substantially and is deferred.
    ///
    /// ## Container pattern
    ///
    /// A disposable `rustfs/rc` container runs in host-network mode so it can
    /// reach both the backup S3 source (typically an internet endpoint or a local
    /// S3 server used for e2e) and the live S3 service (typically `localhost:<port>`).
    /// Credentials are passed exclusively via `RC_HOST_*` environment variables —
    /// the same pattern the `S3MirrorEngine` backup engine uses.
    async fn restore_in_place(&self, ctx: super::RestoreContext<'_>) -> Result<()> {
        info!(
            service = %ctx.source_service.name,
            backup_location = ctx.backup_location,
            "S3 restore_in_place: starting one-shot rc mirror (--overwrite --remove)"
        );

        // Ensure the live MinIO container is running before writing to it.
        self.start().await?;

        let s3_config = self.get_s3_config(ctx.source_config)?;
        let docker = &self.docker;

        self.pull_rc_image(docker).await?;

        // ── RC_HOST env vars ─────────────────────────────────────────────────
        // The backup-source endpoint already carries a scheme (http:// / https://).
        // Strip it before embedding in the RC_HOST string to avoid the malformed
        // `http://...@http://...` double-scheme that would otherwise result.
        //
        // Credentials are ALREADY DECRYPTED by the orchestrator (RestoreContext
        // contract). Never call EncryptionService::decrypt_string on them here.
        let bkp_endpoint = ctx.s3_source.endpoint.as_deref().unwrap_or("");
        let (bkp_scheme, bkp_hostpath) = mc_strip_scheme(bkp_endpoint);

        // ── Restore shell script ─────────────────────────────────────────────
        // The S3MirrorEngine backup engine mirrors:
        //   source/<bucket>/ -> bkp/<bkp_bucket>/<prefix>/<original_bucket>/
        //
        // For S3Service the source bucket name is always "" (no per-bucket config
        // on the service row), so the backup prefix contains one folder per
        // original MinIO bucket:
        //   bkp/<bkp_bucket>/<prefix>/bucket1/...
        //   bkp/<bkp_bucket>/<prefix>/bucket2/...
        //
        // The script discovers those bucket folders via `rc ls`, creates each in
        // the live service, and mirrors their contents with --overwrite --remove.
        //
        // SECURITY (injection prevention): `ctx.backup_location` is user-supplied
        // and is passed as the Docker env var `RESTORE_PREFIX` rather than being
        // interpolated into the shell script string via Rust format!(). The shell
        // expands `${RESTORE_PREFIX}` safely — env var values are never re-parsed
        // as shell commands, so single-quotes or other metacharacters in the value
        // cannot break out of the quoting context and inject commands.
        // See `RESTORE_IN_PLACE_SCRIPT` for the static script text.
        //
        // TODO(security): No service-identity validation — a caller with
        // BackupsWrite + ExternalServicesWrite can restore service A's backup
        // onto service B's live MinIO. A full fix needs an optional
        // `confirm_target_service_id` field on `StartRestoreRequest` checked
        // against the URL `{id}`. Deferred because it requires the shared
        // restore-framework code well beyond S3's scope (tracked: PR #595
        // description, "MINOR: no service-identity binding" finding).
        let backup_prefix = format!(
            "bkp/{}/{}",
            ctx.s3_source.bucket_name,
            ctx.backup_location.trim_matches('/')
        );

        // SECURITY (docker inspect exposure): RC_HOST_* env vars embed plaintext
        // credentials in the container's environment for the container's lifetime.
        // This is the same pattern the S3MirrorEngine backup engine uses (accepted
        // design). The container is removed immediately after the script exits (see
        // the remove_container call below), bounding the exposure to the restore
        // execution window. Anyone with Docker socket access has equivalent trust
        // to these credentials for that window.
        //
        // A temporary backup credential's session token is NOT part of
        // `RC_HOST_bkp` (rc would read it as part of the secret); it travels
        // as `TEMPS_REMOTE_SESSION_TOKEN` and the script sends it with `-H`
        // on the backup-side calls only.
        let mut env_vars = vec![
            format!(
                "RC_HOST_bkp={}://{}@{}",
                bkp_scheme,
                super::mc_host_credential(
                    &ctx.s3_source.access_key_id,
                    &ctx.s3_source.secret_key,
                    None,
                ),
                bkp_hostpath,
            ),
            // Live service is always reached via localhost in host-network mode.
            // Percent-encoded: rc cannot parse a raw `/` in the secret.
            format!(
                "RC_HOST_live=http://{}@localhost:{}",
                super::mc_host_credential(&s3_config.access_key, &s3_config.secret_key, None),
                s3_config.port
            ),
            // RESTORE_PREFIX carries the user-influenced backup_location value.
            // It is passed as an env var — never interpolated into shell syntax —
            // so shell metacharacters in the value cannot cause injection.
            format!("RESTORE_PREFIX={}/", backup_prefix),
        ];
        env_vars.extend(super::rc_client::remote_session_token_env(
            ctx.s3_source.session_token.as_deref(),
        ));

        // The container's combined stdout/stderr is folded into the error this
        // method returns, which is persisted to `restore_runs.error` and shown
        // in the API. `mc` printed the host URL it failed on, so every
        // credential in those variables — session token included — is
        // scrubbed out of that blob first.
        let sensitive_values = SensitiveValues::new()
            .credential(
                &ctx.s3_source.access_key_id,
                &ctx.s3_source.secret_key,
                ctx.s3_source.session_token.as_deref(),
            )
            .credential(&s3_config.access_key, &s3_config.secret_key, None);

        // Static script constant — no user-supplied values are interpolated here.
        let script = Self::RESTORE_IN_PLACE_SCRIPT.to_string();

        // ── Spin up and wait ─────────────────────────────────────────────────
        let container_name = format!("temps-s3restore-{}", uuid::Uuid::new_v4());
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(super::rc_client::RC_IMAGE.to_string()),
            env: Some(env_vars),
            // One-shot: `sh -c <script>` exits when the script does. The
            // image's own entrypoint is `rc`, so it has to be overridden.
            entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
            cmd: Some(vec![script]),
            host_config: Some(bollard::models::HostConfig {
                // Host network so rc can reach both localhost (live S3 server)
                // and the remote backup S3 endpoint without extra routing.
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to create rc restore container '{}': {}",
                    container_name,
                    e
                )
            })?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to start rc restore container '{}': {}",
                    container_name,
                    e
                )
            })?;

        // Block until the container exits and capture its exit code.
        let exit_code = docker
            .wait_container(&container.id, None::<WaitContainerOptions>)
            .try_collect::<Vec<_>>()
            .await
            .ok()
            .and_then(|v| v.into_iter().next().map(|r| r.status_code))
            .unwrap_or(1);

        // Collect logs for diagnostics (best-effort — don't let a log-fetch
        // failure mask the actual result).
        let logs = docker
            .logs(
                &container.id,
                Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()),
            )
            .try_collect::<Vec<_>>()
            .await
            .map(|v| v.into_iter().map(|c| c.to_string()).collect::<String>())
            .unwrap_or_default();

        // Best-effort cleanup — don't let removal failure mask the real result.
        let _ = docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if exit_code != 0 {
            return Err(anyhow::anyhow!(
                "S3 restore failed: rc restore script exited with code {} for service '{}'. Logs:\n{}",
                exit_code,
                ctx.source_service.name,
                sensitive_values.redact(logs.trim()),
            ));
        }

        info!(
            service = %ctx.source_service.name,
            "S3 restore_in_place: completed successfully"
        );
        Ok(())
    }

    /// Provision a fresh S3/MinIO service and mirror a backup into it.
    ///
    /// Strategy: clone the source service's config (image, region), generate new
    /// credentials and an unused host port, spin up a new container+volume, then
    /// run `rc mirror` from the backup location into every bucket discovered
    /// under the backup prefix. The new service gets its OWN access keys — it's
    /// not a clone of the source's credentials.
    ///
    /// The orchestrator creates the `external_services` DB row AFTER this
    /// returns, using the parameters we hand back.
    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            "Provisioning new S3/MinIO service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        // Start from the source service's parameters so we keep the image/region.
        let mut new_config = self.get_s3_config(ctx.source_config.clone())?;

        // Fresh port (source's is taken).
        let new_port = find_available_port(9000)
            .ok_or_else(|| anyhow::anyhow!("No available ports for new S3/MinIO service"))?
            .to_string();
        new_config.port = new_port;

        // Fresh credentials — the restored bucket contents are just objects; they
        // carry no embedded auth, so we don't need to preserve the source's keys.
        new_config.access_key = default_access_key();
        new_config.secret_key = default_secret_key();

        // Apply caller overrides on top of the cloned config.
        if let Some(overrides) = parameter_overrides.as_object() {
            if let Some(port) = overrides.get("port").and_then(|v| v.as_str()) {
                new_config.port = port.to_string();
            }
            if let Some(image) = overrides.get("docker_image").and_then(|v| v.as_str()) {
                new_config.docker_image = image.to_string();
                // The source's kind described the source's image. A new image
                // without a `server` override is resolved from its metadata.
                new_config.server = None;
            }
            if let Some(server) = overrides.get("server").and_then(|v| v.as_str()) {
                new_config.server = Some(S3ServerKind::parse(server).map_err(|e| {
                    anyhow::anyhow!(
                        "Invalid `server` override for new S3 service '{}': {}",
                        new_service_name,
                        e
                    )
                })?);
            }
            if let Some(ak) = overrides.get("access_key").and_then(|v| v.as_str()) {
                new_config.access_key = ak.to_string();
            }
            if let Some(sk) = overrides.get("secret_key").and_then(|v| v.as_str()) {
                new_config.secret_key = sk.to_string();
            }
        }

        // Build a sibling service instance targeting the new container name.
        let new_service = S3Service::new(
            new_service_name.clone(),
            self.docker.clone(),
            self.encryption_service.clone(),
        );
        let cloned_limits = ServiceResourceLimits::from_parameters(&ctx.source_config.parameters);
        *new_service.config.write().await = Some(new_config.clone());
        *new_service.resource_limits.write().await = cloned_limits.clone();

        // Provision the new container (image pull, volume, port binding, health check).
        // `create_container` writes any port-conflict retry back into
        // `new_config`, so the `rc mirror` connection below (and the final
        // parameters returned to the orchestrator) target the port the
        // container actually bound to.
        new_service
            .create_container(&self.docker, &mut new_config, &cloned_limits)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create new S3/MinIO container: {}", e))?;
        *new_service.config.write().await = Some(new_config.clone());

        // The orchestrator already decrypted these into the plaintext copy
        // of s3_source it hands us via RestoreContext. Passing them straight
        // through — re-decrypting would fail because the bytes are no longer
        // ciphertext.
        let source_access_key = ctx.s3_source.access_key_id.clone();
        let source_secret_key = ctx.s3_source.secret_key.clone();
        // Plaintext by the same RestoreContext contract. `None` for every
        // long-lived operator-configured credential.
        let source_session_token = ctx.s3_source.session_token.clone();
        let has_token = source_session_token
            .as_deref()
            .is_some_and(|token| !token.is_empty());
        let source_endpoint = ctx
            .s3_source
            .endpoint
            .as_deref()
            .unwrap_or("https://s3.amazonaws.com");

        // Reuse the same rc-based mirror flow that restore_from_s3 uses, but
        // dest is the NEW container. We shell out to a disposable rc container
        // (host networking) rather than calling restore_from_s3 directly because
        // restore_from_s3 calls self.start() — which is a no-op on a fresh
        // instance that hasn't been init()'d.
        self.pull_rc_image(&self.docker).await?;

        let rc_container_name = format!("rc-restore-new-{}", uuid::Uuid::new_v4());
        // SECURITY (docker inspect exposure): RC_HOST_* env vars embed plaintext
        // credentials in the container environment for the container's lifetime.
        // The container is removed immediately after all mirror operations complete
        // (see the remove_container call at the end of this function), bounding
        // the exposure window to the restore execution time. Anyone with Docker
        // socket access has equivalent trust to these credentials for that window.
        //
        // Aliases via `RC_HOST_*` only (no `rc alias set`: keys would sit in
        // argv, and its probe cannot send a session token). A temporary source
        // credential's token goes in `TEMPS_REMOTE_SESSION_TOKEN` and is only
        // sent, via `-H`, by the source-side `rc` calls.
        let mut env_vars = vec![
            super::rc_client::rc_host_env(
                "backup-source",
                source_endpoint,
                &source_access_key,
                &source_secret_key,
            ),
            super::rc_client::rc_host_env(
                "dest",
                &format!("http://localhost:{}", new_config.port),
                &new_config.access_key,
                &new_config.secret_key,
            ),
        ];
        env_vars.extend(super::rc_client::remote_session_token_env(
            source_session_token.as_deref(),
        ));

        // Client stderr is logged and folded into the error persisted to
        // `restore_runs.error`, and `mc` used to echo the credential-bearing
        // host URL into it. A temporary source credential's
        // session token is as sensitive as its secret key, so it goes on the
        // list too.
        let sensitive_values = SensitiveValues::new()
            .credential(
                &source_access_key,
                &source_secret_key,
                source_session_token.as_deref(),
            )
            .credential(&new_config.access_key, &new_config.secret_key, None);

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(super::rc_client::RC_IMAGE.to_string()),
            env: Some(env_vars),
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&rc_container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        // List buckets at the backup prefix and mirror each into the new service.
        let source_backup_location = format!(
            "backup-source/{}/{}/",
            ctx.s3_source.bucket_name,
            ctx.backup_location.trim_matches('/')
        );
        let buckets = super::rc_client::list_dir_names(
            &self.docker,
            &container.id,
            &source_backup_location,
            has_token,
            &sensitive_values,
        )
        .await;
        let buckets = match buckets {
            Ok(buckets) => buckets,
            Err(e) => {
                let _ = self
                    .docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                // The typed error already names the listed location.
                return Err(e.into());
            }
        };

        info!(
            "Restoring {} bucket(s) into new service '{}'",
            buckets.len(),
            new_service_name
        );

        for bucket_name in &buckets {
            let dest_location = format!("dest/{}", bucket_name);

            // Create destination bucket; `--ignore-existing` exits 0 when the
            // bucket is already there.
            let mb_cmd = vec!["rc", "mb", "--ignore-existing", &dest_location];
            let mb_exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(mb_cmd),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
            let mut mb_stderr = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                self.docker.start_exec(&mb_exec.id, None).await?
            {
                while let Ok(Some(chunk)) = output.try_next().await {
                    if let bollard::container::LogOutput::StdErr { message } = chunk {
                        mb_stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                }
            }
            if let Some(code) = self.docker.inspect_exec(&mb_exec.id).await?.exit_code {
                if code != 0 {
                    let _ = self
                        .docker
                        .remove_container(
                            &container.id,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                    return Err(anyhow::anyhow!(
                        "rc mb failed with exit code {} for bucket '{}' in new service '{}': {}",
                        code,
                        bucket_name,
                        new_service_name,
                        sensitive_values.redact(mb_stderr.trim())
                    ));
                }
            }

            // Mirror bucket contents into the fresh bucket.
            // --overwrite: replace any objects that landed during container
            // startup (none expected, but be defensive).
            // No --remove: the destination bucket was just created so there is
            // nothing extra to remove; --remove would be a no-op at best and
            // could race with startup writes at worst.
            let source_bucket_loc = format!("{}{}/", source_backup_location, bucket_name);
            let dest_bucket_loc = format!("{}/", dest_location);
            let mirror_cmd = super::rc_client::mirror_command(
                &source_bucket_loc,
                &dest_bucket_loc,
                false,
                super::rc_client::TokenSide::when(has_token, super::rc_client::TokenSide::Source),
            );
            let mirror_cmd: Vec<&str> = mirror_cmd.iter().map(String::as_str).collect();

            info!(
                "Mirroring bucket {} -> new service '{}'",
                bucket_name, new_service_name
            );
            let mirror_exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(mirror_cmd),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
            let mut new_mirror_stdout = String::new();
            let mut new_mirror_stderr = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                self.docker.start_exec(&mirror_exec.id, None).await?
            {
                while let Ok(Some(chunk)) = output.try_next().await {
                    match chunk {
                        bollard::container::LogOutput::StdOut { message } => {
                            let msg = String::from_utf8_lossy(&message).into_owned();
                            info!("mirror stdout: {}", msg);
                            new_mirror_stdout.push_str(&msg);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            let msg = String::from_utf8_lossy(&message).into_owned();
                            error!("mirror stderr: {}", sensitive_values.redact(&msg));
                            new_mirror_stderr.push_str(&msg);
                        }
                        _ => {}
                    }
                }
            }

            let mirror_exit = self
                .docker
                .inspect_exec(&mirror_exec.id)
                .await?
                .exit_code
                .unwrap_or(-1);
            if mirror_exit != 0 {
                let _ = self
                    .docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                // Redact the accumulated output rather than the individual
                // chunks: a credential can straddle two Docker log frames and
                // only the joined string is guaranteed to contain it intact.
                return Err(anyhow::anyhow!(
                    "rc mirror failed for bucket '{}' into new service '{}' with exit code {}. \
                     stdout: {}. stderr: {}",
                    bucket_name,
                    new_service_name,
                    mirror_exit,
                    sensitive_values.redact(new_mirror_stdout.trim()),
                    sensitive_values.redact(new_mirror_stderr.trim()),
                ));
            }
        }

        // Tear down the helper rc container.
        let _ = self
            .docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        // Serialize the final runtime config so the orchestrator can persist it
        // as the new service's parameters.
        let runtime_json = serde_json::to_value(&new_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize runtime config: {}", e))?;
        let mut parameters = HashMap::new();
        if let Some(obj) = runtime_json.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    parameters.insert(k.clone(), s.to_string());
                } else if let Some(n) = v.as_u64() {
                    parameters.insert(k.clone(), n.to_string());
                }
            }
        }

        let connection_info = format!(
            "s3://{}:***@{}:{}",
            new_config.access_key, new_config.host, new_config.port
        );

        Ok(super::NewServiceRestoreResult {
            parameters,
            connection_info,
        })
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version). New services run RustFS: MinIO no
        // longer publishes server images.
        let image = super::rustfs::DEFAULT_RUSTFS_IMAGE;
        let (name, version) = image.rsplit_once(':').unwrap_or((image, "latest"));
        (name.to_string(), version.to_string())
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        let container = self
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;

        // Get the image from the container's inspection data
        if let Some(image) = container.config.and_then(|c| c.image) {
            // Parse image name and tag from the full image string
            if let Some((name, tag)) = image.split_once(':') {
                Ok((name.to_string(), tag.to_string()))
            } else {
                Ok((image.clone(), "latest".to_string()))
            }
        } else {
            Err(anyhow::anyhow!(
                "Failed to get current docker image for S3/MinIO container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        self.get_default_docker_image().1
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting S3/MinIO upgrade");

        let _old_s3_config = self.get_s3_config(old_config)?;
        let mut new_s3_config = self.get_s3_config(new_config)?;

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_s3_config.docker_image
        );
        self.verify_image_pullable(&new_s3_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old S3/MinIO container");
        self.stop().await?;

        // Create container with new image (keeping the same volume for data persistence)
        info!("Starting S3/MinIO container with new image");
        let limits = self.resource_limits.read().await.clone();
        self.create_container(&self.docker, &mut new_s3_config, &limits)
            .await?;
        *self.config.write().await = Some(new_s3_config);

        info!("S3/MinIO upgrade completed successfully");
        Ok(())
    }

    async fn import_from_container(
        &self,
        container_id: String,
        service_name: String,
        credentials: HashMap<String, String>,
        additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        // Inspect the container to get details
        let container = self
            .docker
            .inspect_container(
                &container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to inspect container '{}': {}", container_id, e)
            })?;

        // The real Docker container name — every operation on an imported
        // service must target this, not the derived `minio-{name}`.
        let imported_container_name = container
            .name
            .as_deref()
            .unwrap_or(&container_id)
            .trim_start_matches('/')
            .to_string();

        // The server the container runs, from its own config (entrypoint,
        // command, env, labels) — not from the image name. Stored so the
        // service never has to guess again.
        let container_image_config =
            container
                .config
                .as_ref()
                .map(|c| bollard::models::ImageConfig {
                    entrypoint: c.entrypoint.clone(),
                    cmd: c.cmd.clone(),
                    env: c.env.clone(),
                    labels: c.labels.clone(),
                    ..Default::default()
                });
        let server = detect_image_server_kind(container_image_config.as_ref());
        // Extract image name and version
        let image = container.config.and_then(|c| c.image).ok_or_else(|| {
            anyhow::anyhow!("Could not determine image for container '{}'", container_id)
        })?;
        let server = server.unwrap_or_else(|| {
            warn!(
                "Imported S3 container '{}' (image '{}') does not identify as RustFS or MinIO; \
                 recording it as MinIO. Set the service's `server` parameter if that is wrong.",
                container_id, image
            );
            S3ServerKind::Minio
        });

        // Extract version from image name (e.g., "minio/minio:latest" -> "latest")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "latest".to_string()
        };

        // Extract port from additional config if provided, otherwise use 9000
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("9000")
            .to_string();

        // Extract credentials
        let access_key = credentials
            .get("access_key")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Access key is required for S3/MinIO import"))?;
        let secret_key = credentials
            .get("secret_key")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Secret key is required for S3/MinIO import"))?;

        // Build endpoint
        let endpoint = format!("http://localhost:{}", port);

        // Verify connection to the imported service by attempting to list
        // buckets. Connects directly with `.await` on the current runtime —
        // spinning up a nested `tokio::runtime::Runtime` and calling
        // `block_on` here panics with "Cannot start a runtime from within a
        // runtime", since this `async fn` is already driven by one.
        let creds =
            aws_sdk_s3::config::Credentials::new(&access_key, &secret_key, None, None, "imported");
        let s3_client_config = aws_sdk_s3::config::Config::builder()
            .credentials_provider(creds)
            .endpoint_url(&endpoint)
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .force_path_style(true)
            .behavior_version_latest()
            .build();
        let client = aws_sdk_s3::Client::from_conf(s3_client_config);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.list_buckets().send(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("S3/MinIO connection timed out after 5 seconds"))?
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to S3/MinIO at {} with provided credentials: {}",
                endpoint,
                e
            )
        })?;
        info!("Successfully verified S3/MinIO connection for import");

        let network_ready = match ensure_network_exists(&self.docker).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "Failed to ensure Temps Docker network before S3/MinIO import attach: {:?}",
                    e
                );
                false
            }
        };
        if network_ready {
            let network_name = temps_core::NETWORK_NAME.as_str();
            let request = bollard::models::NetworkConnectRequest {
                container: container_id.clone(),
                ..Default::default()
            };
            match self.docker.connect_network(network_name, request).await {
                Ok(()) => info!(
                    "Attached imported S3/MinIO container '{}' to {}",
                    imported_container_name, network_name
                ),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => debug!(
                    "Imported S3/MinIO container '{}' is already attached to {}",
                    imported_container_name, network_name
                ),
                Err(e) => warn!(
                    "Failed to attach imported S3/MinIO container '{}' to {}: {}",
                    imported_container_name, network_name, e
                ),
            }
        }

        // Register as `Minio`, NOT `S3`: only the `Minio` service type routes
        // back to `S3Service` in `create_service_instance` (the `S3` type
        // routes to `RustfsService`, which manages its own RustFS container
        // and can't operate this imported MinIO container). Storing `S3` here
        // would make every later start/stop/backup reload the service as a
        // RustfsService against a synthesized `rustfs-{name}` container that
        // doesn't exist. This branch is only reachable by importing a
        // container as the `minio` type in the first place.
        #[allow(deprecated)]
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Minio,
            version: Some(version),
            parameters: serde_json::json!({
                "endpoint": endpoint,
                "port": port,
                "access_key": access_key,
                "secret_key": secret_key,
                "use_ssl": false,
                "docker_image": image,
                "server": server,
                "container_name": imported_container_name,
            }),
        };

        info!(
            "Successfully imported S3/MinIO service '{}' from container",
            config.name
        );
        Ok(config)
    }
}

/// Strip the URL scheme from an S3 endpoint so it can be embedded in an
/// `RC_HOST_<alias>=<scheme>://<key>:<secret>@<host>` env var without
/// producing a malformed double-scheme like `http://key:secret@http://host`.
///
/// Returns `(scheme, host_and_path)`.  When the endpoint carries no scheme
/// it is assumed to be a bare `host:port` and `"http"` is used.
fn mc_strip_scheme(endpoint: &str) -> (&'static str, &str) {
    if let Some(rest) = endpoint.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        ("http", rest)
    } else if endpoint.is_empty() {
        // No endpoint configured; fall back to a sensible default.
        ("http", "localhost:9000")
    } else {
        // Bare host:port — assume plain HTTP (internal/MinIO default).
        ("http", endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let service = S3Service::new("test-editable".to_string(), docker, encryption_service);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let schema_obj = schema.as_object().expect("Schema should be an object");
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Define expected editable status for each field
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("access_key", false),
            ("secret_key", false),
            ("region", false),
            ("docker_image", true),
        ];

        for (field_name, should_be_editable) in editable_status {
            let field = properties
                .get(field_name)
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{} field should exist", field_name));

            let is_editable = field
                .get("x-editable")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| panic!("{} should have x-editable property", field_name));

            assert_eq!(
                is_editable, should_be_editable,
                "Field {} editable status should be {}",
                field_name, should_be_editable
            );
        }
    }

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let service = S3Service::new("test-image".to_string(), docker, encryption_service);
        let (image_name, version) = service.get_default_docker_image();
        // MinIO no longer publishes server images; new services run RustFS.
        assert_eq!(
            format!("{image_name}:{version}"),
            crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE
        );
        assert_eq!(service.get_default_version(), version);
    }

    /// A persisted config without `docker_image` belongs to a service created
    /// before the parameter existed, i.e. a MinIO one. It must keep resolving
    /// to MinIO: starting RustFS on a MinIO data volume would not serve it.
    #[test]
    fn test_persisted_config_without_image_stays_on_minio() {
        let input: S3InputConfig = serde_json::from_value(serde_json::json!({
            "port": "9000",
            "access_key": "k",
            "secret_key": "s",
        }))
        .unwrap();
        assert_eq!(input.docker_image, LEGACY_MINIO_IMAGE);
        assert_eq!(input.server, None);
    }

    #[test]
    fn test_schema_defaults_are_rustfs() {
        assert_eq!(
            default_image(),
            crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE
        );
        assert_eq!(default_server_kind(), S3ServerKind::Rustfs);
        let schema = serde_json::to_value(schemars::schema_for!(S3InputConfig)).unwrap();
        assert_eq!(schema["properties"]["server"]["default"], "rustfs");
    }

    #[test]
    fn test_server_kind_parses_and_round_trips() {
        for (raw, kind) in [
            ("rustfs", S3ServerKind::Rustfs),
            ("minio", S3ServerKind::Minio),
            (" RustFS ", S3ServerKind::Rustfs),
        ] {
            assert_eq!(S3ServerKind::parse(raw).unwrap(), kind);
        }
        assert!(matches!(
            S3ServerKind::parse("ceph"),
            Err(S3ServerKindError::Unknown { ref value }) if value == "ceph"
        ));

        let input: S3InputConfig = serde_json::from_value(serde_json::json!({
            "docker_image": "registry.internal:5000/mirror/object-store:1.0.0",
            "server": "rustfs",
        }))
        .unwrap();
        let config = S3Config::from(input);
        assert_eq!(config.server, Some(S3ServerKind::Rustfs));
        let stored = serde_json::to_value(&config).unwrap();
        assert_eq!(stored["server"], "rustfs");

        // A blank form field counts as unset; an unknown kind is rejected.
        let blank: S3InputConfig =
            serde_json::from_value(serde_json::json!({ "server": "" })).unwrap();
        assert_eq!(blank.server, None);
        assert!(
            serde_json::from_value::<S3InputConfig>(serde_json::json!({ "server": "ceph" }))
                .is_err()
        );
    }

    fn config_for(image: &str, server: Option<S3ServerKind>) -> S3Config {
        S3Config {
            port: "9000".to_string(),
            access_key: "AKIAEXAMPLE".to_string(),
            secret_key: "secret/with+chars".to_string(),
            host: "localhost".to_string(),
            region: "us-east-1".to_string(),
            docker_image: image.to_string(),
            server,
            container_name: None,
        }
    }

    /// The explicit kind decides the settings, whatever the image is called:
    /// a private mirror may publish RustFS under a name with no "rustfs" in it,
    /// or under one that mentions "minio".
    #[test]
    fn test_explicit_rustfs_kind_gets_rustfs_env_and_default_command() {
        for image in [
            crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE,
            "registry.internal:5000/mirror/object-store:1.0.0",
            "registry.internal:5000/minio-replacement/s3:1.0.0",
        ] {
            let spec = s3_server_container_spec(
                &config_for(image, Some(S3ServerKind::Rustfs)),
                S3ServerKind::Rustfs,
            );
            assert_eq!(
                spec.env,
                vec![
                    "RUSTFS_ACCESS_KEY=AKIAEXAMPLE".to_string(),
                    "RUSTFS_SECRET_KEY=secret/with+chars".to_string(),
                ],
                "{image}"
            );
            assert_eq!(
                spec.cmd, None,
                "RustFS serves /data with its default command"
            );
            assert!(spec.healthcheck.contains("http://localhost:9000/health"));
            assert!(
                !spec.healthcheck.contains("mc "),
                "RustFS images ship no mc"
            );
        }
    }

    #[test]
    fn test_minio_kind_keeps_minio_env_and_command() {
        let spec =
            s3_server_container_spec(&config_for(LEGACY_MINIO_IMAGE, None), S3ServerKind::Minio);
        assert_eq!(
            spec.env,
            vec![
                "MINIO_ROOT_USER=AKIAEXAMPLE".to_string(),
                "MINIO_ROOT_PASSWORD=secret/with+chars".to_string(),
                "MINIO_PROMETHEUS_AUTH_TYPE=public".to_string(),
            ]
        );
        assert_eq!(
            spec.cmd,
            Some(vec!["server".to_string(), "/data".to_string()])
        );
        assert_eq!(spec.healthcheck, "mc ready local");
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    /// The metadata of the real images (`docker image inspect`) is what the
    /// detection reads — not the repository name.
    #[test]
    fn test_detect_server_kind_from_image_metadata() {
        let rustfs = detect_server_kind(
            &strings(&["/entrypoint.sh"]),
            &strings(&["rustfs"]),
            &strings(&["PATH=/usr/bin", "RUSTFS_VOLUMES=/data"]),
            &HashMap::from([("name".to_string(), "RustFS".to_string())]),
        );
        assert_eq!(rustfs, Some(S3ServerKind::Rustfs));

        let minio = detect_server_kind(
            &strings(&["/usr/bin/docker-entrypoint.sh"]),
            &strings(&["minio"]),
            &strings(&["PATH=/usr/bin", "MINIO_ROOT_USER_FILE=access_key"]),
            &HashMap::from([("name".to_string(), "MinIO".to_string())]),
        );
        assert_eq!(minio, Some(S3ServerKind::Minio));

        // One signal is enough: a rebuilt image that kept only its binary.
        assert_eq!(
            detect_server_kind(
                &strings(&["/usr/local/bin/rustfs"]),
                &[],
                &[],
                &HashMap::new()
            ),
            Some(S3ServerKind::Rustfs)
        );
        assert_eq!(
            detect_server_kind(&[], &strings(&["minio server /data"]), &[], &HashMap::new()),
            Some(S3ServerKind::Minio)
        );

        // Nothing identifying, or conflicting signals: undecided.
        assert_eq!(
            detect_server_kind(
                &strings(&["/entrypoint.sh"]),
                &strings(&["serve"]),
                &strings(&["PATH=/usr/bin"]),
                &HashMap::new()
            ),
            None
        );
        assert_eq!(
            detect_server_kind(
                &[],
                &strings(&["rustfs"]),
                &strings(&["MINIO_ROOT_USER=x"]),
                &HashMap::new()
            ),
            None
        );
    }

    #[test]
    fn test_pull_failure_wording_follows_kind_then_name() {
        assert!(image_name_mentions_minio(LEGACY_MINIO_IMAGE));
        assert!(image_name_mentions_minio(
            "registry.internal:5000/minio/minio:x"
        ));
        assert!(!image_name_mentions_minio(
            "registry.internal:5000/mirror/object-store:1.0.0"
        ));
        // A tag that mentions minio does not count.
        assert!(!image_name_mentions_minio(
            "registry.internal:5000/mirror/object-store:minio-migration"
        ));
    }

    /// Resolution against real pulled images: an explicit kind is used as is
    /// (no inspection, so even a nonexistent image resolves), and a legacy
    /// config without one is resolved from the image's metadata.
    #[tokio::test]
    async fn test_resolve_server_kind_uses_explicit_kind_then_image_metadata() {
        use bollard::query_parameters::{RemoveImageOptions, TagImageOptionsBuilder};

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker unavailable, skipping: {e}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }

        let explicit = config_for(
            "registry.internal:5000/mirror/does-not-exist:1",
            Some(S3ServerKind::Rustfs),
        );
        assert_eq!(
            S3Service::resolve_server_kind(&docker, &explicit)
                .await
                .unwrap(),
            S3ServerKind::Rustfs
        );

        let missing = config_for("registry.internal:5000/mirror/does-not-exist:1", None);
        assert!(matches!(
            S3Service::resolve_server_kind(&docker, &missing).await,
            Err(S3ServerKindError::ImageInspect { ref image, .. }) if image == &missing.docker_image
        ));

        let rustfs_image = crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE;
        if crate::utils::pull_image_with_retry(&docker, rustfs_image, None)
            .await
            .is_err()
        {
            println!("{rustfs_image} unavailable, skipping the metadata half");
            return;
        }
        // A private-mirror name that says nothing about the server, and one
        // that says the wrong thing: both resolve to RustFS from metadata.
        let suffix = rand::random::<u32>();
        let mirrors = [
            ("temps-test-mirror/object-store", format!("t{suffix}")),
            ("temps-test-mirror/minio", format!("t{suffix}")),
        ];
        for (repo, tag) in &mirrors {
            docker
                .tag_image(
                    rustfs_image,
                    Some(TagImageOptionsBuilder::new().repo(repo).tag(tag).build()),
                )
                .await
                .expect("tagging the RustFS image locally should succeed");
        }
        let mut resolved = Vec::new();
        for (repo, tag) in &mirrors {
            resolved.push(
                S3Service::resolve_server_kind(
                    &docker,
                    &config_for(&format!("{repo}:{tag}"), None),
                )
                .await,
            );
        }
        for (repo, tag) in &mirrors {
            let _ = docker
                .remove_image(&format!("{repo}:{tag}"), None::<RemoveImageOptions>, None)
                .await;
        }
        for result in resolved {
            assert_eq!(result.unwrap(), S3ServerKind::Rustfs);
        }

        // busybox names neither server: a legacy config falls back to MinIO.
        if crate::utils::pull_image_with_retry(&docker, "busybox:latest", None)
            .await
            .is_ok()
        {
            assert_eq!(
                S3Service::resolve_server_kind(&docker, &config_for("busybox:latest", None))
                    .await
                    .unwrap(),
                S3ServerKind::Minio
            );
        }
    }

    #[test]
    fn test_legacy_docker_hub_minio_spelling() {
        assert_eq!(
            legacy_docker_hub_minio_spelling("quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z")
                .as_deref(),
            Some("minio/minio:RELEASE.2025-09-07T16-13-09Z")
        );
        assert_eq!(
            legacy_docker_hub_minio_spelling("registry.internal:5000/minio/minio:latest"),
            None
        );
    }

    #[test]
    fn test_missing_minio_image_error_is_actionable() {
        let message = minio_image_unavailable_error(
            LEGACY_MINIO_IMAGE,
            "failed to pull image: 401 Unauthorized",
        )
        .to_string();
        assert!(message.contains(LEGACY_MINIO_IMAGE), "{message}");
        assert!(message.contains("401 Unauthorized"), "{message}");
        assert!(message.contains("no longer publishes"), "{message}");
        assert!(message.contains("docker_image"), "{message}");
        assert!(
            message.contains(crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE),
            "{message}"
        );
    }

    /// A MinIO image that cannot be pulled (MinIO withdrew them) is still
    /// usable when the host holds it under the old Docker Hub spelling, and
    /// fails with an actionable error when the host does not hold it at all.
    #[tokio::test]
    async fn test_ensure_server_image_handles_withdrawn_minio_images() {
        use bollard::query_parameters::{RemoveImageOptions, TagImageOptionsBuilder};

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker unavailable, skipping: {e}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }
        if crate::utils::pull_image_with_retry(&docker, "busybox:latest", None)
            .await
            .is_err()
        {
            println!("busybox:latest unavailable, skipping");
            return;
        }

        // Only the bare Docker Hub spelling exists locally (tagged from
        // busybox); the configured quay.io name has to be served from it.
        let tag = format!("temps-test-{}", rand::random::<u32>());
        docker
            .tag_image(
                "busybox:latest",
                Some(
                    TagImageOptionsBuilder::new()
                        .repo("minio/minio")
                        .tag(&tag)
                        .build(),
                ),
            )
            .await
            .expect("tagging busybox locally should succeed");
        let configured = format!("quay.io/minio/minio:{tag}");
        let adopted = S3Service::ensure_server_image(&docker, &configured, None).await;
        let tagged_locally = docker.inspect_image(&configured).await.is_ok();
        for name in [format!("minio/minio:{tag}"), configured.clone()] {
            let _ = docker
                .remove_image(&name, None::<RemoveImageOptions>, None)
                .await;
        }
        assert!(
            adopted.is_ok(),
            "local bare image must be used: {adopted:?}"
        );
        assert!(
            tagged_locally,
            "the configured name must now resolve locally"
        );

        let missing = format!(
            "quay.io/minio/minio:temps-missing-{}",
            rand::random::<u32>()
        );
        let err = S3Service::ensure_server_image(&docker, &missing, None)
            .await
            .expect_err("an unpullable MinIO image absent from the host must fail")
            .to_string();
        assert!(err.contains(&missing), "{err}");
        assert!(err.contains("no longer publishes"), "{err}");
    }

    /// The in-place restore script runs in the `rc` image: every client call
    /// must be `rc`, and folder names must be cut out of rc's full keys.
    #[test]
    fn test_restore_in_place_script_uses_rc() {
        let script = S3Service::RESTORE_IN_PLACE_SCRIPT;
        assert!(!script.contains("mc "), "script still calls mc: {script}");
        assert!(script.contains(r#"rc ls --json "${RESTORE_PREFIX}""#));
        assert!(script.contains(r#"grep -q '"truncated": *true'"#));
        assert!(script.contains(r#"if ! grep -q '"truncated": *false'"#));
        assert!(script.contains(r#"rc mb --ignore-existing "${DEST}/${bucket}""#));
        assert!(script.contains(
            r#"rc mirror --overwrite --remove "${RESTORE_PREFIX}${bucket}/" "${DEST}/${bucket}/""#
        ));
        assert!(script.contains("s|.*/||"), "must strip the listed prefix");
    }

    /// With a temporary backup credential the script sends the token with
    /// `-H` on backup-side calls only, reads it from the container env, and
    /// stages the mirror so the live-side call never carries it.
    #[test]
    fn test_restore_in_place_script_sends_the_token_only_to_the_backup_side() {
        let script = S3Service::RESTORE_IN_PLACE_SCRIPT;
        assert!(script.contains(
            r#"bkp_rc() { rc -H "x-amz-security-token: ${TEMPS_REMOTE_SESSION_TOKEN}" "$@"; }"#
        ));
        assert!(script.contains(r#"bkp_rc ls --json "${RESTORE_PREFIX}""#));
        assert!(script
            .contains(r#"bkp_rc mirror --overwrite "${RESTORE_PREFIX}${bucket}/" "${STAGE}/""#));
        assert!(
            script.contains(r#"rc mirror --overwrite --remove "${STAGE}/" "${DEST}/${bucket}/""#)
        );
        // The live side (DEST) is never reached through bkp_rc.
        for line in script.lines().filter(|l| l.contains("bkp_rc ")) {
            assert!(
                !line.contains("${DEST}"),
                "live side must not get -H: {line}"
            );
        }
        for line in script.lines().filter(|l| l.contains("-H ")) {
            assert!(line.contains("bkp_rc()"), "-H only inside bkp_rc: {line}");
        }
        assert!(script.contains(r#"trap 'rm -rf "${STAGE_ROOT}"' EXIT"#));
    }

    #[test]
    fn test_image_field_in_configuration() {
        // Test S3 configuration with an already-qualified docker_image field
        let input_config = S3InputConfig {
            port: Some("9000".to_string()),
            access_key: Some("minioadmin".to_string()),
            secret_key: Some("minioadmin".to_string()),
            host: "localhost".to_string(),
            region: "us-east-1".to_string(),
            docker_image: "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z".to_string(),
            server: None,
            container_name: None,
        };

        // Convert to runtime config
        let runtime_config: S3Config = input_config.into();

        // Verify an already-qualified docker_image is preserved as-is
        assert_eq!(
            runtime_config.docker_image,
            "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z"
        );
    }

    #[test]
    fn test_legacy_docker_hub_minio_image_is_normalized_to_quay_io() {
        // Services created before the Docker Hub -> quay.io migration have
        // this bare reference persisted in the database. It keeps being
        // rewritten on load so the containers created under the quay.io name
        // since then are not recreated (see `normalize_minio_registry`).
        for (persisted, expected) in [
            (
                "minio/minio:RELEASE.2025-09-07T16-13-09Z",
                "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z",
            ),
            ("minio/minio:latest", "quay.io/minio/minio:latest"),
            ("minio/mc:latest", "quay.io/minio/mc:latest"),
        ] {
            let input_config = S3InputConfig {
                port: Some("9000".to_string()),
                access_key: Some("minioadmin".to_string()),
                secret_key: Some("minioadmin".to_string()),
                host: "localhost".to_string(),
                region: "us-east-1".to_string(),
                docker_image: persisted.to_string(),
                server: None,
                container_name: None,
            };

            let runtime_config: S3Config = input_config.into();
            assert_eq!(runtime_config.docker_image, expected, "for {persisted}");
        }
    }

    #[test]
    fn test_custom_registry_minio_image_is_not_rewritten() {
        // An operator-configured mirror or fork must never be silently
        // redirected to quay.io -- only the exact former bare Docker Hub
        // defaults are normalized.
        for custom in [
            "registry.internal:5000/minio/minio:latest",
            "ghcr.io/acme/minio:latest",
            "minio-fork/minio:latest",
        ] {
            let input_config = S3InputConfig {
                port: Some("9000".to_string()),
                access_key: Some("minioadmin".to_string()),
                secret_key: Some("minioadmin".to_string()),
                host: "localhost".to_string(),
                region: "us-east-1".to_string(),
                docker_image: custom.to_string(),
                server: None,
                container_name: None,
            };

            let runtime_config: S3Config = input_config.into();
            assert_eq!(runtime_config.docker_image, custom);
        }
    }

    #[test]
    fn test_get_effective_address_docker_mode_uses_imported_container_name() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let service = S3Service::new("imported-svc".to_string(), docker, encryption_service);

        let config = ServiceConfig {
            name: "imported-svc".to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "9000",
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "region": "us-east-1",
                "container_name": "legacy-minio",
            }),
        };

        let (host, port) = service
            .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
            .unwrap();
        // The imported container name wins over the derived `minio-{name}`.
        assert_eq!(host, "legacy-minio");
        assert_eq!(port, S3_INTERNAL_PORT);
    }

    #[test]
    fn test_get_effective_address_host_and_docker_use_environment_specific_addresses() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let service = S3Service::new("address-mapping".to_string(), docker, encryption_service);
        let config = ServiceConfig {
            name: "address-mapping".to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "19000",
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "region": "us-east-1",
            }),
        };

        let host = service
            .get_effective_address_for_environment(
                config.clone(),
                temps_core::ExecutionEnvironment::Host,
            )
            .unwrap();
        let docker = service
            .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
            .unwrap();

        assert_eq!(host, ("localhost".to_string(), "19000".to_string()));
        assert_eq!(
            docker,
            (
                "minio-address-mapping".to_string(),
                S3_INTERNAL_PORT.to_string()
            )
        );
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`minio-{name}`), never supplied by the client — same as MariaDB
        // (see mariadb.rs's identical test). The create form is generated
        // from this schema, so the field must not appear in it.
        let schema = serde_json::to_value(schemars::schema_for!(S3InputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the S3/MinIO create schema"
        );
    }

    #[test]
    fn test_minio_version_upgrade_config() {
        // Test simulated MinIO image upgrade
        let old_config = super::ServiceConfig {
            name: "test-s3".to_string(),
            service_type: super::ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "port": Some("9000"),
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "host": "localhost",
                "region": "us-east-1",
                "image": "minio/minio:RELEASE.2025-06-01T01-00-00Z"
            }),
        };

        let new_config = super::ServiceConfig {
            name: "test-s3".to_string(),
            service_type: super::ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "port": Some("9000"),
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "host": "localhost",
                "region": "us-east-1",
                "image": "minio/minio:RELEASE.2025-09-07T16-13-09Z"
            }),
        };

        // Verify image upgrade configuration
        let old_image = old_config
            .parameters
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let new_image = new_config
            .parameters
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        assert!(
            old_image.contains("2025-06-01"),
            "Old image should contain 2025-06-01"
        );
        assert!(
            new_image.contains("2025-09-07"),
            "New image should contain 2025-09-07"
        );
        assert_ne!(old_image, new_image, "Images should be different");
    }

    #[test]
    fn test_import_service_config_creation() {
        let config = ServiceConfig {
            name: "test-s3-import".to_string(),
            service_type: ServiceType::S3,
            version: Some("latest".to_string()),
            parameters: serde_json::json!({
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "endpoint_url": "http://localhost:9000",
                "region": "us-east-1",
                "use_ssl": false,
                "docker_image": "minio/minio:latest",
                "container_id": "ghi789jkl012",
            }),
        };

        assert_eq!(config.name, "test-s3-import");
        assert_eq!(config.service_type, ServiceType::S3);
        assert_eq!(config.version, Some("latest".to_string()));
    }

    #[test]
    fn test_import_s3_version_extraction() {
        let test_cases = vec![
            ("minio/minio:latest", "latest"),
            (
                "minio/minio:RELEASE.2025-01-01T00-00-00Z",
                "RELEASE.2025-01-01T00-00-00Z",
            ),
            ("minio/minio:2024", "2024"),
        ];

        for (image, expected_version) in test_cases {
            let version = if let Some(tag_pos) = image.rfind(':') {
                image[tag_pos + 1..].to_string()
            } else {
                "latest".to_string()
            };

            assert_eq!(version, expected_version, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_import_validates_required_credentials() {
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // S3 requires access_key and secret_key

        assert!(!credentials.contains_key("access_key"));
        assert!(!credentials.contains_key("secret_key"));
    }

    #[test]
    fn test_import_credential_extraction() {
        let mut credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        credentials.insert("access_key".to_string(), "AKIAIOSFODNN7EXAMPLE".to_string());
        credentials.insert(
            "secret_key".to_string(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        );
        credentials.insert(
            "endpoint_url".to_string(),
            "http://localhost:9000".to_string(),
        );

        assert_eq!(
            credentials.get("access_key").map(|s| s.as_str()),
            Some("AKIAIOSFODNN7EXAMPLE")
        );
        assert_eq!(
            credentials.get("secret_key").map(|s| s.as_str()),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );
        assert_eq!(
            credentials.get("endpoint_url").map(|s| s.as_str()),
            Some("http://localhost:9000")
        );
    }

    #[test]
    fn test_import_s3_endpoint_validation() {
        let endpoints = vec![
            "http://localhost:9000",
            "https://s3.amazonaws.com",
            "http://minio:9000",
        ];

        for endpoint in endpoints {
            assert!(
                endpoint.contains("://"),
                "Endpoint should have protocol: {}",
                endpoint
            );
        }
    }

    // `flavor = "multi_thread"` is required because `S3TestContainer`'s
    // `Drop` impl calls `tokio::task::block_in_place`, which panics on the
    // default current-thread runtime.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_s3_backup_and_restore_to_s3() {
        use super::super::test_utils::{
            create_mock_backup, create_mock_db, create_mock_external_service, S3TestContainer,
        };

        // Check if Docker is available
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                println!("Docker not available, skipping test: {}", e);
                return;
            }
        };

        // Verify Docker is actually responding
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping test");
            return;
        }

        // Create encryption service (required for S3Service)
        let encryption_service = match EncryptionService::new("test_encryption_key_1234567890ab") {
            Ok(svc) => Arc::new(svc),
            Err(e) => {
                println!("Failed to create encryption service: {}. Skipping test", e);
                return;
            }
        };

        // Use separate S3 instances so the backup destination is not mirrored
        // back into the source service while copying all source buckets.
        let run_id = chrono::Utc::now().timestamp_millis();
        let source_bucket_0 = format!("temps-s3-roundtrip-{run_id}-000");
        let source_minio = match S3TestContainer::start(docker.clone(), &source_bucket_0).await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("Skipping S3 backup test: TLS certificate issue");
                    println!(
                        "   Reason: {}",
                        error_msg.lines().next().unwrap_or(&error_msg)
                    );
                    println!("   Solution: Install system root certificates (required by AWS SDK even for HTTP endpoints)");
                    return;
                }
                panic!("Failed to start MinIO container: {}", e);
            }
        };

        let backup_minio =
            match S3TestContainer::start(docker.clone(), "s3-backup-destination").await {
                Ok(m) => m,
                Err(e) => {
                    let _ = source_minio.cleanup().await;
                    panic!("Failed to start backup MinIO container: {}", e);
                }
            };

        let source_service_name = format!("test-s3-backup-{run_id}");
        let restored_service_name = format!("test-s3-restore-{run_id}");

        let s3_params = serde_json::json!({
            "host": "localhost",
            "port": source_minio.port.to_string(),
            "access_key": source_minio.access_key.clone(),
            "secret_key": source_minio.secret_key.clone(),
            "region": "us-east-1",
            // The restored service is created from this image, so this also
            // covers `S3Service` running a RustFS container.
            "docker_image": crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE,
        });

        let s3_config = ServiceConfig {
            name: source_service_name.clone(),
            service_type: ServiceType::S3,
            version: Some("latest".to_string()),
            parameters: s3_params,
        };

        let s3_service = S3Service::new(
            source_service_name.clone(),
            docker.clone(),
            encryption_service.clone(),
        );

        let mut expected_objects = Vec::new();
        for bucket_index in 0..100 {
            let bucket_name = format!("temps-s3-roundtrip-{run_id}-{bucket_index:03}");
            if bucket_index > 0 {
                source_minio
                    .s3_client
                    .create_bucket()
                    .bucket(&bucket_name)
                    .send()
                    .await
                    .unwrap_or_else(|e| {
                        panic!("failed to create source bucket {bucket_name}: {e}")
                    });
            }

            for object_index in 0..2 {
                let key = format!("fixtures/object-{object_index:02}.txt");
                let body = format!(
                    "synthetic minio backup fixture run={run_id} bucket={bucket_index} object={object_index}\n"
                );
                source_minio
                    .s3_client
                    .put_object()
                    .bucket(&bucket_name)
                    .key(&key)
                    .body(aws_sdk_s3::primitives::ByteStream::from(
                        body.as_bytes().to_vec(),
                    ))
                    .send()
                    .await
                    .unwrap_or_else(|e| panic!("failed to create object {bucket_name}/{key}: {e}"));
                expected_objects.push((bucket_name.clone(), key, body));
            }
        }

        let source_buckets = source_minio
            .s3_client
            .list_buckets()
            .send()
            .await
            .expect("source MinIO should list buckets");
        assert_eq!(source_buckets.buckets().len(), 100);
        println!("Created 100 source buckets and 200 synthetic objects");

        // Create mock database connection for backup/restore operations
        let mock_db = match create_mock_db().await {
            Ok(db) => db,
            Err(e) => {
                println!("Failed to create mock database: {}. Skipping test", e);
                let _ = source_minio.cleanup().await;
                let _ = backup_minio.cleanup().await;
                return;
            }
        };

        // Create mock backup record - using encrypted credentials for the backup destination
        let encrypted_access_key = match encryption_service.encrypt_string(&backup_minio.access_key)
        {
            Ok(encrypted) => encrypted,
            Err(e) => {
                println!("Failed to encrypt access key: {}. Skipping test", e);
                let _ = source_minio.cleanup().await;
                let _ = backup_minio.cleanup().await;
                return;
            }
        };

        let encrypted_secret_key = match encryption_service.encrypt_string(&backup_minio.secret_key)
        {
            Ok(encrypted) => encrypted,
            Err(e) => {
                println!("Failed to encrypt secret key: {}. Skipping test", e);
                let _ = source_minio.cleanup().await;
                let _ = backup_minio.cleanup().await;
                return;
            }
        };

        // Create s3_source for backup destination with encrypted credentials
        let backup_s3_source = temps_entities::s3_sources::Model {
            id: 2,
            name: "backup-destination".to_string(),
            bucket_name: backup_minio.bucket_name.clone(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://localhost:{}", backup_minio.port)),
            bucket_path: "".to_string(),
            access_key_id: encrypted_access_key,
            secret_key: encrypted_secret_key,
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            backing_service_id: None,
        };

        let backup_s3_source_plaintext = temps_entities::s3_sources::Model {
            access_key_id: backup_minio.access_key.clone(),
            secret_key: backup_minio.secret_key.clone(),
            ..backup_s3_source.clone()
        };

        let subpath_root = format!("external_services/s3/{source_service_name}");
        let backup = create_mock_backup(&subpath_root);
        let external_service =
            create_mock_external_service(source_service_name.clone(), "s3", "latest");

        let s3_creds = backup_minio.s3_credentials();
        let outcome = s3_service
            .backup_to_s3(
                &backup_minio.s3_client,
                &s3_creds,
                backup.clone(),
                &backup_s3_source,
                &subpath_root,
                &subpath_root,
                &mock_db,
                &external_service,
                s3_config.clone(),
            )
            .await
            .expect("S3 service backup to MinIO should complete");
        assert_eq!(outcome.location, subpath_root);
        assert!(
            outcome.size_bytes.unwrap_or_default() > 0,
            "backup should report copied object bytes"
        );
        println!("Backed up source service to {}", outcome.location);

        let restore_ctx = super::super::RestoreContext {
            s3_client: &backup_minio.s3_client,
            s3_credentials: &s3_creds,
            s3_source: &backup_s3_source_plaintext,
            backup: &backup,
            backup_location: &outcome.location,
            source_service: &external_service,
            source_config: s3_config.clone(),
            pool: &mock_db,
        };

        let restore_result = s3_service
            .restore_to_new_service(
                restore_ctx,
                restored_service_name.clone(),
                serde_json::json!({}),
            )
            .await
            .expect("S3 restore-to-new-service from MinIO backup should complete");

        let restored_port = restore_result
            .parameters
            .get("port")
            .expect("restored service should report port")
            .clone();
        let restored_access_key = restore_result
            .parameters
            .get("access_key")
            .expect("restored service should report access_key")
            .clone();
        let restored_secret_key = restore_result
            .parameters
            .get("secret_key")
            .expect("restored service should report secret_key")
            .clone();

        let restored_s3_config = aws_sdk_s3::Config::builder()
            .endpoint_url(format!("http://localhost:{restored_port}"))
            .region(Region::new("us-east-1"))
            .behavior_version_latest()
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                restored_access_key,
                restored_secret_key,
                None,
                None,
                "restored-minio",
            ))
            .force_path_style(true)
            .build();
        let restored_client = aws_sdk_s3::Client::from_conf(restored_s3_config);

        let restored_buckets = restored_client
            .list_buckets()
            .send()
            .await
            .expect("restored MinIO should list buckets");
        assert_eq!(restored_buckets.buckets().len(), 100);

        for (bucket, key, expected_body) in expected_objects {
            let object = restored_client
                .get_object()
                .bucket(&bucket)
                .key(&key)
                .send()
                .await
                .unwrap_or_else(|e| panic!("missing restored object {bucket}/{key}: {e}"));
            let bytes = object
                .body
                .collect()
                .await
                .unwrap_or_else(|e| panic!("failed reading restored object {bucket}/{key}: {e}"))
                .into_bytes();
            assert_eq!(
                bytes.as_ref(),
                expected_body.as_bytes(),
                "restored object body should match for {bucket}/{key}"
            );
        }

        println!("Verified 100 restored buckets and 200 restored objects");

        // Cleanup
        let restored_service = S3Service::new(
            restored_service_name,
            docker.clone(),
            encryption_service.clone(),
        );
        let _ = restored_service.remove().await;
        let _ = s3_service.cleanup().await;
        let _ = source_minio.cleanup().await;
        let _ = backup_minio.cleanup().await;

        println!("S3 MinIO backup and restore-to-new-service test passed");
    }

    // ── mc_strip_scheme ───────────────────────────────────────────────────────

    #[test]
    fn strip_scheme_removes_http_prefix() {
        let (scheme, host) = mc_strip_scheme("http://localhost:9092");
        assert_eq!(scheme, "http");
        assert_eq!(host, "localhost:9092");
    }

    #[test]
    fn strip_scheme_removes_https_prefix() {
        let (scheme, host) = mc_strip_scheme("https://my-bucket.s3.amazonaws.com");
        assert_eq!(scheme, "https");
        assert_eq!(host, "my-bucket.s3.amazonaws.com");
    }

    #[test]
    fn strip_scheme_bare_host_port() {
        let (scheme, host) = mc_strip_scheme("192.168.1.10:9000");
        assert_eq!(scheme, "http");
        assert_eq!(host, "192.168.1.10:9000");
    }

    #[test]
    fn strip_scheme_empty_endpoint_falls_back() {
        let (scheme, host) = mc_strip_scheme("");
        assert_eq!(scheme, "http");
        assert_eq!(host, "localhost:9000");
    }

    #[test]
    fn strip_scheme_rc_host_var_format_no_double_scheme() {
        // Regression: before mc_strip_scheme, an http:// endpoint produced
        // <client>_HOST=http://key:secret@http://host — an invalid URL.
        let endpoint = "http://minio.example.com:9000";
        let (scheme, host) = mc_strip_scheme(endpoint);
        let var = format!("RC_HOST_bkp={}://key:secret@{}", scheme, host);
        assert!(!var.contains("http://http://"), "double-scheme detected");
        assert_eq!(var, "RC_HOST_bkp=http://key:secret@minio.example.com:9000");
    }

    /// Verify the CRITICAL shell-injection fix in `restore_in_place`.
    ///
    /// Before the fix `backup_location` was interpolated into the shell script
    /// via Rust `format!()` inside POSIX single-quotes:
    ///   `BACKUP_PREFIX='{prefix}/'`
    /// A value containing `'` (e.g. `"foo'; env; #"`) breaks out of the quoted
    /// context and injects arbitrary shell commands that run with host-network
    /// access and plaintext S3 credentials in the environment.
    ///
    /// After the fix `backup_location` travels only as the value of the Docker
    /// env var `RESTORE_PREFIX`. Environment variable values are never re-parsed
    /// as shell syntax, so metacharacters are handled safely.
    #[test]
    fn test_restore_in_place_injection_safety() {
        // Classic single-quote injection payload.
        let injection_payload = "foo'; env; echo INJECTED; #";

        // Simulate what restore_in_place does to produce the RESTORE_PREFIX env var.
        let bucket_name = "backup-bucket";
        let backup_prefix = format!(
            "bkp/{}/{}",
            bucket_name,
            injection_payload.trim_matches('/')
        );
        let env_var = format!("RESTORE_PREFIX={}/", backup_prefix);

        // 1. The env var MUST carry the raw, unescaped value so that rc can reach
        //    the correct path.  This is safe: Docker env var values are byte
        //    sequences passed directly to the process, never re-parsed as shell.
        assert!(
            env_var.contains(injection_payload),
            "RESTORE_PREFIX env var must carry the raw backup_location value \
             (including metacharacters); got: {:?}",
            env_var
        );

        // 2. The static script MUST NOT contain any part of the injection payload.
        //    It is a compile-time constant — there is no code path that embeds
        //    user-supplied data into it.
        let script = S3Service::RESTORE_IN_PLACE_SCRIPT;

        assert!(
            !script.contains("foo"),
            "static script must not contain any user-supplied value"
        );
        assert!(
            !script.contains("INJECTED"),
            "injection payload must not appear in the static script"
        );
        assert!(
            !script.contains("'; env;"),
            "shell-injection snippet must not appear in the static script"
        );

        // 3. The script must reference RESTORE_PREFIX via ${RESTORE_PREFIX}
        //    (shell env var expansion), not any Rust-interpolated literal.
        assert!(
            script.contains("${RESTORE_PREFIX}"),
            "script must reference RESTORE_PREFIX via shell env var expansion"
        );

        // 4. Confirm the script uses `while IFS= read -r` instead of
        //    `for bucket in $BUCKETS` to prevent word-splitting on bucket names.
        assert!(
            script.contains("while IFS= read -r bucket"),
            "script must use 'while IFS= read -r' to avoid word-splitting"
        );
        assert!(
            !script.contains("for bucket in $"),
            "script must not use bare 'for bucket in $VAR' (word-splitting risk)"
        );
    }
}
